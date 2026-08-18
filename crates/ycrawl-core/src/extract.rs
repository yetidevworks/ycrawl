use crate::clean::{preclean, tidy_markdown};
use crate::fetch::{Fetched, FetchedBody};
use crate::types::{ExtractPath, Meta, Page, Tier};
use crate::verdict::{self, Verdict};
use anyhow::{Context, Result};
use dom_query::Document;
use dom_smoothie::{Config, Readability};
use htmd::HtmlToMarkdown;
use std::collections::HashSet;
use url::Url;

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

/// What the caller intends to do with the result.
///
/// This decides what is worth collecting, never how the page is read: every mode
/// runs the same extraction so that the metadata a summary reports is the
/// metadata the full fetch would have produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExtractMode {
    #[default]
    Full,
    /// Metadata only. The body is still extracted, because the word count and the
    /// verdict are derived from it, but the links are not collected.
    Summary,
    Links,
}

#[derive(Debug, Clone)]
pub struct ExtractOptions {
    pub keep_images: bool,
    pub mode: ExtractMode,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            keep_images: true,
            mode: ExtractMode::Full,
        }
    }
}

/// Extract a fetched HTML, text, or PDF document.
pub fn extract_fetched(
    fetched: Fetched,
    requested_url: &str,
    tier: Tier,
    opts: &ExtractOptions,
) -> Result<Page> {
    let Fetched {
        status,
        final_url,
        content_type,
        source_bytes,
        body,
        elapsed_ms,
    } = fetched;

    match body {
        FetchedBody::Html(html) => Ok(extract_html(
            &html,
            requested_url,
            &final_url,
            status,
            elapsed_ms,
            tier,
            source_bytes,
            content_type,
            opts,
        )),
        FetchedBody::Text(text) => Ok(extract_text(
            text,
            requested_url,
            final_url,
            status,
            elapsed_ms,
            tier,
            source_bytes,
            content_type,
        )),
        FetchedBody::Pdf(bytes) => extract_pdf(
            &bytes,
            requested_url,
            final_url,
            status,
            elapsed_ms,
            tier,
            source_bytes,
            content_type,
        ),
    }
}

/// Turn HTML into markdown. Kept as a small public convenience for callers that
/// already have the source in memory.
pub fn extract(
    html: &str,
    requested_url: &str,
    final_url: &str,
    status: u16,
    elapsed_ms: u64,
    tier: Tier,
    opts: &ExtractOptions,
) -> Page {
    extract_html(
        html,
        requested_url,
        final_url,
        Some(status),
        elapsed_ms,
        tier,
        html.len(),
        Some("text/html".into()),
        opts,
    )
}

#[allow(clippy::too_many_arguments)]
fn extract_html(
    html: &str,
    requested_url: &str,
    final_url: &str,
    status: Option<u16>,
    elapsed_ms: u64,
    tier: Tier,
    source_bytes: usize,
    content_type: Option<String>,
    opts: &ExtractOptions,
) -> Page {
    let cleaned = preclean(html, final_url);
    let description = meta_description(&cleaned);
    let title = document_title(&cleaned);

    // Every mode runs the same extraction. A summary that picked a different path
    // or counted words differently would not be describing the page the caller is
    // about to ask for, which is the only thing a summary is for.
    let source_rows = count_rows(&cleaned);
    let (title, byline, body_html, path) = match readable(&cleaned, final_url) {
        Some((readable_title, byline, content))
            if content.trim().len() >= MIN_ARTICLE_CHARS
                && !discards_listing(source_rows, count_rows(&content)) =>
        {
            (Some(readable_title), byline, content, ExtractPath::Article)
        }
        // Readability came back empty or near-empty. That is the listing-page
        // failure mode, not a short page. Fall back to the whole document.
        _ => (title, None, body_of(&cleaned), ExtractPath::Document),
    };

    let markdown = markdown_converter(opts)
        .convert(&body_html)
        .map(|md| tidy_markdown(&md))
        .unwrap_or_default();
    let words = markdown.split_whitespace().count();
    let links = if opts.mode == ExtractMode::Summary {
        Vec::new()
    } else {
        collect_links(&body_html, final_url)
    };

    // Classification runs against the raw HTML, not the cleaned copy: several wall
    // signatures live in script and iframe sources that cleaning strips out.
    let verdict = verdict::classify(html, status.unwrap_or(200), words);
    make_page(
        requested_url,
        final_url,
        status,
        title,
        description,
        byline,
        words,
        source_bytes,
        content_type,
        elapsed_ms,
        path,
        verdict,
        tier,
        markdown,
        links,
    )
}

