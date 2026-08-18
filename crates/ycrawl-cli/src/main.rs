use anyhow::{bail, Result};
use clap::Parser;
use futures_util::stream::{self, StreamExt};
use std::sync::Arc;
use std::time::{Duration, Instant};
use ycrawl_core::{
    client, extract, extract_fetched, fetch_with_limit, Attempt, Browser, Escalation, ExtractMode,
    ExtractOptions, Page, Profile, Tier, Verdict, DEFAULT_MAX_BYTES,
};

/// Fetch web pages and turn the useful parts into clean markdown.
#[derive(Parser, Debug)]
#[command(name = "ycrawl", version, about, long_about = None)]
struct Args {
    /// One or more URLs to fetch.
    #[arg(conflicts_with = "html_file")]
    urls: Vec<String>,

    /// Convert a local HTML file instead of fetching.
    #[arg(long, value_name = "PATH")]
    html_file: Option<std::path::PathBuf>,

    /// Base URL used to resolve links in a local HTML file.
    #[arg(
        long,
        value_name = "URL",
        default_value = "https://example.invalid/",
        requires = "html_file"
    )]
    base_url: String,

    /// Emit JSON. Several URLs produce one JSON object per line.
    #[arg(long, conflicts_with = "links")]
    json: bool,

    /// Print page details without the body.
    #[arg(long, conflicts_with_all = ["links", "max_chars"])]
    summary: bool,

    /// Print only links, in the order they appear.
    #[arg(long, conflicts_with_all = ["json", "summary", "max_chars"])]
    links: bool,

    /// Shorten the body to roughly this many characters.
    #[arg(long, value_name = "N", conflicts_with_all = ["summary", "links"])]
    max_chars: Option<usize>,

    /// Leave images out of the markdown.
    #[arg(long)]
    no_images: bool,

    /// Browser identity to use for direct requests.
    #[arg(long, default_value = "chrome")]
    profile: Profile,

    /// Time limit for each direct request, in seconds.
    #[arg(long, default_value_t = 20)]
    timeout: u64,

    /// Largest response to accept, in bytes.
    #[arg(long, default_value_t = DEFAULT_MAX_BYTES)]
    max_bytes: usize,

    /// Number of pages to fetch at once.
    #[arg(long, default_value_t = 4)]
    concurrency: usize,

    /// Browser fallback: auto, never, or always.
    #[arg(long, default_value = "auto", value_parser = ["auto", "never", "always"])]
    escalate: String,

    /// Total time allowed for each browser attempt, in seconds.
    #[arg(long, default_value_t = 25)]
    browser_timeout: u64,

    /// Port used to talk to geckodriver.
    #[arg(long, default_value_t = 4444)]
    driver_port: u16,

    /// Return a failure code if any URL fails.
    #[arg(long)]
    fail_on_error: bool,
}

struct Work {
    index: usize,
    url: String,
    result: Result<Page>,
    attempts: Vec<Attempt>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    validate(&args)?;
    let opts = ExtractOptions {
        keep_images: !args.no_images,
        mode: output_mode(&args),
    };

    if let Some(path) = &args.html_file {
        let html = std::fs::read_to_string(path)?;
        let page = extract(
            &html,
            &args.base_url,
            &args.base_url,
            200,
            0,
            Tier::Http,
            &opts,
        );
        emit(&page, &args, false);
        return Ok(());
    }

    let client = Arc::new(client(args.timeout, args.profile)?);
    let opts = Arc::new(opts);
    let concurrency = args.concurrency;
    let max_bytes = args.max_bytes;

    let fetches = stream::iter(args.urls.iter().cloned().enumerate().map(|(index, url)| {
        let client = Arc::clone(&client);
        let opts = Arc::clone(&opts);
        async move {
            let normalized = normalize(&url);
            let attempt_started = Instant::now();
            let fetched = fetch_with_limit(&client, &normalized, max_bytes).await;
            match fetched {
                Ok(fetched) => {
                    let status = fetched.status;
                    let requested = url.clone();
                    let extracted = tokio::task::spawn_blocking(move || {
                        extract_fetched(fetched, &requested, Tier::Http, &opts)
                    })
                    .await
                    .map_err(anyhow::Error::from)
                    .and_then(|result| result);
                    let elapsed_ms = attempt_started.elapsed().as_millis() as u64;
                    match extracted {
                        Ok(page) => Work {
                            index,
                            url,
                            attempts: vec![attempt_for_page(&page, elapsed_ms, true)],
                            result: Ok(page),
                        },
                        Err(error) => Work {
                            index,
                            url,
                            attempts: vec![Attempt {
                                tier: Tier::Http,
                                status,
                                elapsed_ms,
                                verdict: None,
                                error: Some(format!("{error:#}")),
                                accepted: false,
                            }],
                            result: Err(error),
                        },
                    }
                }
                Err(error) => Work {
                    index,
                    url,
                    attempts: vec![Attempt {
                        tier: Tier::Http,
                        status: None,
                        elapsed_ms: attempt_started.elapsed().as_millis() as u64,
                        verdict: None,
                        error: Some(format!("{error:#}")),
                        accepted: false,
                    }],
                    result: Err(error),
                },
            }
        }
    }))
    .buffer_unordered(concurrency);

