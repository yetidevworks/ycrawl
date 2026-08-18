use anyhow::{Context, Result};
use std::time::{Duration, Instant};
use wreq::Client;
use wreq_util::{Emulation, Platform, Profile as WreqProfile};

/// Raw result of a tier-1 HTTP fetch.
pub struct Fetched {
    pub status: u16,
    pub final_url: String,
    pub html: String,
    pub elapsed_ms: u64,
}

/// Which browser's TLS and HTTP/2 fingerprint to present.
///
/// This is worth exposing rather than hard-coding: benchmarking found Firefox
/// clearing markedly more bot-walled sites than Chrome at the browser layer, and
/// the same hypothesis is worth testing one layer down at the handshake.
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
///
/// This is the single highest-value thing in the fetch path: in benchmarking it
/// nearly doubled the number of bot-walled pages that came back with real content
/// versus a default HTTP client, at no measurable cost in latency.
pub fn client(timeout_secs: u64, profile: Profile) -> Result<Client> {
    Client::builder()
        .emulation(profile.emulation())
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .context("building HTTP client")
}

pub async fn fetch(client: &Client, url: &str) -> Result<Fetched> {
    let started = Instant::now();
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?;
    let status = resp.status().as_u16();
    let final_url = resp.uri().to_string();
    let html = resp.text().await.context("reading response body")?;
    Ok(Fetched {
        status,
        final_url,
        html,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}
