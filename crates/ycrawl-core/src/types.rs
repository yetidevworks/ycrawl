use crate::verdict::{Escalation, Verdict};
use serde::Serialize;

/// One network attempt made while producing a result.
#[derive(Debug, Clone, Serialize)]
pub struct Attempt {
    pub tier: Tier,
    pub status: Option<u16>,
    pub elapsed_ms: u64,
    pub verdict: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub accepted: bool,
}

/// Everything we know about a page apart from its body.
#[derive(Debug, Clone, Serialize)]
pub struct Meta {
    pub url: String,
    pub final_url: String,
    /// HTTP status, when the fetch method exposes it. WebDriver does not.
    pub status: Option<u16>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub byline: Option<String>,
    pub words: usize,
    pub source_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
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
    /// Every fetch that was tried, including results that were rejected.
    pub attempts: Vec<Attempt>,
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
    /// A plain-text response needed no HTML conversion.
    Text,
    /// Text was extracted directly from a PDF.
    Pdf,
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
        #[derive(Serialize)]
        struct Frontmatter<'a> {
            url: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            title: &'a Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            description: &'a Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            byline: &'a Option<String>,
            words: usize,
            verdict: String,
            tier: Tier,
        }

        let yaml = serde_yaml::to_string(&Frontmatter {
            url: &self.meta.final_url,
            title: &self.meta.title,
            description: &self.meta.description,
            byline: &self.meta.byline,
            words: self.meta.words,
            verdict: self.meta.verdict.explain(),
            tier: self.meta.tier,
        })
        .expect("serializing frontmatter cannot fail");
        let mut out = String::from("---\n");
        out.push_str(&yaml);
        out.push_str("---\n\n");
        out.push_str(&self.markdown);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdict::Escalation;

    #[test]
    fn frontmatter_is_valid_yaml_for_windows_paths_and_colons() {
        let page = Page {
            meta: Meta {
                url: "https://example.com".into(),
                final_url: "https://example.com/a:b".into(),
                status: Some(200),
                title: Some(r#"C:\docs: \"guide\""#.into()),
                description: None,
                byline: None,
                words: 1,
                source_bytes: 1,
                content_type: None,
                elapsed_ms: 1,
                path: ExtractPath::Document,
                verdict: Verdict::Content,
                escalation: Escalation::Unnecessary,
                tier: Tier::Http,
                attempts: vec![],
            },
            markdown: "body".into(),
            links: vec![],
        };
        let output = page.to_frontmatter_markdown();
        let yaml = output.split("---\n").nth(1).unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(value["title"], r#"C:\docs: \"guide\""#);
    }
}