    let mut results: Vec<Work> = fetches.collect().await;
    results.sort_by_key(|work| work.index);

    let wanted: Vec<usize> = results
        .iter()
        .enumerate()
        .filter(|(_, work)| match &work.result {
            Ok(page) => needs_browser(page, &args.escalate),
            Err(_) => args.escalate != "never",
        })
        .map(|(index, _)| index)
        .collect();

    if !wanted.is_empty() {
        match Browser::launch(args.driver_port).await {
            Ok(browser) => {
                for index in wanted {
                    try_browser(&browser, &mut results[index], &args, &opts).await;
                }
            }
            Err(error) => {
                for index in wanted {
                    results[index].attempts.push(Attempt {
                        tier: Tier::Browser,
                        status: None,
                        elapsed_ms: 0,
                        verdict: None,
                        error: Some(format!("browser unavailable: {error:#}")),
                        accepted: false,
                    });
                }
                eprintln!("ycrawl: browser fallback unavailable: {error:#}");
            }
        }
    }

    let multiple = args.urls.len() > 1;
    let mut failures = 0usize;
    for (position, work) in results.into_iter().enumerate() {
        match work.result {
            Ok(mut page) => {
                page.meta.elapsed_ms = total_attempt_ms(&work.attempts);
                page.meta.attempts = work.attempts;
                if let Some(max) = args.max_chars {
                    truncate(&mut page, max);
                }
                emit(&page, &args, multiple && position > 0);
            }
            Err(error) => {
                failures += 1;
                eprintln!("ycrawl: {}: {error:#}", work.url);
                if args.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "url": work.url,
                            "error": format!("{error:#}"),
                            "elapsed_ms": total_attempt_ms(&work.attempts),
                            "attempts": work.attempts,
                        })
                    );
                }
            }
        }
    }

    if failures > 0 && (args.fail_on_error || failures == args.urls.len()) {
        bail!("{failures} of {} fetches failed", args.urls.len());
    }
    Ok(())
}

fn validate(args: &Args) -> Result<()> {
    if args.html_file.is_none() && args.urls.is_empty() {
        bail!("provide at least one URL, or use --html-file <PATH>");
    }
    if args.concurrency == 0 {
        bail!("--concurrency must be at least 1");
    }
    if args.max_bytes == 0 {
        bail!("--max-bytes must be at least 1");
    }
    Ok(())
}

async fn try_browser(browser: &Browser, work: &mut Work, args: &Args, opts: &ExtractOptions) {
    let target = normalize(&work.url);
    let attempt_started = Instant::now();
    match browser
        .fetch(
            &target,
            Duration::from_secs(args.browser_timeout),
            Duration::from_millis(1_500),
        )
        .await
    {
        Ok(fetched) => {
            let status = fetched.status;
            let extracted = extract_fetched(fetched, &work.url, Tier::Browser, opts);
            let elapsed_ms = attempt_started.elapsed().as_millis() as u64;
            match extracted {
                Ok(page) => {
                    let better = work
                        .result
                        .as_ref()
                        .map(|previous| result_better(&page, previous))
                        .unwrap_or(true);
                    work.attempts
                        .push(attempt_for_page(&page, elapsed_ms, better));
                    if better {
                        for attempt in &mut work.attempts {
                            attempt.accepted = false;
                        }
                        if let Some(last) = work.attempts.last_mut() {
                            last.accepted = true;
                        }
                        work.result = Ok(page);
                    }
                }
                Err(error) => work.attempts.push(Attempt {
                    tier: Tier::Browser,
                    status,
                    elapsed_ms,
                    verdict: None,
                    error: Some(format!("{error:#}")),
                    accepted: false,
                }),
            }
        }
        Err(error) => work.attempts.push(Attempt {
            tier: Tier::Browser,
            status: None,
            elapsed_ms: attempt_started.elapsed().as_millis() as u64,
            verdict: None,
            error: Some(format!("{error:#}")),
            accepted: false,
        }),
    }
}

fn attempt_for_page(page: &Page, elapsed_ms: u64, accepted: bool) -> Attempt {
    Attempt {
        tier: page.meta.tier,
        status: page.meta.status,
        elapsed_ms,
        verdict: Some(page.meta.verdict.explain()),
        error: None,
        accepted,
    }
}

fn total_attempt_ms(attempts: &[Attempt]) -> u64 {
    attempts.iter().map(|attempt| attempt.elapsed_ms).sum()
}

fn result_better(candidate: &Page, previous: &Page) -> bool {
    let candidate_rank = verdict_rank(&candidate.meta.verdict);
    let previous_rank = verdict_rank(&previous.meta.verdict);
    candidate_rank > previous_rank
        || (candidate_rank == previous_rank && candidate.meta.words > previous.meta.words)
}

