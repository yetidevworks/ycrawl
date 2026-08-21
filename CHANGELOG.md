# 1.0.0

## 08/21/2026

1. [](#new)
    * First stable release. Extraction, the two-tier fetch and the verdicts it
      reports have held up across enough sites to stop calling this a 0.x, so
      the version now matches. The CLI flags and the JSON shape are settled.

# 0.2.1

## 08/18/2026

1. [](#bugfix)
    * HTTP redirects are now followed. A URL that answered with a 301 or 302
      returned the redirect stub rather than the page it pointed at, which read
      as a thin result instead of the obvious "you were sent somewhere else".

# 0.2.0

## 08/18/2026

1. [](#new)
    * PDF support. A PDF with selectable text comes back page by page as
      markdown. Image-only scans need OCR, which ycrawl does not do, and it
      says so rather than returning an empty page.
1. [](#improved)
    * Reworked extraction. Navigation, cookie banners, scripts and inline SVG
      are stripped more reliably, code blocks keep their language hint, and
      links come back absolute with tracking parameters removed.

# 0.1.0

## 08/18/2026

1. [](#new)
    * Initial release. Fetches a page and returns clean markdown with YAML
      frontmatter, escalating to a real browser only where that is measured to
      help. Every fetch reports an honest verdict, so a page behind DataDome is
      reported as blocked rather than silently returned as nothing.
