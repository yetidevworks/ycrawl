use dom_query::{Document, Selection};
use url::Url;

/// Elements that never carry page content but often dominate token count.
/// `svg` matters more than it looks: inline icon sets routinely cost thousands
/// of tokens and carry nothing an agent can use.
const DROP: &[&str] = &[
    "script",
    "style",
    "noscript",
    "svg",
    "template",
    "iframe",
    "object",
    "embed",
    "canvas",
    "link",
    "meta[http-equiv]",
];

/// Query parameters that only ever identify a campaign, never a resource.
const TRACKING_PREFIXES: &[&str] = &["utm_", "mc_", "pk_"];
const TRACKING_EXACT: &[&str] = &[
    "gclid", "fbclid", "msclkid", "igshid", "ref", "ref_src", "spm", "yclid", "_ga",
];

/// Strip noise and rewrite every link to an absolute URL.
///
/// Absolutising has to happen here rather than after markdown conversion: once a
/// link is `[text](/some/path)` the base is gone and the reference is useless to
/// anyone reading the output later.
pub fn preclean(html: &str, base_url: &str) -> String {
    let doc = Document::from(html);

    for sel in DROP {
        doc.select(sel).remove();
    }

    normalize_code_blocks(&doc);
    promote_table_headers(&doc);
    drop_furniture_links(&doc);

    if let Ok(base) = Url::parse(base_url) {
        for node in doc.select("a[href]").iter() {
            if let Some(href) = node.attr("href") {
                match absolutize(&base, &href) {
                    Some(abs) => node.set_attr("href", &abs),
                    // javascript:, mailto:, in-page anchors. Keep the text, drop the link
                    None => node.remove_attr("href"),
                }
            }
        }
        for sel in ["img[src]", "source[src]"] {
            for node in doc.select(sel).iter() {
                // lazy-loaded images keep the real URL in data-src
                let raw = node
                    .attr("data-src")
                    .or_else(|| node.attr("src"))
                    .map(|s| s.to_string());
                match raw.as_deref().and_then(|r| absolutize(&base, r)) {
                    Some(abs) => node.set_attr("src", &abs),
                    None => node.remove(),
                }
            }
        }
    }

    doc.html().to_string()
}

/// Rewrite every `<pre>` into `<pre><code class="language-X">`.
///
/// Two problems are fixed at once. Markdown converters only fence a `<pre>` that
/// contains a `<code>`, and plenty of documentation ships bare `<pre>`. The Python
/// docs among them, which is why 34 code blocks were arriving as loose prose.
/// Replacing the contents with plain text also discards the syntax-highlight
/// `<span>` soup, which is pure token cost in the output.
fn normalize_code_blocks(doc: &Document) {
    for pre in doc.select("pre").iter() {
        let lang = language_for(&pre);
        let code = pre.text().to_string();
        if code.trim().is_empty() {
            continue;
        }
        let class = lang
            .map(|l| format!(" class=\"language-{l}\""))
            .unwrap_or_default();
        pre.set_html(format!("<code{class}>{}</code>", escape_html(&code)));
    }
}

/// Anchor text that is a control rather than content.
const FURNITURE_LINKS: &[&str] = &[
    "hide",
    "flag",
    "vote",
    "upvote",
    "downvote",
    "favorite",
    "unfavorite",
    "reply",
    "parent",
    "context",
    "permalink",
    "report",
    "share",
    "save",
    "unsave",
    "edit",
    "delete",
    "print",
    "skip to content",
    "skip to main content",
    "back to top",
    "scroll to top",
];

/// Remove links that operate the site rather than describe it.
///
/// Listing pages attach a row of controls to every item (hide, flag, vote) and
/// each one costs a full absolute URL in the markdown. On the Hacker News front
/// page these accounted for most of 181 links without carrying any information a
/// reader would want.
fn drop_furniture_links(doc: &Document) {
    for a in doc.select("a").iter() {
        let text = a.text().trim().to_ascii_lowercase();
        if text.is_empty() {
            // An anchor with no text is a target marker or an icon; the link is noise
            // but anything nested inside it might not be.
            if a.select("img").length() == 0 {
                a.remove();
            }
            continue;
        }
        if FURNITURE_LINKS.contains(&text.as_str()) {
            a.remove();
        }
    }
}