fn verdict_rank(verdict: &Verdict) -> u8 {
    match verdict {
        Verdict::Content => 5,
        Verdict::Thin { .. } => 4,
        Verdict::JsRequired => 3,
        Verdict::Blocked { .. } => 2,
        Verdict::HttpError { .. } => 1,
    }
}

fn output_mode(args: &Args) -> ExtractMode {
    if args.summary {
        ExtractMode::Summary
    } else if args.links {
        ExtractMode::Links
    } else {
        ExtractMode::Full
    }
}

fn emit(page: &Page, args: &Args, separator: bool) {
    if args.json {
        let value = if args.summary {
            serde_json::to_value(&page.meta).unwrap_or_default()
        } else {
            serde_json::to_value(page).unwrap_or_default()
        };
        println!("{value}");
        return;
    }

    if separator {
        println!("\n---\n");
    }

    if args.links {
        for link in &page.links {
            println!("{link}");
        }
        return;
    }

    if args.summary {
        let status = page
            .meta
            .status
            .map(|value| value.to_string())
            .unwrap_or_else(|| "?".into());
        println!(
            "{}  [{}]  {} words  {}ms  {}  via {:?}",
            page.meta.final_url,
            status,
            page.meta.words,
            page.meta.elapsed_ms,
            page.meta.verdict.explain(),
            page.meta.tier,
        );
        if let Some(title) = &page.meta.title {
            println!("  title: {title}");
        }
        if let Some(hint) = escalation_hint(page) {
            println!("  {hint}");
        }
        return;
    }

    if !page.meta.verdict.is_content() {
        eprintln!(
            "ycrawl: {}: {}{}",
            page.meta.final_url,
            page.meta.verdict.explain(),
            escalation_hint(page)
                .map(|hint| format!(". {hint}"))
                .unwrap_or_default()
        );
    }
    println!("{}", page.to_frontmatter_markdown());
}

fn needs_browser(page: &Page, mode: &str) -> bool {
    match mode {
        "never" => false,
        "always" => true,
        _ => page.meta.escalation == Escalation::Worthwhile,
    }
}

fn escalation_hint(page: &Page) -> Option<String> {
    if let Some(browser) = page
        .meta
        .attempts
        .iter()
        .rev()
        .find(|attempt| attempt.tier == Tier::Browser)
    {
        return if browser.accepted {
            None
        } else if let Some(error) = &browser.error {
            Some(format!("browser fallback was tried but failed: {error}"))
        } else {
            Some("browser fallback was tried but did not improve the result".into())
        };
    }

    match page.meta.escalation {
        Escalation::Unnecessary => None,
        Escalation::Worthwhile => Some("browser fallback may recover this page".into()),
        Escalation::NotRecommended => {
            Some("browser fallback is unlikely to change this response".into())
        }
        Escalation::Futile => Some("the browser engines tested could not pass this wall".into()),
    }
}

fn truncate(page: &mut Page, max: usize) {
    if page.markdown.chars().count() <= max {
        return;
    }
    let cut: String = page.markdown.chars().take(max).collect();
    let cut = match cut.rfind("\n\n") {
        Some(index) if index > max / 2 => cut[..index].to_string(),
        _ => cut,
    };
    let remaining = page.markdown.chars().count() - cut.chars().count();
    page.markdown = format!("{cut}\n\n[… {remaining} more characters truncated]");
}

fn normalize(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn useful_browser_result_outranks_a_longer_block_page() {
        let blocked = test_page(
            Verdict::Blocked {
                wall: ycrawl_core::Wall::CloudflareChallenge,
            },
            500,
        );
        let content = test_page(Verdict::Content, 20);
        assert!(result_better(&content, &blocked));
    }

    #[test]
    fn always_mode_retries_even_content() {
        assert!(needs_browser(&test_page(Verdict::Content, 20), "always"));
    }

    #[test]
    fn incompatible_output_flags_are_rejected() {
        assert!(Args::try_parse_from(["ycrawl", "--json", "--links", "example.com"]).is_err());
        assert!(
            Args::try_parse_from(["ycrawl", "--summary", "--max-chars", "10", "example.com"])
                .is_err()
        );
    }

    fn test_page(verdict: Verdict, words: usize) -> Page {
        Page {
            meta: ycrawl_core::Meta {
                url: "https://example.com".into(),
                final_url: "https://example.com".into(),
                status: Some(200),
                title: None,
                description: None,
                byline: None,
                words,
                source_bytes: 1,
                content_type: Some("text/html".into()),
                elapsed_ms: 1,
                path: ycrawl_core::ExtractPath::Document,
                escalation: verdict.escalation(),
                verdict,
                tier: Tier::Http,
                attempts: vec![],
            },
            markdown: String::new(),
            links: vec![],
        }
    }
}
