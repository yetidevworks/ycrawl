# ycrawl

Fetch a web page, get clean markdown. Nothing is cached, indexed, or written to disk.

```bash
ycrawl https://doc.rust-lang.org/book/ch01-00-getting-started.html
```

## Why

Agent web tooling burns tokens two ways: it fetches pages that hand back navigation instead of content, and it pipes whatever it got straight into the conversation. A single documentation page can cost 15,000 tokens when the useful part was three paragraphs.

ycrawl attacks both. Extraction strips the furniture, and the output modes let an agent triage a set of URLs for about twenty tokens each before deciding what is worth reading.

It also tells you *why* a page came back empty. A bot wall and a genuinely blank page are different problems, and an agent that cannot distinguish them will retry forever or quietly report nothing.

Every design decision here is measured rather than assumed. The `ycrawl-bench/` harness in the neighbouring directory runs candidate engines against 55 live URLs chosen for how they resist automated access, and the numbers throughout this README come from it.

## Compared with the obvious alternatives

Reaching for `curl` is the reflex, but for an agent it is rarely the real default — most coding agents ship a built-in fetch tool that already converts a page to text. ycrawl is aimed at where both of those fall down.

| | `curl` / `wget` | built-in agent fetch | headless browser | ycrawl |
|---|---|---|---|---|
| Survives TLS fingerprinting | no | varies | yes | yes |
| Renders JavaScript | no | no | yes | on demand |
| What you get back | raw HTML | a model's summary | raw DOM | clean markdown |
| Stripe API reference costs | 447,849 tokens | opaque | ~100k tokens | **3,269 tokens** |
| Says *why* it failed | status code | an error | no | a verdict |
| Several URLs at once | shell scripting | one at a time | yes | yes |
| Cost of a page that cannot be fetched | retry forever | retry forever | ~5 s to the same wall | told to stop |

**Against `curl`.** Most "works in my browser, 403s in my script" failures are decided at the TLS handshake, before a single byte of HTTP is sent — so a spoofed user-agent changes nothing. Measured on the benchmark corpus, plain curl cleared 5 of 18 bot-walled pages and the identical requests with a browser fingerprint cleared 9. curl also hands back raw HTML: the Stripe API reference is roughly 448,000 tokens of markup, against 3,269 tokens of markdown from ycrawl carrying the same prose.

**Against a built-in fetch tool.** Those are genuinely good at "answer this question about this page", and they need no install. Two things they do not do: they hand back a summary rather than the document, so you cannot grep it, quote it exactly, or feed it anywhere downstream; and when a page is defended they fail opaquely. ycrawl returns the actual markdown, and distinguishes an empty page from a Cloudflare interstitial from a DataDome wall — which is the difference between an agent reporting "that site blocks automated access" and an agent silently returning nothing, or retrying forever.

**Against driving a headless browser yourself.** That works, and it is what ycrawl falls back to. But paying four seconds and 300 MB for every fetch is the wrong default when a fingerprinted HTTP request clears the same page in 180 ms — and, measured here, headless Chromium clears no more bot walls than a plain HTTP client does. The browser is worth having for the third of pages that need it, and worth skipping for the rest.

**What ycrawl is not.** It does not search — it fetches a URL you already have. It will not get you past DataDome or PerimeterX; nothing tested here does, and it says so rather than pretending.

## Features

- **Two-tier fetching** — HTTP with a real browser TLS fingerprint first, headless Firefox only where a browser measurably helps
- **Honest verdicts** — every fetch reports whether it got content, a shell, or a specific bot wall
- **Evidence-based escalation** — never burns seconds on walls that no engine has been able to pass
- **Readable markdown** — fenced code with language tags, real tables, absolute links, no nav or tracking parameters
- **Agent-first output modes** — metadata-only triage, truncation, links-only, JSON and NDJSON
- **Fingerprint choice** — Chrome, Firefox, Safari, or a market-share weighted random profile
- **Local replay** — convert a saved HTML file instead of fetching, for testing extraction changes
- **Ships a Claude Code skill** so an agent reaches for it correctly without being told how

## Installation

```bash
cargo install --path crates/ycrawl-cli
```

Browser escalation additionally needs Firefox and geckodriver:

```bash
brew install geckodriver
```

Without them tier 1 still works, and ycrawl says why it could not escalate rather than failing silently.

