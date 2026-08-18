use anyhow::{bail, Result};
use clap::Parser;
use std::sync::Arc;
use std::time::Duration;
use ycrawl_core::{client, extract, fetch, Browser, Escalation, ExtractOptions, Page, Profile, Tier};

/// Fetch web pages and convert them to clean markdown. Nothing is written to disk.
#[derive(Parser, Debug)]
#[command(name = "ycrawl", version, about, long_about = None)]
struct Args {
    /// One or more URLs to fetch.
    urls: Vec<String>,

    /// Classify and convert a local HTML file instead of fetching. Useful for
    /// replaying saved responses against the classifier.
    #[arg(long, value_name = "PATH")]
    html_file: Option<std::path::PathBuf>,

    /// Base URL used to resolve links when reading with --html-file.
    #[arg(long, value_name = "URL", default_value = "https://example.invalid/")]
    base_url: String,

    /// Emit JSON instead of markdown. With several URLs this is NDJSON, one object per line.
    #[arg(long)]
    json: bool,

    /// Print only the metadata line for each page — no body. Use this to triage
    /// a set of URLs before pulling any content.
    #[arg(long)]
    summary: bool,

    /// Print only the links found on each page.
    #[arg(long)]
    links: bool,

    /// Truncate the body to roughly this many characters.
    #[arg(long, value_name = "N")]
    max_chars: Option<usize>,

    /// Drop images rather than keeping them as markdown image syntax.
    #[arg(long)]
    no_images: bool,

    /// Browser TLS fingerprint to present: chrome, firefox, safari or random.
    #[arg(long, default_value = "chrome")]
    profile: Profile,

    /// Per-request timeout in seconds.
    #[arg(long, default_value_t = 20)]
    timeout: u64,

    /// Maximum concurrent fetches.
    #[arg(long, default_value_t = 4)]
    concurrency: usize,

    /// When to fall back to a headless Firefox: auto, never, or always.
    ///
    /// "auto" escalates only where benchmarking showed a browser actually recovers
    /// pages — shells, JavaScript-only pages and Cloudflare interstitials. It does
    /// not escalate into DataDome or PerimeterX, which held against every engine
    /// tested including real Chrome.
    #[arg(long, default_value = "auto", value_parser = ["auto", "never", "always"])]
    escalate: String,

    /// Seconds to wait for a browser page load before taking whatever rendered.
    #[arg(long, default_value_t = 25)]
    browser_timeout: u64,

    /// Port for the geckodriver process.
    #[arg(long, default_value_t = 4444)]
    driver_port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    if let Some(path) = &args.html_file {
        let html = std::fs::read_to_string(path)?;
        let page = extract(
            &html,
            &args.base_url,
            &args.base_url,
            200,
            0,
            Tier::Http,
            &ExtractOptions { keep_images: !args.no_images },
        );
        emit(&page, &args, false);
        return Ok(());
    }

    if args.urls.is_empty() {
        bail!("provide at least one URL, or --html-file <PATH>");
    }
    if args.concurrency == 0 {
        bail!("--concurrency must be at least 1");
    }

    let client = Arc::new(client(args.timeout, args.profile)?);
    let opts = Arc::new(ExtractOptions {
        keep_images: !args.no_images,
    });
    let sem = Arc::new(tokio::sync::Semaphore::new(args.concurrency));
    let multiple = args.urls.len() > 1;

