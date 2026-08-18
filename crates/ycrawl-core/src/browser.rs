use crate::fetch::{Fetched, FetchedBody};
use anyhow::{bail, Context, Result};
use fantoccini::{ClientBuilder, Locator};
use serde_json::{json, Map, Value};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A resident geckodriver process. Sessions are opened per page and closed after,
/// but the driver itself stays up, because cold-starting it per URL dominates the cost.
///
/// Firefox rather than Chromium is a measured choice, not a preference. On 46
/// bot-walled targets a headless Firefox cleared 34 where every Chromium build
/// cleared 26, the same score a plain TLS-impersonating HTTP client achieves at a
/// thirtieth of the cost. A Chromium tier would have earned nothing.
pub struct Browser {
    driver: Child,
    port: u16,
}

impl Browser {
    /// Spawn geckodriver and wait for it to accept connections.
    pub async fn launch(port: u16) -> Result<Self> {
        let driver = Command::new("geckodriver")
            .arg("--port")
            .arg(port.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("spawning geckodriver (is it installed? `brew install geckodriver`)")?;

        let me = Self { driver, port };
        me.wait_ready(Duration::from_secs(15)).await?;
        Ok(me)
    }

    async fn wait_ready(&self, within: Duration) -> Result<()> {
        let url = format!("http://127.0.0.1:{}/status", self.port);
        let started = Instant::now();
        loop {
            if wreq::Client::new()
                .get(&url)
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                return Ok(());
            }
            if started.elapsed() > within {
                bail!("geckodriver did not become ready on port {}", self.port);
            }
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
    }

    fn capabilities(&self) -> Map<String, Value> {
        let mut caps = Map::new();
        caps.insert(
            "moz:firefoxOptions".into(),
            json!({
                "args": ["-headless", "-width=1440", "-height=900"],
                "prefs": {
                    // Quieter and faster: no telemetry, no push, no update checks.
                    "browser.tabs.remote.autostart": true,
                    "datareporting.healthreport.uploadEnabled": false,
                    "app.update.auto": false,
                    "dom.push.enabled": false,
                    "permissions.default.image": 2,
                    "intl.accept_languages": "en-US, en",
                }
            }),
        );
        caps.insert("pageLoadStrategy".into(), json!("eager"));
        caps.insert("acceptInsecureCerts".into(), json!(true));
        caps
    }

    /// Load one URL in a fresh session and return its rendered HTML.
    ///
    /// WebDriver gives no access to the HTTP status line, so browser results leave
    /// it unknown. Callers should lean on the verdict instead.
    pub async fn fetch(&self, url: &str, timeout: Duration, settle: Duration) -> Result<Fetched> {
        let started = Instant::now();
        let deadline = started + timeout;
        let endpoint = format!("http://127.0.0.1:{}", self.port);

        // rustls rather than native-tls on purpose: native-tls links OpenSSL, and
        // wreq already links BoringSSL for TLS fingerprinting. Both in one binary
        // collide at link time on Linux with undefined SSL_* symbols.
        let mut builder =
            ClientBuilder::rustls().context("initialising the WebDriver TLS backend")?;
        builder.capabilities(self.capabilities());
        let connect = builder.connect(&endpoint);
        let client = tokio::time::timeout(remaining(deadline)?, connect)
            .await
            .context("browser deadline expired while opening Firefox")?
            .context("opening a Firefox session")?;

        let result = self.load(&client, url, deadline, settle).await;
        // Always close the session, even if the load failed, or Firefox windows leak.
        let _ = tokio::time::timeout(Duration::from_secs(1), client.close()).await;

        let (html, final_url) = result?;
        let source_bytes = html.len();
        Ok(Fetched {
            status: None,
            final_url,
            content_type: Some("text/html; rendered=true".into()),
            source_bytes,
            body: FetchedBody::Html(html),
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }

    async fn load(
        &self,
        client: &fantoccini::Client,
        url: &str,
        deadline: Instant,
        settle: Duration,
    ) -> Result<(String, String)> {
        // A timeout is not automatically fatal: the page may have rendered enough to
        // be useful before the last asset stalled, so take whatever is there. Every
        // step below is allowed to run out of budget; only the one read that
        // produces the HTML has to succeed, and it gets a grace period so that an
        // exhausted deadline still returns a page rather than an error.
        if let Ok(within) = remaining(deadline) {
            if let Ok(r) = tokio::time::timeout(within, client.goto(url)).await {
                r.context("navigating")?;
            }
        }

        // Give client-side rendering a moment to populate the DOM.
        let _ = sleep_within(deadline, settle).await;
        if let Ok(within) = remaining(deadline) {
            let _ = tokio::time::timeout(within, client.find(Locator::Css("body"))).await;
        }

        // A Cloudflare challenge needs several seconds of JavaScript before the real
        // page replaces it. Reading the DOM once, immediately, returns the
        // interstitial, so poll until the wall clears or the budget runs out.
        let read_budget = remaining(deadline)
            .unwrap_or(SOURCE_READ_GRACE)
            .max(SOURCE_READ_GRACE);
        let mut html = tokio::time::timeout(read_budget, client.source())
            .await
            .context("browser deadline expired while reading the page")?
            .context("reading page source")?;
        if crate::verdict::is_interstitial(&html) {
            while Instant::now() < deadline {
                if sleep_within(deadline, Duration::from_millis(750))
                    .await
                    .is_err()
                {
                    break;
                }
                let Ok(within) = remaining(deadline) else {
                    break;
                };
                match tokio::time::timeout(within, client.source()).await {
                    Ok(Ok(next)) => {
                        let cleared = !crate::verdict::is_interstitial(&next);
                        html = next;
                        if cleared {
                            // Let the page that replaced it finish rendering.
                            let _ = sleep_within(deadline, settle).await;
                            if let Ok(within) = remaining(deadline) {
                                if let Ok(Ok(next)) =
                                    tokio::time::timeout(within, client.source()).await
                                {
                                    html = next;
                                }
                            }
                            break;
                        }
                    }
                    _ => break,
                }
            }
        }
        let final_url = if let Ok(within) = remaining(deadline) {
            tokio::time::timeout(within, client.current_url())
                .await
                .ok()
                .and_then(Result::ok)
                .map(|value| value.to_string())
                .unwrap_or_else(|| url.to_string())
        } else {
            url.to_string()
        };
        Ok((html, final_url))
    }
}

/// The last read is what turns a browser session into a result, so it is given a
/// short grace period past the deadline rather than being cancelled outright.
const SOURCE_READ_GRACE: Duration = Duration::from_secs(2);

fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .context("browser deadline expired")
}

async fn sleep_within(deadline: Instant, duration: Duration) -> Result<()> {
    tokio::time::timeout(remaining(deadline)?, tokio::time::sleep(duration))
        .await
        .context("browser deadline expired")?;
    Ok(())
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.driver.kill();
        let _ = self.driver.wait();
    }
}