## Quick start

```bash
# Triage first — metadata only, no body
ycrawl --summary https://docs.stripe.com/api/charges
# https://docs.stripe.com/api/charges  [200]  1422 words  1295ms  content  via Http
#   title: Charges | Stripe API Reference

# Then pull the body once you know it is worth pulling
ycrawl https://docs.stripe.com/api/charges

# Several URLs in one call — fetched concurrently, one browser reused for escalations
ycrawl --summary url1 url2 url3
```

## Usage

```
ycrawl <URL>...                    markdown with YAML frontmatter
ycrawl --summary <URL>...          one metadata line per page, no body
ycrawl --json <URL>...             JSON; NDJSON when given several URLs
ycrawl --links <URL>               outbound links only, absolute
ycrawl --max-chars 4000 <URL>      truncate the body, with a marker
ycrawl --no-images <URL>           drop image markup
ycrawl --profile safari <URL>      chrome | firefox | safari | random
ycrawl --escalate never <URL>      tier 1 only, skip the browser
ycrawl --concurrency 8 a b c       several URLs at once
ycrawl --html-file page.html       convert a local file instead of fetching
```

## Verdicts

Every fetch reports what it actually got, and whether a browser would do better. The recovery rates below are measured: each tier-1 failure in the corpus was re-run through a real Firefox to see what a browser actually buys.

| verdict | meaning | browser worth trying? |
|---|---|---|
| `content` | real page text | not needed |
| `thin` | parsed, but a shell | yes — 6/10 recovered |
| `js-required` | page says it needs scripting | yes |
| `blocked by Cloudflare challenge` | interstitial | yes — 3/4 recovered |
| `blocked by DataDome` / `PerimeterX` | commercial bot wall | **no** — 0/6 recovered |

DataDome and PerimeterX held against every engine tested, including real Chrome. ycrawl reports them instead of spending five seconds arriving at the same wall:

```
https://www.yelp.com/  [403]  8 words  107ms  blocked by DataDome  via Http
  this wall held against every engine benchmarked, including real Chrome — a browser will not help
```

Classification runs against raw HTML, before cleaning, because several wall signatures live in script and iframe sources that extraction strips out.

Two rules keep it honest, both learned from false positives that scored correctly-fetched pages as blocked:

- **Vendor-presence strings never decide a verdict.** Cloudflare injects `challenge-platform` on *successful* loads; treating it as proof of a block mis-scored four working pages.
- **Substantial body text outranks challenge markup.** seekingalpha.com serves 1,305 words while carrying PerimeterX's `px-captcha` three times over, and udemy.com renders its homepage with "Just a moment" sitting in the source.

## How it works

