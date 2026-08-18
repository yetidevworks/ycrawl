use crate::verdict::{Escalation, Verdict};
use serde::Serialize;

/// Everything we know about a page apart from its body.
#[derive(Debug, Clone, Serialize)]
pub struct Meta {
    pub url: String,
    pub final_url: String,
    pub status: u16,
    pub title: Option<String>,
    pub description: Option<String>,
    pub byline: Option<String>,
    pub words: usize,
    pub html_bytes: usize,
    pub elapsed_ms: u64,
    /// Which extraction path produced the body.
    pub path: ExtractPath,
    /// What we actually got: content, a shell, or a wall.
    #[serde(flatten)]
    pub verdict: Verdict,
    /// Whether a browser would plausibly do better.
    pub escalation: Escalation,
    /// Which tier actually produced this page.
    pub tier: Tier,
}

/// Which fetch tier produced the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// HTTP with a browser TLS fingerprint.
    Http,
    /// A real headless Firefox.
    Browser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtractPath {
    /// Readability found a main-content subtree.
    Article,
    /// Readability returned too little; whole-document conversion was used.
    Document,
}

#[derive(Debug, Clone, Serialize)]
pub struct Page {
    #[serde(flatten)]
    pub meta: Meta,
    pub markdown: String,
    pub links: Vec<String>,
}

impl Page {
    /// Markdown with a YAML frontmatter block, for writing to a file or stdout.
    pub fn to_frontmatter_markdown(&self) -> String {
        let esc = |s: &str| s.replace('"', "\\\"");
        let mut out = String::from("---\n");
        out.push_str(&format!("url: \"{}\"\n", esc(&self.meta.final_url)));
        if let Some(t) = &self.meta.title {
            out.push_str(&format!("title: \"{}\"\n", esc(t)));
        }
        if let Some(d) = &self.meta.description {
            out.push_str(&format!("description: \"{}\"\n", esc(d)));
        }
        if let Some(b) = &self.meta.byline {
            out.push_str(&format!("byline: \"{}\"\n", esc(b)));
        }
        out.push_str(&format!("words: {}\n", self.meta.words));
        out.push_str(&format!("verdict: {}\n", self.meta.verdict.explain()));
        out.push_str(&format!("tier: {:?}\n", self.meta.tier));
        out.push_str("---\n\n");
        out.push_str(&self.markdown);
        out
    }
}