#[allow(clippy::too_many_arguments)]
fn extract_text(
    text: String,
    requested_url: &str,
    final_url: String,
    status: Option<u16>,
    elapsed_ms: u64,
    tier: Tier,
    source_bytes: usize,
    content_type: Option<String>,
) -> Page {
    let words = text.split_whitespace().count();
    let verdict = verdict::classify(&text, status.unwrap_or(200), words);
    make_page(
        requested_url,
        &final_url,
        status,
        title_from_url(&final_url),
        None,
        None,
        words,
        source_bytes,
        content_type,
        elapsed_ms,
        ExtractPath::Text,
        verdict,
        tier,
        text.trim().to_string(),
        vec![],
    )
}

#[allow(clippy::too_many_arguments)]
fn extract_pdf(
    bytes: &[u8],
    requested_url: &str,
    final_url: String,
    status: Option<u16>,
    elapsed_ms: u64,
    tier: Tier,
    source_bytes: usize,
    content_type: Option<String>,
) -> Result<Page> {
    let pages =
        pdf_extract::extract_text_from_mem_by_pages(bytes).context("extracting text from PDF")?;
    let plain = pages
        .iter()
        .map(|page| page.trim())
        .filter(|page| !page.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let words = plain.split_whitespace().count();
    let verdict = verdict::classify(&plain, status.unwrap_or(200), words);
    let markdown = pages
        .iter()
        .enumerate()
        .filter_map(|(index, page)| {
            let page = page.trim();
            (!page.is_empty()).then(|| format!("## Page {}\n\n{page}", index + 1))
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    Ok(make_page(
        requested_url,
        &final_url,
        status,
        title_from_url(&final_url),
        None,
        None,
        words,
        source_bytes,
        content_type.or_else(|| Some("application/pdf".into())),
        elapsed_ms,
        ExtractPath::Pdf,
        verdict,
        tier,
        markdown,
        vec![],
    ))
}

#[allow(clippy::too_many_arguments)]
fn make_page(
    requested_url: &str,
    final_url: &str,
    status: Option<u16>,
    title: Option<String>,
    description: Option<String>,
    byline: Option<String>,
    words: usize,
    source_bytes: usize,
    content_type: Option<String>,
    elapsed_ms: u64,
    path: ExtractPath,
    verdict: Verdict,
    tier: Tier,
    markdown: String,
    links: Vec<String>,
) -> Page {
    Page {
        meta: Meta {
            url: requested_url.to_string(),
            final_url: final_url.to_string(),
            status,
            title: title.filter(|value| !value.trim().is_empty()),
            description,
            byline,
            words,
            source_bytes,
            content_type,
            elapsed_ms,
            path,
            escalation: verdict.escalation(),
            verdict,
            tier,
            attempts: vec![],
        },
        markdown,
        links,
    }
}

/// A stand-in title for formats that carry no title of their own.
fn title_from_url(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    let last = parsed.path_segments()?.next_back()?;
    let decoded = percent_decode(last);
    // strip_suffix, not trim_end_matches: the latter strips the suffix repeatedly,
    // turning "report.pdf.pdf" into "report".
    let stem = [".pdf", ".txt", ".md", ".text"]
        .iter()
        .find_map(|suffix| decoded.strip_suffix(suffix))
        .unwrap_or(&decoded);
    let name = stem.replace(['-', '_'], " ");
    (!name.trim().is_empty()).then_some(name)
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                match u8::from_str_radix(&value[index + 1..index + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_string())
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
    let mut readability = Readability::new(html, Some(url), Some(cfg)).ok()?;
    let article = readability.parse().ok()?;
    Some((
        article.title.to_string(),
        article.byline.map(|value| value.to_string()),
        article.content.to_string(),
    ))
}

fn document_title(html: &str) -> Option<String> {
    let title = Document::from(html)
        .select("title")
        .text()
        .trim()
        .to_string();
    (!title.is_empty()).then_some(title)
}

fn meta_description(html: &str) -> Option<String> {
    let doc = Document::from(html);
    for selector in [
        "meta[name='description']",
        "meta[property='og:description']",
    ] {
        if let Some(node) = doc.select(selector).iter().next() {
            if let Some(content) = node.attr("content") {
                let content = content.trim().to_string();
                if !content.is_empty() {
                    return Some(content);
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

/// The outbound pages a document points at, in the order they appear.
///
/// Links keep their fragment in the body markdown, where it names the section
/// worth landing on. A link list is a list of pages, so the fragment only splits
/// one destination into many entries and drags every in-page anchor along with
/// it: the `Vec` page of the std docs yields 93 pages and 1412 raw hrefs.
fn collect_links(html: &str, page_url: &str) -> Vec<String> {
    let here = without_fragment(page_url);
    let doc = Document::from(html);
    let mut seen = HashSet::new();
    let mut links = Vec::new();
    for node in doc.select("a[href]").iter() {
        if let Some(href) = node.attr("href") {
            let target = without_fragment(&href);
            if target == here {
                continue;
            }
            if seen.insert(target.clone()) {
                links.push(target);
            }
        }
    }
    links
}

/// Precleaning has already made every surviving href absolute, so this normally
/// parses; an href that does not is passed through untouched rather than dropped.
fn without_fragment(url: &str) -> String {
    match Url::parse(url) {
        Ok(mut parsed) => {
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::FetchedBody;

    fn options(mode: ExtractMode) -> ExtractOptions {
        ExtractOptions {
            keep_images: true,
            mode,
        }
    }

    fn page_at(html: &str, url: &str, mode: ExtractMode) -> Page {
        extract(html, url, url, 200, 1, Tier::Http, &options(mode))
    }

    /// A summary exists to tell the caller what the full fetch will cost them, so
    /// it has to describe the same extraction, not a cheaper one.
    #[test]
    fn a_summary_describes_the_body_the_full_fetch_would_return() {
        let nav: String = (1..40)
            .map(|i| format!(r#"<li><a href="/n{i}">Navigation entry number {i}</a></li>"#))
            .collect();
        let html = format!(
            "<html><head><title>Doc</title></head><body><nav><ul>{nav}</ul></nav>\
             <article><p>One short paragraph of actual prose.</p></article>\
             <footer><ul>{nav}</ul></footer></body></html>"
        );

        let full = page_at(&html, "https://example.com/doc", ExtractMode::Full);
        let summary = page_at(&html, "https://example.com/doc", ExtractMode::Summary);

        assert_eq!(summary.meta.words, full.meta.words);
        assert_eq!(summary.meta.verdict, full.meta.verdict);
        assert_eq!(summary.meta.path, full.meta.path);
        assert_eq!(summary.meta.title, full.meta.title);
        // The body is withheld by the caller, not by extraction.
        assert!(summary.links.is_empty());
    }

    #[test]
    fn link_mode_preserves_document_order_and_deduplicates() {
        let page = page_at(
            r#"<body><a href="/z">z</a><a href="/a">a</a><a href="/z">again</a></body>"#,
            "https://example.com",
            ExtractMode::Links,
        );
        assert_eq!(
            page.links,
            vec!["https://example.com/z", "https://example.com/a"]
        );
    }

    #[test]
    fn the_link_list_drops_in_page_anchors_and_ignores_fragments() {
        let page = page_at(
            r##"<body>
                 <a href="#intro">Intro</a>
                 <a href="#usage">Usage</a>
                 <a href="/other#method.push">push</a>
                 <a href="/other#method.pop">pop</a>
                 <a href="https://elsewhere.example/x">out</a>
               </body>"##,
            "https://example.com/guide",
            ExtractMode::Links,
        );
        assert_eq!(
            page.links,
            vec!["https://example.com/other", "https://elsewhere.example/x"]
        );
    }

    /// The fragment is still worth keeping where it points at a section to read.
    #[test]
    fn body_links_keep_their_fragment() {
        let page = page_at(
            r#"<body><p><a href="/other#section">deep link</a></p></body>"#,
            "https://example.com/guide",
            ExtractMode::Full,
        );
        assert!(
            page.markdown.contains("https://example.com/other#section"),
            "got: {}",
            page.markdown
        );
    }

    #[test]
    fn a_repeated_extension_is_only_stripped_once() {
        assert_eq!(
            title_from_url("https://example.com/report.pdf.pdf").as_deref(),
            Some("report.pdf")
        );
        assert_eq!(
            title_from_url("https://example.com/annual%20report.pdf").as_deref(),
            Some("annual report")
        );
    }

    #[test]
    fn extracts_text_and_page_boundaries_from_a_pdf() {
        let bytes = minimal_pdf(
            "PDF extraction works and returns clear text from a generated document with enough words to be useful today.",
        );
        let page = extract_fetched(
            Fetched {
                status: Some(200),
                final_url: "https://example.com/sample.pdf".into(),
                content_type: Some("application/pdf".into()),
                source_bytes: bytes.len(),
                body: FetchedBody::Pdf(bytes),
                elapsed_ms: 1,
            },
            "https://example.com/sample.pdf",
            Tier::Http,
            &options(ExtractMode::Full),
        )
        .unwrap();

        assert_eq!(page.meta.path, ExtractPath::Pdf);
        assert!(page.markdown.starts_with("## Page 1"));
        assert!(page.markdown.contains("PDF extraction works"));
        assert_eq!(page.meta.verdict, Verdict::Content);
    }

    fn minimal_pdf(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
        let objects = [
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string(),
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string(),
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>\nendobj\n".to_string(),
            "4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n".to_string(),
            format!(
                "5 0 obj\n<< /Length {} >>\nstream\n{stream}\nendstream\nendobj\n",
                stream.len()
            ),
        ];

        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0usize];
        for object in objects {
            offsets.push(pdf.len());
            pdf.extend_from_slice(object.as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len()).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets.iter().skip(1) {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                offsets.len()
            )
            .as_bytes(),
        );
        pdf
    }
}