**Tier 1 — HTTP with a browser fingerprint.** Most "works in the browser, 403s in a script" failures are decided at the TLS handshake, before a byte of HTTP is exchanged. Presenting a real Chrome JA3/JA4 and HTTP/2 fingerprint via [`wreq`](https://crates.io/crates/wreq) took bot-walled pages from 5/18 to 9/18 with no browser involved, at a 126 ms median. It is the highest-value single measure in the whole stack.

**Tier 2 — headless Firefox.** Reached only when the verdict says a browser helps. Firefox rather than Chromium is a measured choice, and an uncomfortable one:

| engine | bot-walled pages cleared (of 46) |
|---|---|
| WebKit | 35 |
| **Firefox** | **34** |
| Chromium (headless) | 26 |
| Chrome (real, headless) | 26 |
| HTTP + TLS fingerprint | 26 |

Headless Chromium ties a plain HTTP client at a thirtieth of the speed. A Chromium tier would have earned nothing. Firefox is the pick over WebKit because geckodriver gives a clean Rust path where WebKit would drag in a Node runtime.

Stealth-patching Chromium — nulling `navigator.webdriver`, spoofing WebGL, restoring `window.chrome`, stripping the automation flag — changed nothing: 8/18 either way on the 18-target subset where it was measured. Modern detection reads the handshake, not those properties.

geckodriver stays resident and sessions open per page; cold-starting it per URL dominates otherwise. Cloudflare interstitials need several seconds of JavaScript before the real page appears, so the browser tier polls the DOM until the wall clears rather than reading it once and returning the challenge.

**Extraction.** [`dom_smoothie`](https://crates.io/crates/dom_smoothie) finds the main content and [`htmd`](https://crates.io/crates/htmd) converts it. When readability returns almost nothing, or discards a data table it should have kept, ycrawl falls back to whole-document conversion and reports `path: document` so you can tell which ran.

## Markdown fidelity

Structure is where extraction quality actually lives, so several conventions are normalised before conversion:

- **Bare `<pre>` becomes a fenced code block.** Converters only fence `<pre><code>`, and plenty of documentation ships neither — the Python docs among them, where 34 code blocks were arriving as loose prose.
- **Language tags are recovered** from `language-x`, `lang-x`, `highlight-python3` (Sphinx), `highlight-source-x` (GitHub) and `brush: js` (MDN). Syntax-highlight `<span>` soup inside code is discarded, which is pure token cost otherwise.
- **Headerless data tables gain a header** so they survive as markdown tables instead of collapsing into loose cells. Layout tables — nested, or with ragged rows — are deliberately left as prose, because forcing Hacker News into a grid reads worse than the paragraph form.
- **Site controls are dropped.** Hide, flag, vote and reply each cost a full absolute URL and carry nothing a reader wants.
- **Links are absolute and clean.** Resolution happens before conversion, since once a link is `[text](/some/path)` the base is gone. Tracking parameters are stripped.
- **Scripts, styles, iframes and inline SVG are removed.** An icon set alone can cost thousands of tokens.

Against the reference tools on six pages, ycrawl produces fewer tokens than Obscura on five and retains 87–119% of each document's non-link prose.

One caveat on that comparison: heading and row counts are **not** a fair yardstick against tools that keep navigation. Obscura reports 111 headings on the Stripe API reference and 11 on the Svelte docs; those are sidebars. ycrawl keeps 5 and 0 while retaining 89% and 87% of the real prose.

## Results

Measured on the 55-URL benchmark corpus — 9 undefended pages plus 46 behind bot walls, login walls or paywalls.

| | tier 1 only | with escalation |
|---|---|---|
| Undefended pages | 9/9 | 9/9 |
| Bot-walled / paywalled | 22/46 | **34/46** |
| Median fetch | 180 ms | 479 ms |

The router earns its keep: 58% of fetches never touch a browser, 31% escalate where it pays, and 11% hit walls where escalation is correctly refused.

## Agent skill

`claude-plugin/` ships a Claude Code skill so an agent uses ycrawl well without being told how.

```
/plugin marketplace add /path/to/ycrawl
/plugin install ycrawl@ycrawl-local
```

It teaches three things that matter more than the flag list: run `--summary` first so a 15,000-token page body never lands in context uninvited; read the verdict before retrying; and stop entirely on DataDome or PerimeterX rather than looping.

Verified end to end. A fresh agent given only the skill — with ycrawl's source explicitly off limits — batched both URLs into one `--summary` call, pulled only the body that had content, followed a link and capped it with `--max-chars`, never retried the walled page, and reported the block plainly with alternatives. Command logging confirmed its self-report exactly.

## Limitations

- **Commercial bot walls stay shut.** DataDome and PerimeterX resisted every engine tested. That is a residential-IP problem, which is rented rather than built, and ycrawl says so instead of pretending otherwise.
- **Browser-tier results always report HTTP 200.** WebDriver exposes no status line. Trust the verdict, not the status.
- **ycrawl does not search.** It fetches a URL you already have.
- **The Firefox advantage may be self-erasing.** It is most likely an artefact of anti-bot vendors prioritising the engine everyone automates. If Firefox-based scraping becomes common the gap closes, which is the argument for keeping the benchmark harness maintained.

## Development

```bash
cargo test                              # 16 tests
cargo build --release
ycrawl --html-file saved.html           # replay a saved response through the classifier
```

[`ycrawl-bench`](https://github.com/yetidevworks/ycrawl-bench) holds the measurement harness: `targets.json` is the corpus, `run.py` fetches with every candidate engine, `report.py` classifies, `eval_ycrawl.py` scores the real binary, and `quality.py` compares markdown output against the reference tools. Adding an engine is one function.

## Layout

```
crates/ycrawl-core    fetch, clean, extract, verdict, browser
crates/ycrawl-cli     the ycrawl binary
claude-plugin/        Claude Code skill
```

## License

MIT