    let mut tasks = Vec::new();
    for url in args.urls.clone() {
        let (client, opts, sem) = (client.clone(), opts.clone(), sem.clone());
        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            let normalized = normalize(&url);
            match fetch(&client, &normalized).await {
                Ok(f) => Ok(extract(
                    &f.html,
                    &url,
                    &f.final_url,
                    f.status,
                    f.elapsed_ms,
                    Tier::Http,
                    &opts,
                )),
                Err(e) => Err((url, e)),
            }
        }));
    }

    // Phase 1: every URL over HTTP, concurrently.
    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await?);
    }

    // Phase 2: escalate only the pages a browser is measured to recover. Sessions
    // run sequentially — geckodriver serves one at a time, and by design this path
    // should be the exception rather than the rule.
    let wanted: Vec<usize> = results
        .iter()
        .enumerate()
        .filter(|(_, r)| match r {
            Ok(p) => needs_browser(p, &args.escalate),
            Err(_) => args.escalate != "never",
        })
        .map(|(i, _)| i)
        .collect();

    if !wanted.is_empty() {
        match Browser::launch(args.driver_port).await {
            Ok(browser) => {
                for i in wanted {
                    let url = match &results[i] {
                        Ok(p) => p.meta.url.clone(),
                        Err((u, _)) => u.clone(),
                    };
                    let target = normalize(&url);
                    match browser
                        .fetch(
                            &target,
                            Duration::from_secs(args.browser_timeout),
                            Duration::from_millis(1500),
                        )
                        .await
                    {
                        Ok(f) => {
                            let page = extract(
                                &f.html,
                                &url,
                                &f.final_url,
                                f.status,
                                f.elapsed_ms,
                                Tier::Browser,
                                &opts,
                            );
                            // Keep the browser result only if it actually improved on
                            // the HTTP one; a wall reached twice is not progress.
                            let better = match &results[i] {
                                Ok(prev) => page.meta.words > prev.meta.words,
                                Err(_) => true,
                            };
                            if better {
                                results[i] = Ok(page);
                            }
                        }
                        Err(e) => eprintln!("ycrawl: {url}: browser tier failed: {e:#}"),
                    }
                }
            }
            Err(e) => eprintln!("ycrawl: browser tier unavailable: {e:#}"),
        }
    }

    let mut failures = 0usize;
    for (i, r) in results.into_iter().enumerate() {
        match r {
            Ok(mut page) => {
                if let Some(n) = args.max_chars {
                    truncate(&mut page, n);
                }
                emit(&page, &args, multiple && i > 0);
            }
            Err((url, e)) => {
                failures += 1;
                eprintln!("ycrawl: {url}: {e:#}");
                if args.json {
                    println!(
                        "{}",
                        serde_json::json!({ "url": url, "error": format!("{e:#}") })
                    );
                }
            }
        }
    }

    if failures > 0 && failures == args.urls.len() {
        bail!("all {failures} fetches failed");
    }
    Ok(())
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
        for l in &page.links {
            println!("{l}");
        }
        return;
    }

    if args.summary {
        println!(
            "{}  [{}]  {} words  {}ms  {}  via {:?}",
            page.meta.final_url,
            page.meta.status,
            page.meta.words,
            page.meta.elapsed_ms,
            page.meta.verdict.explain(),
            page.meta.tier,
        );
        if let Some(t) = &page.meta.title {
            println!("  title: {t}");
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
                .map(|h| format!(" — {h}"))
                .unwrap_or_default()
        );
    }
    println!("{}", page.to_frontmatter_markdown());
}

fn needs_browser(page: &Page, mode: &str) -> bool {
    match mode {
        "never" => false,
        "always" => !page.meta.verdict.is_content(),
        // auto: only where a browser measurably recovers pages
        _ => page.meta.escalation == Escalation::Worthwhile,
    }
}

/// Tell the caller what a browser would buy them, so an agent can decide rather
/// than retrying blindly.
fn escalation_hint(page: &Page) -> Option<String> {
    match page.meta.escalation {
        Escalation::Unnecessary => None,
        Escalation::Worthwhile => Some("a browser would plausibly recover this page".into()),
        Escalation::Futile => Some(
            "this wall held against every engine benchmarked, including real Chrome — \
             a browser will not help"
                .into(),
        ),
    }
}

fn truncate(page: &mut Page, max: usize) {
    if page.markdown.chars().count() <= max {
        return;
    }
    let cut: String = page.markdown.chars().take(max).collect();
    let cut = match cut.rfind("\n\n") {
        Some(i) if i > max / 2 => cut[..i].to_string(),
        _ => cut,
    };
    let remaining = page.markdown.chars().count() - cut.chars().count();
    page.markdown = format!("{cut}\n\n[… {remaining} more characters truncated]");
}

/// Accept bare hostnames the way a browser address bar does.
fn normalize(url: &str) -> String {
    let t = url.trim();
    if t.starts_with("http://") || t.starts_with("https://") {
        t.to_string()
    } else {
        format!("https://{t}")
    }
}
