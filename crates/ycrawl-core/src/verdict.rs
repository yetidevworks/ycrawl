use serde::Serialize;

/// What kind of wall a page is behind, when it is behind one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wall {
    CloudflareChallenge,
    CloudflareBlock,
    PerimeterX,
    DataDome,
    Akamai,
    LoginWall,
    BotGeneric,
    Anubis,
}

impl Wall {
    pub fn label(self) -> &'static str {
        match self {
            Wall::CloudflareChallenge => "Cloudflare challenge",
            Wall::CloudflareBlock => "Cloudflare block",
            Wall::PerimeterX => "PerimeterX",
            Wall::DataDome => "DataDome",
            Wall::Akamai => "Akamai",
            Wall::LoginWall => "login wall",
            Wall::BotGeneric => "bot detection",
            Wall::Anubis => "Anubis proof-of-work",
        }
    }
}

/// What we actually got back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum Verdict {
    /// Real page content.
    Content,
    /// The response is a shell: it parsed, but carries almost nothing.
    Thin {
        words: usize,
    },
    /// The page told us outright that it needs JavaScript.
    JsRequired,
    /// An interstitial stood in the way.
    Blocked {
        wall: Wall,
    },
    HttpError {
        status: u16,
    },
}

/// Whether escalating to a real browser is worth the four seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Escalation {
    /// Not needed — we already have the content.
    Unnecessary,
    /// Measured to recover a useful share of these.
    Worthwhile,
    /// Measured not to help. Report the wall instead of burning time on it.
    Futile,
}

impl Verdict {
    /// Escalation policy, derived from `ycrawl-bench` rather than assumed.
    ///
    /// Recovery rates when tier 1 failed and a Firefox browser was tried:
    ///
    /// | tier-1 verdict       | recovered |
    /// |----------------------|-----------|
    /// | Cloudflare challenge | 3/4 (75%) |
    /// | thin / shell         | 6/10 (60%)|
    /// | DataDome             | 0/5  (0%) |
    /// | PerimeterX           | 0/1  (0%) |
    ///
    /// DataDome and PerimeterX held against every engine tested, including real
    /// Chrome. Escalating into them spends seconds to arrive at the same wall, so
    /// the honest move is to report it.
    pub fn escalation(&self) -> Escalation {
        match self {
            Verdict::Content => Escalation::Unnecessary,
            Verdict::Thin { .. } | Verdict::JsRequired => Escalation::Worthwhile,
            Verdict::HttpError { .. } => Escalation::Worthwhile,
            Verdict::Blocked { wall } => match wall {
                Wall::DataDome | Wall::PerimeterX | Wall::Anubis => Escalation::Futile,
                _ => Escalation::Worthwhile,
            },
        }
    }

    pub fn is_content(&self) -> bool {
        matches!(self, Verdict::Content)
    }

    /// A short human-readable reason, for stderr and `--summary`.
    pub fn explain(&self) -> String {
        match self {
            Verdict::Content => "content".into(),
            Verdict::Thin { words } => format!("shell ({words} words)"),
            Verdict::JsRequired => "page requires JavaScript".into(),
            Verdict::Blocked { wall } => format!("blocked by {}", wall.label()),
            Verdict::HttpError { status } => format!("HTTP {status}"),
        }
    }
}

/// Phrases that appear *only* on an interstitial.
///
/// The distinction matters: an earlier version of this logic treated Cloudflare's
/// `challenge-platform` script as proof of a block, but Cloudflare injects that on
/// successful loads too — so pages that had been fetched perfectly were scored as
/// blocked. Vendor-presence strings never decide a verdict; only these do.
const WALLS: &[(Wall, &[&str])] = &[
    (
        Wall::CloudflareChallenge,
        &[
            "just a moment",
            "enable javascript and cookies to continue",
            "checking your browser before accessing",
            "verifying you are human. this may take",
        ],
    ),
    (
        Wall::CloudflareBlock,
        &[
            "sorry, you have been blocked",
            "attention required! | cloudflare",
            "you have been blocked from accessing",
        ],
    ),
    (
        Wall::PerimeterX,
        &[
            "press & hold to confirm you are",
            "px-captcha",
            "/px/captcha",
        ],
    ),
    (
        Wall::DataDome,
        &["captcha-delivery.com", "geo.captcha-delivery"],
    ),
    (
        Wall::Akamai,
        &[
            "errors.edgesuite.net",
            "you don't have permission to access",
        ],
    ),
    (
        Wall::BotGeneric,
        &[
            "unusual traffic from your computer",
            "to discuss automated access",
            "enter the characters you see below",
            "are you a robot",
            "verify you are human",
        ],
    ),
    (
        Wall::LoginWall,
        &[
            "sign in to continue",
            "log in to continue",
            "you must log in to continue",
        ],
    ),
    (Wall::Anubis, &["proof-of-work challenge"]),
];

