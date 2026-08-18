use anyhow::{bail, Context, Result};
use encoding_rs::{Encoding, UTF_8};
use futures_util::StreamExt;
use std::time::{Duration, Instant};
use wreq::header::CONTENT_TYPE;
use wreq::Client;
use wreq_util::{Emulation, Platform, Profile as WreqProfile};

pub const DEFAULT_MAX_BYTES: usize = 20 * 1024 * 1024;

/// The useful forms a fetched document can take.
#[derive(Debug)]
pub enum FetchedBody {
    Html(String),
    Text(String),
    Pdf(Vec<u8>),
}

/// Raw result of a network fetch.
#[derive(Debug)]
pub struct Fetched {
    pub status: Option<u16>,
    pub final_url: String,
    pub content_type: Option<String>,
    pub source_bytes: usize,
    pub body: FetchedBody,
    pub elapsed_ms: u64,
}

/// Which browser's TLS and HTTP/2 fingerprint to present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    #[default]
    Chrome,
    Firefox,
    Safari,
    /// A market-share weighted random browser fingerprint, re-rolled per client.
    Random,
}

impl Profile {
    fn emulation(self) -> Emulation {
        let profile = match self {
            Profile::Chrome => WreqProfile::Chrome149,
            Profile::Firefox => WreqProfile::Firefox151,
            Profile::Safari => WreqProfile::Safari26_4,
            Profile::Random => return Emulation::weighted_random(),
        };
        Emulation::builder()
            .profile(profile)
            .platform(Platform::MacOS)
            .build()
    }
}

impl std::str::FromStr for Profile {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "chrome" => Ok(Profile::Chrome),
            "firefox" => Ok(Profile::Firefox),
            "safari" => Ok(Profile::Safari),
            "random" => Ok(Profile::Random),
            other => anyhow::bail!(
                "unknown profile {other:?}; expected chrome, firefox, safari or random"
            ),
        }
    }
}

/// A client presenting a browser TLS and HTTP/2 fingerprint.
pub fn client(timeout_secs: u64, profile: Profile) -> Result<Client> {
    Client::builder()
        .emulation(profile.emulation())
        .timeout(Duration::from_secs(timeout_secs))
        // wreq does not follow redirects on its own. Its ClientBuilder::redirect
        // doc comment claims "Default will follow redirects up to a maximum of
        // 10", but the default really is Policy::none(), so leaving this unset
        // returns the redirect stub instead of the page: a bare domain that
        // redirects to www, an http URL upgrading to https, or any short link.
        .redirect(wreq::redirect::Policy::limited(10))
        .build()
        .context("building HTTP client")
}

/// Fetch a document using the default 20 MiB response limit.
pub async fn fetch(client: &Client, url: &str) -> Result<Fetched> {
    fetch_with_limit(client, url, DEFAULT_MAX_BYTES).await
}

/// Fetch a document without allowing an unexpectedly large response to consume
/// unbounded memory.
pub async fn fetch_with_limit(client: &Client, url: &str, max_bytes: usize) -> Result<Fetched> {
    let started = Instant::now();
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?;
    let status = resp.status().as_u16();
    let final_url = resp.uri().to_string();
    let content_type = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    if resp
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        bail!("response is larger than the {max_bytes}-byte limit");
    }

    let capacity = resp.content_length().unwrap_or(0).min(max_bytes as u64) as usize;
    let mut bytes = Vec::with_capacity(capacity);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading response body")?;
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            bail!("response exceeded the {max_bytes}-byte limit while downloading");
        }
        bytes.extend_from_slice(&chunk);
    }

    let source_bytes = bytes.len();
    let body = decode_body(bytes, content_type.as_deref())?;
    Ok(Fetched {
        status: Some(status),
        final_url,
        content_type,
        source_bytes,
        body,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

fn decode_body(bytes: Vec<u8>, content_type: Option<&str>) -> Result<FetchedBody> {
    let mime = content_type
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();

    if mime == "application/pdf" || bytes.starts_with(b"%PDF-") {
        return Ok(FetchedBody::Pdf(bytes));
    }

    if is_binary_family(&mime) {
        bail!("unsupported content type {mime:?}");
    }

    // Decoding first, then looking for a NUL, catches the payloads worth refusing
    // without needing a list of every binary media type. Allow-listing readable
    // types is what turned an ordinary RSS feed into a hard failure.
    let text = decode_text(&bytes, content_type);
    if text.chars().take(8192).any(|c| c == '\0') {
        bail!("response is not readable text");
    }

    if is_plain_text(&mime) {
        Ok(FetchedBody::Text(text))
    } else {
        // Markup, and anything unrecognised. Sites serve real HTML under some odd
        // content types, so extraction is a better default than a refusal.
        Ok(FetchedBody::Html(text))
    }
}

/// Media types that are never worth decoding as text.
fn is_binary_family(mime: &str) -> bool {
    matches!(
        mime.split('/').next().unwrap_or_default(),
        "image" | "audio" | "video" | "font" | "model"
    )
}

/// Text that carries no markup, and so should be passed through as it stands
/// rather than run through HTML extraction.
fn is_plain_text(mime: &str) -> bool {
    if mime == "text/html" || mime == "text/xml" {
        return false;
    }
    mime.starts_with("text/") || mime == "application/json" || mime.ends_with("+json")
}

fn decode_text(bytes: &[u8], content_type: Option<&str>) -> String {
    let charset = content_type.and_then(|value| {
        value.split(';').skip(1).find_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            name.eq_ignore_ascii_case("charset")
                .then(|| value.trim().trim_matches(['\'', '"']).as_bytes())
        })
    });
    let encoding = charset
        .and_then(Encoding::for_label)
        .or_else(|| Encoding::for_bom(bytes).map(|(encoding, _)| encoding))
        .unwrap_or(UTF_8);
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_pdf_by_magic_even_without_a_header() {
        assert!(matches!(
            decode_body(b"%PDF-1.7\n".to_vec(), None).unwrap(),
            FetchedBody::Pdf(_)
        ));
    }

    #[test]
    fn respects_declared_text_encoding() {
        let body = decode_body(
            vec![0x63, 0x61, 0x66, 0xe9],
            Some("text/plain; charset=windows-1252"),
        )
        .unwrap();
        assert!(matches!(body, FetchedBody::Text(text) if text == "café"));
    }

    #[test]
    fn rejects_binary_content() {
        assert!(decode_body(vec![1, 2, 3], Some("image/png")).is_err());
        assert!(decode_body(vec![b'P', b'K', 3, 4, 0, 0], Some("application/zip")).is_err());
    }

    #[test]
    fn feeds_and_other_markup_are_read_as_html() {
        for mime in [
            "application/rss+xml",
            "application/atom+xml",
            "application/xml",
            "text/xml",
            "application/xhtml+xml",
            "application/octet-stream",
        ] {
            assert!(
                matches!(
                    decode_body(
                        b"<rss><channel><title>Feed</title></channel></rss>".to_vec(),
                        Some(mime)
                    )
                    .unwrap(),
                    FetchedBody::Html(_)
                ),
                "{mime} should be read as markup"
            );
        }
    }

    #[test]
    fn json_and_prose_are_passed_through_as_text() {
        for mime in [
            "text/plain",
            "text/markdown",
            "application/json",
            "application/ld+json",
        ] {
            assert!(
                matches!(
                    decode_body(b"just some words".to_vec(), Some(mime)).unwrap(),
                    FetchedBody::Text(_)
                ),
                "{mime} should be read as plain text"
            );
        }
    }
}
