# Using nokk from other tools

Anything that drives Chrome over the DevTools Protocol can drive nokk: point it at
nokk's endpoint instead of launching a browser. The tool keeps working as it was,
and the pages it visits see Chrome 151 on Linux, Cloudflare's challenges included.

## crawl4ai

[crawl4ai](https://github.com/unclecode/crawl4ai) turns pages into Markdown for LLMs.
Its own Chromium is stopped by Cloudflare ("Blocked by anti-bot protection"); with
nokk the same crawl gets the page behind the challenge. Needs nokk 0.1.37 or newer.

<img src="crawl4ai-demo.svg" alt="crawl4ai with its own Chromium is blocked by Cloudflare; the same crawl through nokk gets the page" width="760">

```python
import asyncio, nokk
from crawl4ai import AsyncWebCrawler, BrowserConfig, CacheMode, CrawlerRunConfig

async def main():
    with nokk.launch(auto_solve=True) as server:
        browser = BrowserConfig(browser_mode="custom", cdp_url=server.ws_endpoint)
        async with AsyncWebCrawler(config=browser) as crawler:
            r = await crawler.arun("https://www.scrapingcourse.com/cloudflare-challenge",
                                   config=CrawlerRunConfig(cache_mode=CacheMode.BYPASS))
            print(r.markdown)

asyncio.run(main())
```

The full script is [examples/crawl4ai_nokk.py](../examples/crawl4ai_nokk.py). A nokk
server that is already running works the same way: `cdp_url="ws://127.0.0.1:9222/devtools/browser/nokk"`
(add `?token=…` if it was started with one).

What does not work: crawl4ai's screenshots and PDFs (nokk has no rendering engine).

## Scrapy

[scrapy-playwright](https://github.com/scrapy-plugins/scrapy-playwright) connects to
a running browser with `PLAYWRIGHT_CDP_URL`; pointed at nokk, a spider gets pages
behind Cloudflare's challenge. Needs nokk 0.1.39 or newer (scrapy-playwright routes
every request, which needs request interception).

```python
with nokk.launch(auto_solve=True) as server:
    process = CrawlerProcess({
        "DOWNLOAD_HANDLERS": {
            "http": "scrapy_playwright.handler.ScrapyPlaywrightDownloadHandler",
            "https": "scrapy_playwright.handler.ScrapyPlaywrightDownloadHandler",
        },
        "TWISTED_REACTOR": "twisted.internet.asyncioreactor.AsyncioSelectorReactor",
        "PLAYWRIGHT_CDP_URL": server.ws_endpoint,
        "PLAYWRIGHT_PROCESS_REQUEST_HEADERS": None,   # the browser's headers, not Scrapy's
        "HTTPERROR_ALLOWED_CODES": [403],
    })
    process.crawl(MySpider)   # requests with meta={"playwright": True}
    process.start()
```

The status of a page behind a challenge is the challenge's 403: Playwright reports
a navigation's first response, with Chrome too. The body is the page behind it,
hence `HTTPERROR_ALLOWED_CODES`. The full script is
[examples/scrapy_nokk.py](../examples/scrapy_nokk.py). nokk keeps its Chrome
`User-Agent` and `Accept*` headers whatever a spider sets.

## Playwright MCP

[Playwright MCP](https://github.com/microsoft/playwright-mcp) gives an AI agent a
browser through Playwright. With `--cdp-endpoint` it drives nokk instead of
launching Chromium: navigation, page snapshots, clicks and typing work, and pages
behind Cloudflare come back as the page when nokk runs with `--auto-solve`.

```bash
nokk --port 9222 --auto-solve
```

```json
{
  "mcpServers": {
    "playwright": {
      "command": "npx",
      "args": ["@playwright/mcp@latest", "--cdp-endpoint", "ws://127.0.0.1:9222/devtools/browser/nokk"]
    }
  }
}
```

Checked with @playwright/mcp 0.0.83. Screenshots do not work (no rendering engine).
nokk also has its own MCP server, `pip install "nokk[mcp]"`, see the README.

## Playwright and Puppeteer

See the [README](../README.md#usage). Through Playwright: navigation, `evaluate`,
locators, cookies (`addCookies`, `cookies()`), new pages and CDP sessions, and
request interception: `page.route` (abort, fulfill, continue with changes) and
Puppeteer's `setRequestInterception`.

## browser-use

Not yet. browser-use reads pages through CDP domains nokk does not serve today
(`DOMSnapshot`, `Accessibility`, screenshots); support is planned.
