"""
Urban Integrity — Scrapling Sidecar

HTTP sidecar for scraping UK planning portals via Scrapling.
All scraping is done synchronously behind a FastAPI endpoint.

Start with:
    uvicorn main:app --port 8765 --host 127.0.0.1
"""
import logging
from typing import Optional

from fastapi import FastAPI
from pydantic import BaseModel

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

app = FastAPI(
    title="Urban Integrity Scrapling Sidecar",
    version="0.1.0",
    description="HTTP proxy for Scrapling-based planning portal scraping",
)


class ScrapeRequest(BaseModel):
    url: str
    # CSS selectors dict: first key is the container selector, rest are field selectors
    selectors: dict[str, str]
    timeout: Optional[int] = 30


class ScrapeResponse(BaseModel):
    status: str  # "ok" or "error"
    results: Optional[list[dict[str, Optional[str]]]] = None
    reason: Optional[str] = None


@app.get("/health")
async def health():
    """Liveness check — returns ok when the sidecar is running."""
    return {"status": "ok"}


@app.post("/scrape", response_model=ScrapeResponse)
async def scrape(req: ScrapeRequest) -> ScrapeResponse:
    """
    Scrape a planning portal URL using Scrapling.

    The `selectors` dict uses the first key as the container selector
    (selects repeated items), and subsequent keys as field selectors
    relative to each container element.
    """
    try:
        # Import here so the module is importable even if scrapling is not installed
        # (e.g. for syntax checking). The endpoint will fail at call time if missing.
        from scrapling import Fetcher  # type: ignore

        logger.info(f"Scraping: {req.url}")
        fetcher = Fetcher()
        page = fetcher.get(req.url, timeout=req.timeout)

        selector_items = list(req.selectors.items())
        if not selector_items:
            return ScrapeResponse(status="error", reason="No selectors provided")

        container_label, container_selector = selector_items[0]
        field_selectors = selector_items[1:]

        containers = page.css(container_selector)
        results = []

        for container in containers:
            item: dict[str, Optional[str]] = {}
            for label, selector in field_selectors:
                try:
                    el = container.css_first(selector)
                    item[label] = el.text.strip() if el else None
                except Exception:
                    item[label] = None
            results.append(item)

        logger.info(f"Scraped {len(results)} items from {req.url}")
        return ScrapeResponse(status="ok", results=results)

    except ImportError:
        logger.error("scrapling is not installed — install via: pip install scrapling")
        return ScrapeResponse(status="error", reason="scrapling not installed")
    except Exception as e:
        logger.error(f"Scrape error for {req.url}: {e}")
        return ScrapeResponse(status="error", reason=str(e))


if __name__ == "__main__":
    import uvicorn
    uvicorn.run(app, host="127.0.0.1", port=8765)
