"""Scrapy through nokk: scrapy-playwright drives nokk over CDP instead of its own
Chromium, and pages behind Cloudflare's challenge come back as the page itself.

    pip install scrapy scrapy-playwright nokk
    python examples/scrapy_nokk.py [url ...]
"""

import sys

import nokk
import scrapy
from scrapy.crawler import CrawlerProcess

URLS = sys.argv[1:] or ["https://www.scrapingcourse.com/cloudflare-challenge"]


class TitleSpider(scrapy.Spider):
    name = "nokk"

    async def start(self):
        for url in URLS:
            yield scrapy.Request(url, meta={"playwright": True})

    def parse(self, response):
        yield {"url": response.url, "status": response.status,
               "title": response.css("title::text").get()}


with nokk.launch(auto_solve=True) as server:
    process = CrawlerProcess({
        "DOWNLOAD_HANDLERS": {
            "http": "scrapy_playwright.handler.ScrapyPlaywrightDownloadHandler",
            "https": "scrapy_playwright.handler.ScrapyPlaywrightDownloadHandler",
        },
        "TWISTED_REACTOR": "twisted.internet.asyncioreactor.AsyncioSelectorReactor",
        "PLAYWRIGHT_CDP_URL": server.ws_endpoint,
        # Keep the browser's own request headers instead of Scrapy's.
        "PLAYWRIGHT_PROCESS_REQUEST_HEADERS": None,
        # The navigation's first answer is the challenge's 403, as with Chrome;
        # the body Scrapy gets is the page behind it.
        "HTTPERROR_ALLOWED_CODES": [403],
        "FEEDS": {"stdout:": {"format": "jsonlines"}},
        "LOG_LEVEL": "WARNING",
    })
    process.crawl(TitleSpider)
    process.start()