/// Pages that say outright they need scripting. Observed on wsj.com and
/// leboncoin.fr, which both return a 767-byte shell reading
/// "Please enable JS and disable any ad blocker".
const JS_REQUIRED: &[&str] = &[
    "please enable js",
    "please enable javascript",
    "javascript is required",
    "enable javascript to continue",
    "you need to enable javascript to run this app",
];

/// Below this, a page is a shell rather than a short article. example.com is a
/// legitimate 17 words; the observed block shells were 0-8.
const MIN_CONTENT_WORDS: usize = 15;

/// Above this, the page served us real body text and is treated as content no
/// matter what challenge markup it also carries.
///
/// This exists because wall signatures are not as exclusive as they look.
/// seekingalpha.com serves 1,305 words of market data while carrying PerimeterX's
/// `px-captcha` markup three times over, and udemy.com renders its full homepage
/// with the string "Just a moment" sitting in the source. Both were being reported
/// as blocked. An interstitial is short by nature — the Cloudflare one runs to
/// roughly 30 words — so a page well past that floor has plainly been served.
const SERVED_CONTENT_WORDS: usize = 80;

/// Whether the raw HTML is an anti-bot interstitial, ignoring content entirely.
///
/// The browser tier uses this to decide whether a page is still mid-challenge and
/// worth waiting on: a Cloudflare challenge takes several seconds of JavaScript to
/// clear, so reading the DOM immediately after navigation returns the interstitial
/// rather than the page behind it.
pub fn is_interstitial(raw_html: &str) -> bool {
    let head: String = raw_html
        .chars()
        .take(200_000)
        .collect::<String>()
        .to_ascii_lowercase();
    WALLS
        .iter()
        .any(|(_, needles)| needles.iter().any(|n| head.contains(n)))
}

/// Classify a response.
///
/// This must run against the **raw** HTML, before cleaning: several wall
/// signatures live in script and iframe sources that extraction deliberately
/// strips.
pub fn classify(raw_html: &str, status: u16, extracted_words: usize) -> Verdict {
    // Content first. A page that handed us substantial body text was not blocked,
    // whatever anti-bot markup it also happens to load.
    if extracted_words >= SERVED_CONTENT_WORDS {
        return Verdict::Content;
    }

    let head: String = raw_html
        .chars()
        .take(400_000)
        .collect::<String>()
        .to_ascii_lowercase();

    for (wall, needles) in WALLS {
        if needles.iter().any(|n| head.contains(n)) {
            return Verdict::Blocked { wall: *wall };
        }
    }

    if JS_REQUIRED.iter().any(|n| head.contains(n)) && extracted_words < 100 {
        return Verdict::JsRequired;
    }

    if status >= 400 {
        return Verdict::HttpError { status };
    }

    if extracted_words < MIN_CONTENT_WORDS {
        return Verdict::Thin {
            words: extracted_words,
        };
    }

    Verdict::Content
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloudflare_script_alone_is_not_a_block() {
        // The regression that broke the first benchmark run: this script is present
        // on successful loads and must not decide a verdict.
        let html = r#"<html><body><p>Real article text here</p>
            <script src="/cdn-cgi/challenge-platform/h/b/scripts/main.js"></script></body></html>"#;
        assert_eq!(classify(html, 200, 400), Verdict::Content);
    }

    #[test]
    fn interstitial_is_a_block() {
        let html = "<html><head><title>Just a moment...</title></head><body></body></html>";
        assert_eq!(
            classify(html, 403, 0),
            Verdict::Blocked {
                wall: Wall::CloudflareChallenge
            }
        );
    }

    #[test]
    fn js_shell_is_distinguished_from_a_wall() {
        let html = "<html><body>Please enable JS and disable any ad blocker</body></html>";
        assert_eq!(classify(html, 200, 8), Verdict::JsRequired);
        assert_eq!(classify(html, 200, 8).escalation(), Escalation::Worthwhile);
    }

    #[test]
    fn datadome_escalation_is_futile() {
        let html = r#"<iframe src="https://geo.captcha-delivery.com/captcha/"></iframe>"#;
        let v = classify(html, 403, 0);
        assert_eq!(
            v,
            Verdict::Blocked {
                wall: Wall::DataDome
            }
        );
        assert_eq!(v.escalation(), Escalation::Futile);
    }

    #[test]
    fn served_content_outranks_challenge_markup() {
        // seekingalpha.com: real market data alongside PerimeterX markup.
        let html = r#"<html><body><div class="px-captcha"></div><p>Futures Stock Indices</p></body></html>"#;
        assert_eq!(classify(html, 200, 1305), Verdict::Content);
        // udemy.com: full homepage with a stray "Just a moment" in the source.
        let html = r#"<html><head><title>Just a moment</title></head><body>homepage</body></html>"#;
        assert_eq!(classify(html, 200, 271), Verdict::Content);
    }

    #[test]
    fn short_but_real_pages_are_content() {
        // example.com is 17 words and entirely legitimate.
        assert_eq!(
            classify("<html><body>ok</body></html>", 200, 17),
            Verdict::Content
        );
    }
}
