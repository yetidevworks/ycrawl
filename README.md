# ycrawl

Point it at a URL. Get clean markdown back. Nothing gets cached, indexed, or written to disk.

```bash
ycrawl https://doc.rust-lang.org/book/ch01-00-getting-started.html
```

## Why I built it

Agent web tooling wastes tokens in two ways. It fetches pages that hand back a navigation menu instead of an article, and then it dumps whatever it got straight into the conversation. One documentation page can cost you 15,000 tokens when the part you wanted was three paragraphs.

ycrawl fixes both. Extraction throws away the furniture, and `--summary` lets an agent check what a page actually contains for about twenty tokens before deciding to read it.

It also tells you why a page came back empty. That matters more than it sounds. A bot wall and a blank page look identical to most tools, so an agent either retries forever or quietly reports nothing and moves on.

Every decision here got measured instead of guessed. The [ycrawl-bench](https://github.com/yetidevworks/ycrawl-bench) harness runs candidate engines against 55 live URLs picked for how hard they push back, and every number in this README comes out of it. Some of the results surprised me.

## Why a local binary

Hosted services like Firecrawl pitch themselves as infrastructure. "The context API to search, scrape, and interact with the web at scale." That's a fair description of what they are, and if you need to crawl a whole site or get through walls that want residential IP rotation, go use one. ycrawl does none of that.

It sits at the other end of the trade. One binary, on your machine, that reads a page well.

No key. No credits. No per-page cost, so hammer the same docs page fifty times while you work through a problem and nothing meters you. Nothing leaves your machine either, which matters if the pages you read are competitor sites or client work or internal docs. And there's no service to be down, no round trip to somebody else's API before the round trip to the page you actually wanted.

The bet is that most of what an agent needs from the web is this page, right now, cleanly. And that a straight answer about why a page can't be read is worth as much as the page.

## Compared to your agent's built-in fetch

Your agent probably ships one already. Claude Code has `WebFetch`, Cursor and Codex have their own. Those are the real competition, not `curl`, and they're good at what they do. Zero install, decent answers.

Three places they run out of road.

**You get a summary, not the page.** A small model reads the page and hands back its answer. You can't grep that. You can't quote an exact line, diff it against last week, or pass it to anything downstream. And if the small model skipped the paragraph you needed, nothing tells you. ycrawl gives you the markdown, headings and code fences and absolute links intact.

**Failures are opaque.** A defended page returns an error, not a diagnosis. So your agent can't tell an empty page from a Cloudflare interstitial from DataDome, and it does one of the two worst things available: retry something that will never work, or hand you nothing and say the page was empty.

**No escalation.** Built-in fetches are one-shot HTTP. A JavaScript-rendered page comes back empty and stays empty.

| | built-in agent fetch | `curl` | headless browser | ycrawl |
|---|---|---|---|---|
| What you get | a model's summary | raw HTML | raw DOM | clean markdown |
| Stripe API reference | opaque | 447,849 tokens | ~100k tokens | **3,269 tokens** |
| Survives TLS fingerprinting | varies | no | yes | yes |
| Renders JavaScript | no | no | always | when it helps |
| Tells you why it failed | an error | a status code | no | a verdict |
| Batch several URLs | one at a time | shell scripting | yes | yes |
| A page it can't fetch | retries forever | retries forever | 5s to the same wall | tells you to stop |

`curl` isn't really the competition, but it's worth one measurement because the failure isn't the one people expect. "Works in my browser, 403s in my script" gets decided at the TLS handshake, before a byte of HTTP goes out. Spoofing your user-agent does nothing. Plain curl cleared 5 of 18 bot-walled pages in testing. Same requests with a browser fingerprint cleared 9.

Driving a headless browser yourself works too, and that's exactly what ycrawl falls back to. But paying four seconds and 300MB on every fetch is a bad default when a fingerprinted HTTP request gets the same page in 180ms.

## Installation

```bash
brew tap yetidevworks/ycrawl
brew trust yetidevworks/ycrawl
brew install ycrawl
```

Don't skip `brew trust`. Recent Homebrew refuses to load formulae from third-party taps until you trust them, and you'll get a wall of an error instead of an install.

Prebuilt binaries for macOS and Linux, arm64 and x86_64, hang off every [release](https://github.com/yetidevworks/ycrawl/releases). Or build it:

```bash
cargo install --path crates/ycrawl-cli
```

Browser escalation needs Firefox and geckodriver on top of that:

```bash
brew install geckodriver
```

Skip them and tier 1 still works fine. ycrawl will tell you why it couldn't escalate rather than failing quietly.

## Usage

```
ycrawl <URL>...                    markdown with YAML frontmatter
ycrawl --summary <URL>...          one metadata line per page, no body
ycrawl --json <URL>...             JSON, or NDJSON for several URLs
ycrawl --links <URL>               outbound links only, absolute
ycrawl --max-chars 4000 <URL>      truncate the body, with a marker
ycrawl --no-images <URL>           drop image markup
ycrawl --profile safari <URL>      chrome | firefox | safari | random
ycrawl --escalate never <URL>      tier 1 only, skip the browser
ycrawl --concurrency 8 a b c       several URLs at once
ycrawl --html-file page.html       convert a local file instead of fetching
```

Lead with `--summary`. It costs almost nothing and tells you whether a page is worth reading:

```
$ ycrawl --summary https://docs.stripe.com/api/charges
https://docs.stripe.com/api/charges  [200]  1422 words  1295ms  content  via Http
  title: Charges | Stripe API Reference
```

## Verdicts

Every fetch says what it got, and whether a browser would do better. Those recovery rates are measured. I took every tier-1 failure in the corpus and re-ran it through a real Firefox to see what a browser actually buys you.

| verdict | what it means | browser worth trying? |
|---|---|---|
| `content` | real page text | not needed |
| `thin` | parsed, but it's a shell | yes, 6 of 10 recovered |
| `js-required` | page says it needs scripting | yes |
| `blocked by Cloudflare challenge` | interstitial | yes, 3 of 4 recovered |
| `blocked by DataDome` / `PerimeterX` | commercial bot wall | no. 0 of 6 recovered |

DataDome and PerimeterX held against every engine I tested, real Chrome included. So ycrawl reports them instead of burning five seconds to arrive at the same wall:

```
https://www.yelp.com/  [403]  8 words  107ms  blocked by DataDome  via Http
  this wall held against every engine benchmarked, including real Chrome. A browser will not help
```

Classification runs on raw HTML before cleaning, because several wall signatures live in script and iframe sources that extraction strips out.

Two rules keep it honest, and I learned both the hard way after scoring perfectly good fetches as blocked.

Vendor-presence strings never decide a verdict on their own. Cloudflare injects `challenge-platform` on successful loads too, and treating that as proof of a block mis-scored four working pages.

Body text beats challenge markup. seekingalpha.com serves 1,305 words of market data while carrying PerimeterX's `px-captcha` three times over. udemy.com renders its whole homepage with "Just a moment" sitting in the source. Both were getting reported as blocked.

## How it works

**Tier 1 is HTTP with a browser fingerprint.** Most of those "works in my browser" failures get decided at the TLS handshake. Presenting a real Chrome JA3/JA4 and HTTP/2 fingerprint through [`wreq`](https://crates.io/crates/wreq) took bot-walled pages from 5 of 18 to 9 of 18, with no browser anywhere, at a 126ms median. Best single thing in the whole stack.

**Tier 2 is headless Firefox**, and only when the verdict says a browser helps. Firefox over Chromium is measured, not preference, and the result annoyed me:

| engine | bot-walled pages cleared, of 46 |
|---|---|
| WebKit | 35 |
| **Firefox** | **34** |
| Chromium, headless | 26 |
| Chrome, real, headless | 26 |
| HTTP with a TLS fingerprint | 26 |

Headless Chromium ties a plain HTTP client. Thirty times slower for nothing. A Chromium tier would have been dead weight. Firefox wins over WebKit on plumbing: geckodriver gives a clean Rust path, WebKit drags in a Node runtime.

And stealth patching did nothing at all. Nulling `navigator.webdriver`, spoofing WebGL, restoring `window.chrome`, stripping the automation flag: 8 of 18 either way on the subset where I measured it. Detection happens at the handshake, long before any of those properties can be read.

geckodriver stays resident and sessions open per page, because cold-starting it every time dominates everything else. Cloudflare interstitials need several seconds of JavaScript before the real page shows up, so the browser tier polls the DOM until the wall clears instead of reading it once and returning the challenge.

**Extraction** runs [`dom_smoothie`](https://crates.io/crates/dom_smoothie) to find the main content and [`htmd`](https://crates.io/crates/htmd) to convert it. When readability comes back with nothing, or throws away a data table it should have kept, ycrawl falls back to whole-document conversion and reports `path: document` so you know which one ran.

## Markdown fidelity

Structure is where extraction quality actually lives. A few conventions get normalised before conversion.

Bare `<pre>` becomes a fenced code block. Converters only fence `<pre><code>`, and plenty of docs ship neither. The Python docs are the case that caught me: 34 code blocks were arriving as loose prose.

Language tags get recovered from `language-x`, `lang-x`, `highlight-python3` (Sphinx), `highlight-source-x` (GitHub) and `brush: js` (MDN). Syntax-highlight `<span>` soup inside code gets thrown out, since it's pure token cost.

Headerless data tables get a header so they survive as markdown tables rather than collapsing into loose cells. Layout tables, the nested or ragged ones, stay as prose. Forcing Hacker News into a grid reads worse than paragraphs.

Site controls get dropped. Hide, flag, vote, reply. Each one costs a full absolute URL and carries nothing you want.

Links come out absolute with tracking parameters stripped, and that has to happen before conversion. Once a link is `[text](/some/path)` the base is gone. Scripts, styles, iframes and inline SVG go too, since an icon set on its own can run to thousands of tokens.

Against the reference tools on six pages, ycrawl produces fewer tokens than Obscura on five of them and keeps 87% to 119% of each document's non-link prose.

One caveat on that comparison. Heading and row counts are a bad yardstick against tools that keep navigation. Obscura reports 111 headings on the Stripe API reference and 11 on the Svelte docs. Those are sidebars. ycrawl keeps 5 and 0, and still retains 89% and 87% of the real prose.

## Results

Measured across the 55-URL corpus: 9 undefended pages, 46 behind bot walls, login walls or paywalls.

| | tier 1 only | with escalation |
|---|---|---|
| Undefended pages | 9/9 | 9/9 |
| Bot-walled or paywalled | 22/46 | **34/46** |
| Median fetch | 180ms | 479ms |

The router does real work. 58% of fetches never touch a browser, 31% escalate where it pays, and 11% hit walls where escalating gets refused. Those refusals would have cost about five seconds each to learn nothing.

## Agent skill

`claude-plugin/` ships a Claude Code skill so an agent uses ycrawl properly without being told how.

```
/plugin marketplace add /path/to/ycrawl
/plugin install ycrawl@ycrawl-local
```

It teaches three things that matter more than the flag list. Run `--summary` first so a 15,000-token page body never lands in context uninvited. Read the verdict before retrying. And stop on DataDome or PerimeterX instead of looping.

I tested it end to end. A fresh agent, given only the skill and with ycrawl's source explicitly off limits, batched both URLs into one `--summary` call, pulled only the body that had content, followed a link and capped it with `--max-chars`, never retried the walled page, and reported the block plainly with alternatives. Command logging confirmed its self-report exactly.

## What it won't do

Commercial bot walls stay shut. DataDome and PerimeterX beat every engine I tested. That's a residential IP problem, which you rent rather than build, and ycrawl says so instead of pretending.

Browser-tier results always report HTTP 200, because WebDriver exposes no status line. Trust the verdict, not the status.

ycrawl doesn't search. It fetches a URL you already have, so pair it with whatever search tool your agent already has.

And the Firefox advantage might erase itself. It's most likely an artifact of anti-bot vendors aiming at the engine everyone automates. If Firefox-based scraping gets popular the gap closes, which is the argument for keeping the benchmark harness alive rather than trusting a number from August 2026.

## Development

```bash
cargo test                              # 16 tests
cargo build --release
ycrawl --html-file saved.html           # replay a saved response through the classifier
```

[ycrawl-bench](https://github.com/yetidevworks/ycrawl-bench) holds the measurement harness. `targets.json` is the corpus, `run.py` fetches with every candidate engine, `report.py` classifies, `eval_ycrawl.py` scores the real binary, `quality.py` compares markdown against the reference tools. Adding an engine is one function.

## Layout

```
crates/ycrawl-core    fetch, clean, extract, verdict, browser
crates/ycrawl-cli     the ycrawl binary
claude-plugin/        Claude Code skill
```

## License

MIT
