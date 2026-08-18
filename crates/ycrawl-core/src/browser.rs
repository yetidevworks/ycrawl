use crate::fetch::Fetched;
use anyhow::{bail, Context, Result};
use fantoccini::{ClientBuilder, Locator};
use serde_json::{json, Map, Value};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A resident geckodriver process. Sessions are opened per page and closed after,
/// but the driver itself stays up — cold-starting it per URL dominates the cost.
///
/// Firefox rather than Chromium is a measured choice, not a preference. On 46
/// bot-walled targets a headless Firefox cleared 34 where every Chromium build
/// cleared 26 — the same score a plain TLS-impersonating HTTP client achieves at a
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
                    "intl.accept_languages": "en-US, en",
                }
            }),
        );
        caps.insert("pageLoadStrategy".into(), json!("normal"));
        caps.insert("acceptInsecureCerts".into(), json!(true));
        caps
    }

    /// Load one URL in a fresh session and return its rendered HTML.
    ///
    /// WebDriver gives no access to the HTTP status line, so `status` is reported
    /// as 200 whenever a document was retrieved at all. Callers should lean on the
    /// verdict rather than the status for browser-tier results.
    pub async fn fetch(&self, url: &str, timeout: Duration, settle: Duration) -> Result<Fetched> {
        let started = Instant::now();
        let endpoint = format!("http://127.0.0.1:{}", self.port);

        let client = ClientBuilder::native()
            .capabilities(self.capabilities())
            .connect(&endpoint)
            .await
            .context("opening a Firefox session")?;

        let result = self.load(&client, url, timeout, settle).await;
        // Always close the session, even if the load failed, or Firefox windows leak.
        let _ = client.close().await;

        let (html, final_url) = result?;
        Ok(Fetched {
            status: 200,
            final_url,
            html,
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }

    async fn load(
        &self,
        client: &fantoccini::Client,
        url: &str,
        timeout: Duration,
        settle: Duration,
    ) -> Result<(String, String)> {
        // A timeout is not automatically fatal: the page may have rendered enough to
        // be useful before the last asset stalled, so take whatever is there.
        let nav = tokio::time::timeout(timeout, client.goto(url));
        if let Ok(r) = nav.await {
            r.context("navigating")?;
        }

        // Give client-side rendering a moment to populate the DOM.
        tokio::time::sleep(settle).await;
        let _ = client.find(Locator::Css("body")).await;

        // A Cloudflare challenge needs several seconds of JavaScript before the real
        // page replaces it. Reading the DOM once, immediately, returns the
        // interstitial — so poll until the wall clears or the budget runs out.
        let mut html = client.source().await.context("reading page source")?;
        if crate::verdict::is_interstitial(&html) {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(750)).await;
                match client.source().await {
                    Ok(next) => {
                        let cleared = !crate::verdict::is_interstitial(&next);
                        html = next;
                        if cleared {
                            // Let the page that replaced it finish rendering.
                            tokio::time::sleep(settle).await;
                            html = client.source().await.unwrap_or(html);
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        let final_url = client
            .current_url()
            .await
            .map(|u| u.to_string())
            .unwrap_or_else(|_| url.to_string());
        Ok((html, final_url))
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.driver.kill();
        let _ = self.driver.wait();
    }
}