/// Give headerless data tables a `<thead>` so they survive as markdown tables.
///
/// Markdown converters only emit a pipe table when the source has a `<thead>`;
/// without one, every cell is flattened into loose prose and the relationship
/// between columns is lost. Plenty of real specification and comparison tables ship
/// without a header row.
///
/// Layout tables are deliberately left alone. A table containing another table, or
/// with ragged row lengths, is being used for positioning rather than data. Hacker
/// News is the obvious example, and forcing those into a grid reads worse than the
/// prose form.
fn promote_table_headers(doc: &Document) {
    for table in doc.select("table").iter() {
        if table.select("thead").length() > 0 {
            continue;
        }
        if table.select("table").length() > 0 {
            continue; // nested tables mean layout, not data
        }
        let rows = table.select("tr");
        if rows.length() < 3 {
            continue;
        }
        let widths: Vec<usize> = rows.iter().map(|r| r.select("td, th").length()).collect();
        let first = widths[0];
        if first < 2 || widths.iter().any(|w| *w != first) {
            continue; // ragged rows are a layout artefact
        }
        // Rebuild and replace the whole `<table>`. Two parser rules force this:
        // loose rows get auto-wrapped in a `<tbody>`, so a `<thead>` inserted beside
        // them is invalid; and setting inner HTML parses the fragment outside table
        // context, where `<thead>` and `<tr>` are simply discarded.
        let mut head = String::new();
        let mut body = String::new();
        for (i, row) in rows.iter().enumerate() {
            let cells: String = row
                .select("td, th")
                .iter()
                .map(|c| {
                    let tag = if i == 0 { "th" } else { "td" };
                    format!("<{tag}>{}</{tag}>", c.inner_html())
                })
                .collect();
            if i == 0 {
                head = format!("<thead><tr>{cells}</tr></thead>");
            } else {
                body.push_str(&format!("<tr>{cells}</tr>"));
            }
        }
        table.replace_with_html(format!("<table>{head}<tbody>{body}</tbody></table>"));
    }
}

/// Find a language hint on the element, its `<code>` child, or its ancestors.
///
/// Conventions vary: `language-rust` (CommonMark/Prism), `lang-rust`,
/// `highlight-python3` (Sphinx), `highlight-source-js` (GitHub), and
/// `brush: js` (SyntaxHighlighter, which is what MDN ships).
fn language_for(pre: &Selection) -> Option<String> {
    let mut classes: Vec<String> = Vec::new();
    let mut push = |sel: &Selection| {
        if let Some(c) = sel.attr("class") {
            classes.push(c.to_string());
        }
    };
    push(pre);
    for child in pre.select("code").iter() {
        push(&child);
    }
    for anc in pre.ancestors(Some(3)).iter() {
        push(&anc);
    }

    // SyntaxHighlighter / MDN spell it `class="brush: js notranslate"`.
    for class in &classes {
        if let Some(rest) = class.to_ascii_lowercase().split("brush:").nth(1) {
            if let Some(name) = rest.split_whitespace().next() {
                let name =
                    name.trim_matches(|c: char| !c.is_alphanumeric() && c != '+' && c != '#');
                if !name.is_empty() {
                    return Some(normalize_language(name));
                }
            }
        }
    }

    for class in classes {
        for token in class.split_whitespace() {
            let lower = token.to_ascii_lowercase();
            for prefix in ["language-", "lang-", "highlight-source-", "highlight-"] {
                if let Some(rest) = lower.strip_prefix(prefix) {
                    let name = rest.trim();
                    if name.is_empty() || name == "notranslate" || name == "default" {
                        continue;
                    }
                    return Some(normalize_language(name));
                }
            }
        }
    }
    None
}

