# ycrawl

`ycrawl` fetches a web page and turns the useful part into clean markdown.

```bash
ycrawl https://doc.rust-lang.org/book/ch01-00-getting-started.html
```

It is built for agents and command-line workflows: no API key, no hosted service,
and no fetched pages written to disk. It removes navigation and other page clutter,
keeps useful structure such as headings, links, tables, and code blocks, and tells
you when a page could not be read.

## Install

```bash
brew tap yetidevworks/ycrawl
brew trust yetidevworks/ycrawl
brew install ycrawl
```

Homebrew requires the `trust` step for third-party taps. Prebuilt macOS and Linux
binaries for arm64 and x86_64 are also available on the
[releases page](https://github.com/yetidevworks/ycrawl/releases).

To build from source:

```bash
cargo install --path crates/ycrawl-cli
```

Browser fallback requires Firefox and geckodriver:

```bash
brew install geckodriver
```

ycrawl still works without them; it just cannot retry pages that need a browser.

## Usage

```text
ycrawl <URL>...                    markdown with YAML frontmatter
ycrawl --summary <URL>...          metadata only, one result per page
ycrawl --json <URL>...             JSON (NDJSON for multiple URLs)
ycrawl --links <URL>               absolute outbound links only
ycrawl --max-chars 4000 <URL>      truncate the body
ycrawl --no-images <URL>           omit images
ycrawl --escalate never <URL>      disable browser fallback
ycrawl --concurrency 8 a b c       fetch several URLs concurrently
ycrawl --html-file page.html       convert a local HTML file
```

For agent use, start with `--summary`. It lets the agent check whether a page is
readable and how large it is before pulling the full body into context.

```text
$ ycrawl --summary https://docs.stripe.com/api/charges
https://docs.stripe.com/api/charges  [200]  1422 words  1295ms  content  via Http
  title: Charges | Stripe API Reference
```

You can pass several URLs in one command. They are fetched concurrently.

## Browser fallback and verdicts

Most pages are fetched directly. If the result looks like a JavaScript shell or a
Cloudflare challenge, ycrawl can retry it in headless Firefox. It avoids opening a
browser when the first result is already useful or when a retry is unlikely to help.

Each result includes a verdict:

| Verdict | Meaning |
|---|---|
| `content` | Usable page content was found |
| `thin` | The page parsed, but appears to be a shell |
| `js-required` | The page needs JavaScript |
| `blocked by Cloudflare challenge` | A Cloudflare interstitial was returned |
| `blocked by DataDome` / `PerimeterX` | A bot wall was returned and browser fallback is unlikely to help |

The verdict is based on the original HTML as well as the extracted text. This helps
ycrawl distinguish a genuinely empty page from a challenge page without treating
every site that loads an anti-bot script as blocked.

## Output

ycrawl extracts the main content with
[`dom_smoothie`](https://crates.io/crates/dom_smoothie) and converts it with
[`htmd`](https://crates.io/crates/htmd). If main-content extraction drops too much,
it falls back to converting the whole document.

The markdown includes YAML frontmatter with the URL, title, word count, verdict,
and fetch tier. Relative links become absolute, common tracking parameters are
removed, code blocks keep their language where possible, and page controls,
scripts, styles, iframes, and inline SVG are dropped.

## Benchmarks

The [ycrawl-bench](https://github.com/yetidevworks/ycrawl-bench) harness tests 55
live URLs, including ordinary pages, bot walls, login walls, and paywalls.

| | Direct fetch | With browser fallback |
|---|---:|---:|
| Ordinary pages | 9/9 | 9/9 |
| Defended or restricted pages | 22/46 | 34/46 |
| Median fetch time | 180 ms | 479 ms |

These figures are a snapshot from August 2026. Anti-bot systems change, so the
benchmark is kept separate and repeatable rather than treated as a permanent claim.

## Agent skills

The included Claude Code and Codex skills teach agents to check summaries first,
batch URLs, and stop retrying pages that ycrawl identifies as blocked.

### Codex

Codex finds the skill in `.agents/skills/ycrawl/` automatically when you open this
project. Use it directly with `$ycrawl`, or let Codex select it when you ask to read
a URL.

To make it available in every project, link it from your personal skills folder:

```bash
mkdir -p ~/.agents/skills
ln -s /path/to/ycrawl/.agents/skills/ycrawl ~/.agents/skills/ycrawl
```

See the [Codex skill documentation](https://learn.chatgpt.com/docs/build-skills) for
other installation options.

### Claude Code

```text
/plugin marketplace add /path/to/ycrawl
/plugin install ycrawl@ycrawl-local
```

## Limitations

- ycrawl fetches URLs; it does not search for them.
- DataDome and PerimeterX blocked every browser tested by the benchmark.
- Browser results report HTTP 200 because WebDriver does not expose the response
  status. Use the verdict instead.
- Browser fallback is slower and needs local Firefox and geckodriver.

## Development

```bash
cargo test
cargo build --release
ycrawl --html-file saved.html
```

```text
crates/ycrawl-core    fetching, extraction, verdicts, and browser fallback
crates/ycrawl-cli     command-line interface
claude-plugin/        Claude Code skill
.agents/skills/       Codex skill
```

## License

MIT
