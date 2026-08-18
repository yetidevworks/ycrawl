use crate::clean::{preclean, tidy_markdown};
use crate::types::{ExtractPath, Meta, Page, Tier};
use crate::verdict::{self, Verdict};
use dom_query::Document;
use dom_smoothie::{Config, Readability};
use htmd::HtmlToMarkdown;

/// Below this many characters, readability is treated as having failed outright.
/// Benchmarking showed readability returning literally nothing on listing pages
/// (the Hacker News front page), while legitimately reducing a short chapter page
/// to under 300 characters, so the floor has to be low enough not to punish
/// genuinely short articles.
const MIN_ARTICLE_CHARS: usize = 200;

/// A page with at least this many table rows is carrying tabular data, not using
/// a table for layout decoration.
const LISTING_ROW_FLOOR: usize = 15;

/// If readability keeps a smaller share of those rows than this, it has treated a
/// data table as boilerplate and thrown it away.
const LISTING_KEEP_RATIO: f32 = 0.25;

pub struct ExtractOptions {
    pub keep_images: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self { keep_images: true }
    }
}

/// Turn fetched HTML into markdown, choosing between the readability path and a
/// whole-document conversion.
pub fn extract(
    html: &str,
    requested_url: &str,
    final_url: &str,
    status: u16,
    elapsed_ms: u64,
    tier: Tier,
    opts: &ExtractOptions,
) -> Page {
    let html_bytes = html.len();
    let cleaned = preclean(html, final_url);

    let description = meta_description(&cleaned);
    let converter = markdown_converter(opts);

    let source_rows = count_rows(&cleaned);

    let (title, byline, body_html, path) = match readable(&cleaned, final_url) {
        Some((title, byline, content))
            if content.trim().len() >= MIN_ARTICLE_CHARS
                && !discards_listing(source_rows, count_rows(&content)) =>
        {
            (Some(title), byline, content, ExtractPath::Article)
        }
        // Readability came back empty or near-empty. That is the listing-page
        // failure mode, not a short page. Fall back to the whole document.
        _ => (
            document_title(&cleaned),
            None,
            body_of(&cleaned),
            ExtractPath::Document,
        ),
    };

    let markdown = converter
        .convert(&body_html)
        .map(|md| tidy_markdown(&md))
        .unwrap_or_default();

    let links = collect_links(&body_html);
    let words = markdown.split_whitespace().count();

    // Classification runs against the raw HTML, not the cleaned copy: several wall
    // signatures live in script and iframe sources that cleaning strips out.
    let verdict: Verdict = verdict::classify(html, status, words);
    let escalation = verdict.escalation();

    Page {
        meta: Meta {
            url: requested_url.to_string(),
            final_url: final_url.to_string(),
            status,
            title: title.filter(|t| !t.trim().is_empty()),
            description,
            byline,
            words,
            html_bytes,
            elapsed_ms,
            path,
            verdict,
            escalation,
            tier,
        },
        markdown,
        links,
    }
}

/// Whether readability threw away a data table.
///
/// Readability scores by text and link density, which is the right instinct for an
/// article and the wrong one for an index. On the Hacker News front page it
/// discarded all 98 rows and returned the stories as loose prose, more tokens than
/// the raw DOM conversion, and without the structure that made them readable.
/// Page size alone cannot detect this; only the structure can.
fn discards_listing(source_rows: usize, kept_rows: usize) -> bool {
    source_rows >= LISTING_ROW_FLOOR
        && (kept_rows as f32) < (source_rows as f32) * LISTING_KEEP_RATIO
}

fn count_rows(html: &str) -> usize {
    Document::from(html).select("tr").length()
}

fn markdown_converter(opts: &ExtractOptions) -> HtmlToMarkdown {
    let mut builder = HtmlToMarkdown::builder();
    let mut skipped = vec!["script", "style", "svg", "noscript"];
    if !opts.keep_images {
        skipped.push("img");
    }
    builder = builder.skip_tags(skipped);
    builder.build()
}

fn readable(html: &str, url: &str) -> Option<(String, Option<String>, String)> {
    let cfg = Config {
        // Readability strips class attributes by default, which throws away the
        // `language-*` hint the markdown converter needs to tag a fenced code
        // block. Keeping classes costs nothing downstream: htmd ignores every class
        // it does not recognise, so none of them reach the output.
        keep_classes: true,
        ..Config::default()
    };
    let mut r = Readability::new(html, Some(url), Some(cfg)).ok()?;
    let article = r.parse().ok()?;
    Some((
        article.title.to_string(),
        article.byline.map(|b| b.to_string()),
        article.content.to_string(),
    ))
}

fn document_title(html: &str) -> Option<String> {
    let doc = Document::from(html);
    let t = doc.select("title").text().trim().to_string();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

fn meta_description(html: &str) -> Option<String> {
    let doc = Document::from(html);
    for sel in [
        "meta[name='description']",
        "meta[property='og:description']",
    ] {
        if let Some(node) = doc.select(sel).iter().next() {
            if let Some(c) = node.attr("content") {
                let c = c.trim().to_string();
                if !c.is_empty() {
                    return Some(c);
                }
            }
        }
    }
    None
}

fn body_of(html: &str) -> String {
    let doc = Document::from(html);
    let body = doc.select("body");
    if body.length() > 0 {
        body.inner_html().to_string()
    } else {
        html.to_string()
    }
}

fn collect_links(html: &str) -> Vec<String> {
    let doc = Document::from(html);
    let mut seen = std::collections::BTreeSet::new();
    for node in doc.select("a[href]").iter() {
        if let Some(h) = node.attr("href") {
            seen.insert(h.to_string());
        }
    }
    seen.into_iter().collect()
}
