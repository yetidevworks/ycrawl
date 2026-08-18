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

const MIN_ARTICLE_CHARS: usize = 200;
const LISTING_ROW_FLOOR: usize = 15;
const LISTING_KEEP_RATIO: f32 = 0.25;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExtractMode {
    #[default]
    Full,
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
            opts,
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
            opts,
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

    let (title, byline, body_html, path) = if opts.mode == ExtractMode::Full {
        let source_rows = count_rows(&cleaned);
        match readable(&cleaned, final_url) {
            Some((readable_title, byline, content))
                if content.trim().len() >= MIN_ARTICLE_CHARS
                    && !discards_listing(source_rows, count_rows(&content)) =>
            {
                (Some(readable_title), byline, content, ExtractPath::Article)
            }
            _ => (title, None, body_of(&cleaned), ExtractPath::Document),
        }
    } else {
        (title, None, body_of(&cleaned), ExtractPath::Document)
    };

    let words = Document::from(body_html.as_str())
        .text()
        .split_whitespace()
        .count();
    let markdown = if opts.mode == ExtractMode::Full {
        markdown_converter(opts)
            .convert(&body_html)
            .map(|md| tidy_markdown(&md))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let words = if opts.mode == ExtractMode::Full {
        markdown.split_whitespace().count()
    } else {
        words
    };
    let links = if opts.mode != ExtractMode::Summary {
        collect_links(&body_html)
    } else {
        Vec::new()
    };

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
    opts: &ExtractOptions,
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
        if opts.mode == ExtractMode::Full {
            text.trim().to_string()
        } else {
            String::new()
        },
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
    opts: &ExtractOptions,
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
    let markdown = if opts.mode == ExtractMode::Full {
        pages
            .iter()
            .enumerate()
            .filter_map(|(index, page)| {
                let page = page.trim();
                (!page.is_empty()).then(|| format!("## Page {}\n\n{page}", index + 1))
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    } else {
        String::new()
    };

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

fn title_from_url(url: &str) -> Option<String> {
    let name = Url::parse(url)
        .ok()?
        .path_segments()?
        .next_back()?
        .trim_end_matches(".pdf")
        .trim_end_matches(".txt")
        .replace(['-', '_'], " ");
    (!name.trim().is_empty()).then_some(name)
}

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

fn collect_links(html: &str) -> Vec<String> {
    let doc = Document::from(html);
    let mut seen = HashSet::new();
    let mut links = Vec::new();
    for node in doc.select("a[href]").iter() {
        if let Some(href) = node.attr("href") {
            let href = href.to_string();
            if seen.insert(href.clone()) {
                links.push(href);
            }
        }
    }
    links
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

    #[test]
    fn summary_skips_body_but_keeps_word_count() {
        let page = extract(
            "<html><head><title>Hello</title></head><body>one two three</body></html>",
            "https://example.com",
            "https://example.com",
            200,
            1,
            Tier::Http,
            &options(ExtractMode::Summary),
        );
        assert_eq!(page.meta.words, 3);
        assert!(page.markdown.is_empty());
        assert!(page.links.is_empty());
    }

    #[test]
    fn link_mode_preserves_document_order_and_deduplicates() {
        let page = extract(
            r#"<body><a href="/z">z</a><a href="/a">a</a><a href="/z">again</a></body>"#,
            "https://example.com",
            "https://example.com",
            200,
            1,
            Tier::Http,
            &options(ExtractMode::Links),
        );
        assert_eq!(
            page.links,
            vec!["https://example.com/z", "https://example.com/a"]
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
