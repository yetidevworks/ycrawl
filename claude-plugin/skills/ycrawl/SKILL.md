---
name: ycrawl
description: "Fetch a web page and get clean markdown back. Use whenever you have a URL and need its content: \"read this page\", \"fetch\", \"scrape\", \"grab the content from\", \"what does this page say\", \"get the docs at\", \"pull the changelog\", or any bare URL the user wants you to look at. Handles JavaScript-rendered pages and most bot walls, and tells you plainly when a page is blocked instead of returning an empty result. Use WebSearch first if you need to FIND a URL — ycrawl fetches, it does not search."
license: MIT
---

# ycrawl

`ycrawl <URL>` prints a web page as markdown on stdout. Nothing is cached or written
to disk.

## Check before you read

Page bodies are the single largest thing you can accidentally pull into context. A
documentation page can be 15,000 tokens, and most of the time you need a fraction of
it. **Start with `--summary`** — it costs about 20 tokens per URL and tells you
whether the page has content worth reading.

```bash
ycrawl --summary https://example.com/docs/api
# https://example.com/docs/api  [200]  1424 words  180ms  content  via Http
#   title: API Reference
```

Then pull the body once you know it is worth pulling:

```bash
ycrawl https://example.com/docs/api              # full markdown with frontmatter
ycrawl --max-chars 4000 https://example.com/x    # truncated, with a marker
ycrawl --links https://example.com/              # just the outbound links
```

For several URLs, pass them in **one call** rather than looping — they are fetched
concurrently and a single geckodriver is reused for any escalation:

```bash
ycrawl --summary url1 url2 url3
ycrawl --json url1 url2          # NDJSON, one object per line
```

## Read the verdict before you retry

Every fetch reports what it actually got. This is the difference between telling the
user "that page is behind a bot wall" and silently handing them nothing.

| verdict | what it means | what to do |
|---|---|---|
| `content` | real page text | use it |
| `thin` | parsed, but a shell | ycrawl already retried in a browser; report what you got |
| `js-required` | page needs scripting | already retried in a browser |
| `blocked by Cloudflare challenge` | interstitial | already retried; usually succeeds |
| `blocked by DataDome` / `PerimeterX` | commercial bot wall | **stop.** Tell the user. Retrying will not work |

ycrawl escalates to a real browser on its own where that is measured to help, so a
non-`content` verdict means the browser tier already ran or was deliberately skipped.
**Do not loop retries** — if it says DataDome, no amount of retrying or switching
tools will get that page. Say so and offer the user an alternative.

When a page is blocked, report it plainly:

> That page is behind DataDome's bot protection, which blocks automated fetches.
> I can't read it directly — could you paste the relevant section?

## Options worth knowing

| flag | use |
|---|---|
| `--summary` | metadata only, no body. Your default first move |
| `--max-chars N` | cap the body; adds a truncation marker |
| `--links` | outbound links only, absolute |
| `--json` | structured output; NDJSON for several URLs |
| `--escalate never` | tier 1 only — fast, skips the browser |
| `--no-images` | drop image markup |
| `--timeout N` | per-request seconds, default 20 |

## What you get

Markdown with YAML frontmatter: url, title, word count, verdict, tier, plus
description and byline where the page provides them.
Links are absolute with tracking parameters stripped. Code blocks are fenced with
their language. Navigation, cookie banners, scripts and inline SVG are removed.

## Notes

- Requires `ycrawl` on PATH (`cargo install --path crates/ycrawl-cli`). Browser
  escalation additionally needs `geckodriver` and Firefox; without them tier 1 still
  works and ycrawl says why it could not escalate.
- Escalation to a browser costs a few seconds; `--escalate never` when speed matters
  more than reach.
- Browser-tier results always report HTTP 200 — WebDriver exposes no status line.
  Trust the verdict, not the status.
- `--html-file <PATH>` converts a local HTML file instead of fetching.
- ycrawl does not search. Find the URL with WebSearch, then fetch it here.

## Keywords

fetch, scrape, crawl, read page, get content, pull page, url, webpage, markdown,
documentation, changelog, article, extract page, download page, web content