/// Fold the version-suffixed spellings documentation tools emit.
fn normalize_language(name: &str) -> String {
    match name {
        "python3" | "python2" | "py" | "pycon" => "python".into(),
        "js" | "javascript" => "javascript".into(),
        "ts" | "typescript" => "typescript".into(),
        "sh" | "shell-session" | "console" => "bash".into(),
        "rs" => "rust".into(),
        other => other.to_string(),
    }
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Resolve `href` against `base`, dropping non-navigational schemes and tracking noise.
pub fn absolutize(base: &Url, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    let lower = href.to_ascii_lowercase();
    for scheme in ["javascript:", "mailto:", "tel:", "data:", "blob:", "about:"] {
        if lower.starts_with(scheme) {
            return None;
        }
    }
    let mut url = base.join(href).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    strip_tracking(&mut url);
    url.set_fragment(None);
    Some(url.to_string())
}

fn strip_tracking(url: &mut Url) {
    let keep: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| {
            let k = k.to_ascii_lowercase();
            !TRACKING_PREFIXES.iter().any(|p| k.starts_with(p))
                && !TRACKING_EXACT.contains(&k.as_str())
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if keep.is_empty() {
        url.set_query(None);
    } else {
        let mut qs = url.query_pairs_mut();
        qs.clear();
        for (k, v) in keep {
            qs.append_pair(&k, &v);
        }
    }
}

/// Tidy the converted markdown: collapse runs of blank lines, drop link-only
/// furniture, and trim trailing whitespace.
pub fn tidy_markdown(md: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut blanks = 0usize;
    for line in md.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
            out.push(String::new());
        } else {
            blanks = 0;
            if is_furniture(trimmed.trim()) {
                continue;
            }
            out.push(trimmed.to_string());
        }
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

fn is_furniture(line: &str) -> bool {
    const SKIP: &[&str] = &[
        "skip to content",
        "skip to main content",
        "skip navigation",
        "enable javascript",
    ];
    let lower = line.to_ascii_lowercase();
    if SKIP.iter().any(|s| lower.contains(s)) {
        return true;
    }
    // an empty markdown link, e.g. `[](https://…)`, pure noise
    line.starts_with("[](")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(html: &str) -> String {
        preclean(html, "https://example.com/docs/page")
    }

    #[test]
    fn bare_pre_becomes_a_fenced_code_block() {
        // The Python docs ship `<pre>` with no `<code>`, and converters only fence
        // `<pre><code>`, so 34 code blocks were arriving as loose prose.
        let out = clean(
            r#"<div class="highlight-python3"><div class="highlight"><pre>print(1)</pre></div></div>"#,
        );
        assert!(
            out.contains(r#"<code class="language-python">"#),
            "got: {out}"
        );
    }

    #[test]
    fn brush_convention_is_understood() {
        // MDN uses SyntaxHighlighter's spelling.
        let out = clean(r#"<pre class="brush: js notranslate">let a = 1;</pre>"#);
        assert!(out.contains(r#"class="language-javascript""#), "got: {out}");
    }

    #[test]
    fn highlight_spans_are_stripped_from_code() {
        let out = clean(
            r#"<pre class="language-rust"><span class="k">fn</span> <span class="n">main</span></pre>"#,
        );
        assert!(!out.contains("<span"), "highlight markup survived: {out}");
        assert!(out.contains("fn main"));
    }

    #[test]
    fn headerless_data_table_gains_a_header() {
        let out = clean("<table><tr><td>a</td><td>1</td></tr><tr><td>b</td><td>2</td></tr><tr><td>c</td><td>3</td></tr></table>");
        assert!(out.contains("<thead>"), "got: {out}");
    }

    #[test]
    fn layout_tables_are_left_alone() {
        // A table containing a table is positioning, not data. Hacker News is the
        // canonical example, and forcing it into a grid reads worse than prose.
        let out = clean(
            "<table><tr><td><table><tr><td>x</td></tr></table></td><td>o</td></tr>\
                         <tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>",
        );
        assert!(!out.contains("<thead>"), "layout table was promoted: {out}");
    }

    #[test]
    fn ragged_tables_are_left_alone() {
        let out = clean(
            "<table><tr><td>a</td></tr><tr><td>b</td><td>c</td></tr><tr><td>d</td></tr></table>",
        );
        assert!(!out.contains("<thead>"));
    }

    #[test]
    fn furniture_links_are_dropped() {
        let out =
            clean(r#"<p><a href="/vote?id=1">vote</a> <a href="/item?id=1">A real title</a></p>"#);
        assert!(!out.contains("/vote?id=1"), "furniture link kept: {out}");
        assert!(out.contains("A real title"));
    }

    #[test]
    fn links_become_absolute_and_lose_tracking() {
        let out = clean(r#"<a href="../other?utm_source=x&keep=1">t</a>"#);
        assert!(out.contains("https://example.com/other"), "got: {out}");
        assert!(out.contains("keep=1"));
        assert!(!out.contains("utm_source"));
    }

    #[test]
    fn non_navigational_schemes_lose_their_href() {
        let out = clean(r#"<a href="javascript:void(0)">click</a>"#);
        assert!(!out.contains("javascript:"));
        assert!(out.contains("click"));
    }

    #[test]
    fn tidy_collapses_blank_runs_and_drops_empty_links() {
        let md = "a\n\n\n\n[](https://x.test)\nb\n";
        assert_eq!(tidy_markdown(md), "a\n\nb");
    }
}
