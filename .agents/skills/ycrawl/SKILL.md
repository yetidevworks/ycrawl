---
name: ycrawl
description: Fetch a known web page, text file, or PDF URL with the local ycrawl command and return clean markdown. Use when the user asks to read, fetch, scrape, inspect, quote, or extract content or links from a URL. ycrawl fetches URLs but does not search for them.
license: MIT
---

# ycrawl

Use `ycrawl` when the user already has a URL and needs the page content. It prints
clean markdown to standard output and reports whether it found useful content, a
JavaScript shell, or a bot wall.

If the user needs help finding a URL, search first with the available web search
tool. Then use ycrawl on the URL you found.

## Check the page first

Start with a summary so a large page does not enter the conversation before you
know it is useful:

```bash
ycrawl --summary https://example.com/docs/api
```

The summary shows the title, word count, fetch time, and verdict. Once the page is
known to contain useful content, fetch what the task needs:

```bash
ycrawl https://example.com/docs/api
ycrawl --max-chars 4000 https://example.com/docs/api
ycrawl --links https://example.com/docs/api
```

Use `--max-chars` when a limited section is enough. Use the full page when the user
needs exact wording, a complete document, or details that may appear near the end.

Pass several URLs in one command. ycrawl fetches them concurrently and reuses the
same browser process if fallback is needed:

```bash
ycrawl --summary https://example.com/a https://example.com/b
ycrawl --json https://example.com/a https://example.com/b
```

## Use the verdict

Read the verdict before deciding what to do next:

| Verdict | What to do |
|---|---|
| `content` | Use the result |
| `thin` / `shell (... words)` | Report that little usable content was found |
| `js-required` / `page requires JavaScript` | Browser fallback was attempted or unavailable; report the result |
| `blocked by Cloudflare challenge` | Browser fallback was attempted or unavailable; report the result |
| `blocked by DataDome` / `PerimeterX` | Stop and tell the user the page is blocked |
| `HTTP 404` | The page is missing; do not retry it |

ycrawl automatically retries with Firefox when a browser is likely to help. Do not
repeat the same request after a non-content verdict. For DataDome or PerimeterX,
ask the user to paste the relevant text or offer to look for another source.

If browser fallback was unavailable, say so plainly. The direct fetch may still
contain useful content.

## Useful options

| Option | Purpose |
|---|---|
| `--summary` | Show page details without the body |
| `--max-chars N` | Limit the body and add a truncation marker |
| `--links` | Print absolute outbound links only |
| `--json` | Return JSON, or one JSON object per line for several URLs |
| `--no-images` | Leave images out of the markdown |
| `--escalate never` | Skip browser fallback |
| `--timeout N` | Set the direct-fetch timeout in seconds |
| `--fail-on-error` | Fail a batch if any URL could not be read |

## Requirements and limits

- `ycrawl` must be installed and available on `PATH`.
- Browser fallback also needs Firefox and geckodriver. Direct fetching works without
  them.
- Browser results show an unknown status because Firefox does not expose the
  original response status. Trust the verdict instead.
- PDFs with selectable text are returned page by page. Image-only scans need OCR,
  which ycrawl does not provide.
- `ycrawl --html-file <PATH>` converts a local HTML file without fetching a URL.
- ycrawl does not search the web.
