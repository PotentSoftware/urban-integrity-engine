"""
UrbanIntegrity Scrapling Sidecar

Scrapes planning portals across England, Scotland and Wales and returns
structured planning application data to the Rust engine.

Supported portal types (via `portal_type` field in POST /scrape):
  "idox"    — Idox PublicAccess (~69% of UK LPAs, server-rendered HTML)
  "arcus"   — Arcus Global / Salesforce Experience Cloud (guest Aura JSON API; e.g. Anglesey)
  "wfs"     — WFS data source e.g. Scotland Spatial Hub (stub only)
  "planapi" — PlanAPI commercial aggregator (stub only)

Idox portal confirmed via: publicaccess.iow.gov.uk (live equivalent of
planning.anglesey.gov.wales, which is offline 11-26 May 2026).

Wire protocol (must match Rust crates/ingestor/src/planning/sidecar.rs):
  POST /scrape
    Request:  { "url": str, "selectors": {}, "timeout": int, "portal_type": str }
    Response: { "status": "ok"|"error", "results": [...], "reason": str|null }
  GET /health
    Response: { "status": "ok", "service": "scrapling-sidecar" }
"""
import re
import time
import os
import logging as _log
from datetime import datetime
from typing import Optional
from urllib.parse import urlparse

import uvicorn
from fastapi import FastAPI
from pydantic import BaseModel

# ─── Firecrawl fallback client ─────────────────────────────────────────────────
try:
    from firecrawl import FirecrawlApp as _FirecrawlApp
    _firecrawl = (
        _FirecrawlApp(api_key=os.environ["FIRECRAWL_API_KEY"])
        if os.environ.get("FIRECRAWL_API_KEY")
        else None
    )
except ImportError:
    _log.warning("firecrawl-py not installed — Firecrawl fallback disabled")
    _firecrawl = None


def _firecrawl_get(url: str) -> Optional[str]:
    """Fetch a URL via firecrawl, returning raw HTML.

    Returns None if the firecrawl client is not configured, or on any error.
    Never raises — callers treat None as a failed fetch.
    """
    if _firecrawl is None:
        return None
    try:
        resp = _firecrawl.scrape_url(url, formats=["html"])
        return resp.html
    except Exception as exc:
        status = getattr(exc, "status_code", None)
        if status == 402:
            _log.warning(
                "Firecrawl credits exhausted (402) — fallback disabled for this request: %s", url
            )
        else:
            _log.warning("Firecrawl GET failed for %s: %s", url, exc)
        return None


def _parse_idox_html(html: str, url: str) -> list[dict]:
    """Parse Idox PublicAccess search results from a raw HTML string.

    Applies the same CSS selectors as scrape_idox() but against a pre-fetched
    HTML string via scrapling.Selector (no network call).
    Returns [] if no results are found or parsing fails.
    """
    import re as _re
    try:
        from scrapling import Selector
        page = Selector(html)
        results = []
        for row in page.css("li.searchresult"):
            try:
                def _first(els):
                    return els[0] if els else None

                meta_els = row.css("p.metaInfo")
                meta_el = _first(meta_els)
                meta_text = meta_el.text if meta_el else ""
                ref_match = _re.search(r"Ref\.\s*No:\s*([\w/\-]+)", meta_text)
                if not ref_match:
                    continue
                reference = ref_match.group(1).strip()

                # D-07: extract validated/received date from the metaInfo string.
                # Idox emits "| Date: DD/MM/YYYY" (or "| Date Received: …") on list pages.
                # Normalise to ISO YYYY-MM-DD; degrade to None on absent/malformed input.
                received_date = None
                date_match = _re.search(r"\|\s*Date[^:]*:\s*(\d{2}/\d{2}/\d{4})", meta_text)
                if date_match:
                    try:
                        received_date = datetime.strptime(
                            date_match.group(1), "%d/%m/%Y"
                        ).strftime("%Y-%m-%d")
                    except ValueError:
                        received_date = None

                desc_els = row.css(".summaryLinkTextClamp")
                desc_el = _first(desc_els)
                description = desc_el.text.strip() if desc_el else None

                addr_els = row.css("p.address")
                addr_el = _first(addr_els)
                address = addr_el.text.strip() if addr_el else None

                link_el = _first(row.css("a.summaryLink")) or _first(
                    row.css("a[href*='applicationDetails']")
                )
                detail_href = link_el["href"] if link_el else None

                results.append({
                    "reference": reference,
                    "description": description,
                    "address": address,
                    "ward": None,
                    "applicant": None,
                    "received_date": received_date,
                    # Internal: lets the Firecrawl tiers fetch the detail page to
                    # fill ward/applicant. Stripped before the response is returned.
                    "_detail_href": detail_href,
                })
            except Exception:
                continue
        return results
    except Exception as exc:
        _log.warning("_parse_idox_html failed: %s", exc)
        return []


def _strip_internal(rows: list[dict]) -> list[dict]:
    """Remove internal-only keys (prefixed '_') before returning to the engine.

    D-06: before stripping, promote the portal's native record identifier —
    Arcus's `_sf_id` (Salesforce record id) or Idox's `_detail_href` — into a
    single portal-agnostic `detail_handle` key, so it survives to the wire
    response instead of being silently discarded with the rest of the
    `_`-prefixed internal keys. This is the single promotion point for every
    caller that routes rows through `_strip_internal`; do not duplicate this
    logic at individual call sites.
    """
    for r in rows:
        if r.get("detail_handle") is None:
            handle = r.get("_sf_id") or r.get("_detail_href")
            if handle:
                r["detail_handle"] = handle
        for k in [k for k in r if k.startswith("_")]:
            r.pop(k, None)
    return rows


def _enrich_idox_rows_via_firecrawl(rows: list[dict], base_url: str) -> list[dict]:
    """For each parsed Idox row that carries a `_detail_href`, fetch its detail
    page via Firecrawl and fill ward/applicant. Without this, Firecrawl-sourced
    rows always have applicant=None, which collapses every applicant to
    'Unknown Applicant' downstream. Best-effort: a failed detail fetch leaves the
    row's applicant as None rather than aborting the batch."""
    if _firecrawl is None:
        return _strip_internal(rows)
    for r in rows:
        href = r.get("_detail_href")
        if not href:
            continue
        detail_html = _firecrawl_get(_detail_url(base_url, href))
        if not detail_html:
            continue
        ward, applicant, received_date = _extract_detail_fields_from_html(detail_html)
        if ward and not r.get("ward"):
            r["ward"] = ward
        if applicant and not r.get("applicant"):
            r["applicant"] = applicant
        if received_date and not r.get("received_date"):
            r["received_date"] = received_date
    return _strip_internal(rows)


def _firecrawl_idox_actions(url: str) -> list[dict]:
    """Second-tier Idox fallback: use firecrawl browser actions to replicate
    the Idox GET-form → fill-ward → POST submit flow.

    Credit-heavier than _firecrawl_get but handles portals that require
    genuine session state. Returns [] if firecrawl is unconfigured or fails.
    """
    if _firecrawl is None:
        return []
    try:
        from urllib.parse import parse_qs, urlparse
        parsed = urlparse(url)
        ward = parse_qs(parsed.query).get("searchCriteria.ward", [""])[0]
        path_prefix = parsed.path.rsplit("/", 1)[0]
        base_url = f"{parsed.scheme}://{parsed.netloc}"
        form_url = f"{base_url}{path_prefix}/search.do?action=advanced"

        actions = [
            {"type": "wait", "milliseconds": 1500},
            {"type": "write", "text": ward, "selector": "[name='searchCriteria.ward']"},
            {"type": "click", "selector": "input[type='submit'], button[type='submit']"},
            {"type": "wait", "milliseconds": 2000},
            {"type": "scrape"},
        ]
        resp = _firecrawl.scrape_url(form_url, formats=["html"], actions=actions)
        html = resp.html
        if not html:
            return []
        return _enrich_idox_rows_via_firecrawl(_parse_idox_html(html, url=url), base_url)
    except Exception as exc:
        _log.warning("Firecrawl actions fallback failed for %s: %s", url, exc)
        return []


app = FastAPI(title="UrbanIntegrity Scrapling Sidecar", version="1.0.0")


# ─── URL guard (CR-07) ─────────────────────────────────────────────────────────
#
# `/scrape` takes an arbitrary `url` and makes the host fetch it, then returns the parsed
# body to the caller — a textbook SSRF primitive. Combined with the old `host="0.0.0.0"`
# bind, anyone who could reach port 8765 could make this process fetch the cloud metadata
# service, an internal admin endpoint, or SurrealDB's own HTTP API on localhost:8000, and
# read the response back.
#
# A strict portal allow-list is not workable here: the sidecar is a separate process that
# does not read config.toml, and the project targets many LPAs across GB. So the guard is
# default-deny on *destination class* rather than on individual hosts — every private,
# loopback, link-local, multicast or otherwise reserved address is refused, which is what
# actually makes SSRF dangerous. Set SIDECAR_ALLOWED_HOSTS to a comma-separated list to
# additionally pin exact hostnames.

_ALLOWED_HOSTS = {
    h.strip().lower()
    for h in os.environ.get("SIDECAR_ALLOWED_HOSTS", "").split(",")
    if h.strip()
}


def _same_host(a: str, b: str) -> bool:
    """True when two URLs share a host (case-insensitive), used to skip redundant
    re-validation of same-host detail links (PS-02)."""
    ha, hb = urlparse(a).hostname, urlparse(b).hostname
    return bool(ha) and bool(hb) and ha.lower() == hb.lower()


def _validate_scrape_url(url: str) -> None:
    """Refuse non-HTTP schemes and any host that resolves to a non-public address.

    Raises ScrapeFailed so the endpoint reports status="error" — a refused URL is a
    failure to measure, never an empty ward (CR-01).
    """
    import ipaddress
    import socket

    parsed = urlparse(url)
    if parsed.scheme not in ("http", "https"):
        raise ScrapeFailed(f"unsupported URL scheme: {parsed.scheme!r}")
    host = parsed.hostname
    if not host:
        raise ScrapeFailed(f"URL has no host: {url!r}")

    if _ALLOWED_HOSTS and host.lower() not in _ALLOWED_HOSTS:
        raise ScrapeFailed(f"host not in SIDECAR_ALLOWED_HOSTS: {host}")

    # Resolve first: a public-looking name can still point at 127.0.0.1 or 169.254.169.254.
    try:
        infos = socket.getaddrinfo(host, None)
    except socket.gaierror as exc:
        raise ScrapeFailed(f"cannot resolve host {host}: {exc}") from exc

    for info in infos:
        addr = ipaddress.ip_address(info[4][0])
        if not addr.is_global or addr.is_multicast:
            raise ScrapeFailed(
                f"refusing to fetch {host} — resolves to non-public address {addr}"
            )


# ─── Failure signalling ────────────────────────────────────────────────────────

class ScrapeFailed(Exception):
    """A strategy could not ask the portal — as distinct from asking and getting nothing.

    CR-01. This distinction is the whole point of the `ScrapeOutcome`/`ScrapeFailure`
    contract on the Rust side, and before this class existed the sidecar could not express
    it: every internal failure returned `[]`, which the endpoint wrapped as
    `{"status": "ok", "results": []}`. Rust read that as `ScrapeOutcome::Items(vec![])` ->
    `WardAction::Process(vec![])`, and the step-(6) set-difference then stamped
    `FetchFailed` across every unresolved Contract in the ward.

    Anglesey is `portal_type = "arcus"`, and Salesforce rotates its `fwuid` on every
    release (see the note in `scrape_arcus`), so the single most likely real-world failure
    reproduced the UAT-2026-07-27 defect that `ward_action` exists to prevent: a
    source-verified `absent_at_source` overwritten with `fetch_failed`, ward-wide.

    An empty list must therefore mean one thing only: the portal was asked and published
    nothing.
    """


# ─── Request/Response Models ───────────────────────────────────────────────────

class ScrapeRequest(BaseModel):
    url: str
    selectors: dict[str, str] = {}
    timeout: int = 30
    # Authoritative routing key — drives strategy selection.
    # Values: "idox" | "arcus" | "wfs" | "planapi"
    # Defaults to "idox" to match PlanningPortalConfig.portal_type default.
    portal_type: str = "idox"


class ScrapeResponse(BaseModel):
    status: str
    results: Optional[list[dict[str, Optional[str]]]] = None
    reason: Optional[str] = None


# ─── Idox PublicAccess Strategy ────────────────────────────────────────────────

def scrape_idox(url: str, timeout_secs: int) -> list[dict]:
    """
    Scrape an Idox PublicAccess planning portal results page.

    Primary: scrapling two-step GET form → POST ward.
    Fallback tier 1: firecrawl stealth GET → parse same CSS.
    Fallback tier 2: firecrawl browser actions → fill ward → submit.
    """
    import re as _re
    from datetime import date, timedelta
    from urllib.parse import parse_qs
    from scrapling.fetchers import Fetcher

    fetcher = Fetcher(auto_match=False)
    parsed = urlparse(url)
    base_url = _extract_base_url(url)
    path_prefix = parsed.path.rsplit("/", 1)[0]
    form_url = f"{base_url}{path_prefix}/search.do?action=advanced"
    results_url = f"{base_url}{path_prefix}/advancedSearchResults.do?action=firstPage"
    ward = parse_qs(parsed.query).get("searchCriteria.ward", [""])[0]

    today = date.today()
    date_from = (today - timedelta(days=365)).strftime("%d/%m/%Y")
    date_to = today.strftime("%d/%m/%Y")

    # ── Primary: scrapling ────────────────────────────────────────────────────
    scrapling_results = []
    scrapling_failed = False
    try:
        form_resp = fetcher.get(form_url, timeout=timeout_secs, follow_redirects=False)
        cookies = form_resp.cookies
        csrf_match = _re.search(r'name="_csrf"\s+value="([^"]+)"', form_resp.html_content)
        csrf = csrf_match.group(1) if csrf_match else ""

        page = fetcher.post(
            results_url,
            cookies=cookies,
            data={
                "_csrf": csrf,
                "searchType": "Application",
                "caseAddressType": "Application",
                "searchCriteria.ward": ward,
                "date(applicationReceivedStart)": date_from,
                "date(applicationReceivedEnd)": date_to,
            },
            timeout=timeout_secs,
            follow_redirects=False,
        )

        def _first(els):
            return els[0] if els else None

        for row in page.css("li.searchresult"):
            try:
                meta_el = _first(row.css("p.metaInfo"))
                meta_text = meta_el.text if meta_el else ""
                ref_match = _re.search(r"Ref\.\s*No:\s*([\w/\-]+)", meta_text)
                if not ref_match:
                    continue
                reference = ref_match.group(1).strip()

                desc_el = _first(row.css(".summaryLinkTextClamp"))
                description = desc_el.text.strip() if desc_el else None

                addr_el = _first(row.css("p.address"))
                address = addr_el.text.strip() if addr_el else None

                # Detail link: prefer a.summaryLink, fall back to any anchor that
                # points at an Idox applicationDetails page (theme-tolerant).
                link_el = _first(row.css("a.summaryLink")) or _first(
                    row.css("a[href*='applicationDetails']")
                )
                detail_href = link_el["href"] if link_el else None

                row_ward = None
                applicant = None
                detail_received_date = None
                # D-04: no detail link at all on the list page means there is
                # nothing to fetch — the source published no attributable detail
                # page for this application, not that a fetch failed.
                provenance = "absent_at_source"
                if detail_href:
                    time.sleep(1)
                    try:
                        # Pass the search-session cookies — Idox detail pages return
                        # an empty body without them.
                        row_ward, applicant, detail_received_date, provenance = (
                            _fetch_detail_fields(
                                fetcher, base_url, detail_href, cookies, timeout_secs
                            )
                        )
                        if applicant is None:
                            _log.warning(
                                "No applicant extracted from detail page for ref %s "
                                "(href %s) — Idox layout may have changed",
                                reference, detail_href,
                            )
                    except Exception as exc:
                        _log.warning(
                            "Detail-page fetch failed for ref %s (%s): %s",
                            reference, detail_href, exc,
                        )
                        provenance = "fetch_failed"

                # D-07: extract received date from metaInfo list-page string if not
                # already obtained from the detail page.
                received_date = detail_received_date
                if received_date is None:
                    date_match = _re.search(
                        r"\|\s*Date[^:]*:\s*(\d{2}/\d{2}/\d{4})", meta_text
                    )
                    if date_match:
                        try:
                            received_date = datetime.strptime(
                                date_match.group(1), "%d/%m/%Y"
                            ).strftime("%Y-%m-%d")
                        except ValueError:
                            received_date = None

                scrapling_results.append({
                    "reference": reference,
                    "description": description,
                    "address": address,
                    "ward": row_ward,
                    "applicant": applicant,
                    "received_date": received_date,
                    # D-04/D-06: this path does not go through _strip_internal (it
                    # never carried a "_"-prefixed key), so both keys are added
                    # directly here rather than promoted.
                    "applicant_provenance": provenance,
                    "detail_handle": detail_href,
                })
            except Exception:
                continue
    except Exception as exc:
        _log.warning("Scrapling Idox failed for %s: %s — trying firecrawl", url, exc)
        scrapling_failed = True

    if scrapling_results:
        return scrapling_results

    # ── Fallback tier 1: firecrawl GET ───────────────────────────────────────
    if _firecrawl is not None:
        if not scrapling_failed:
            _log.warning("Scrapling returned 0 results for %s — trying firecrawl GET", url)
        html = _firecrawl_get(url)
        if html:
            tier1 = _parse_idox_html(html, url=url)
            if tier1:
                tier1 = _enrich_idox_rows_via_firecrawl(tier1, base_url)
                _log.warning("Firecrawl GET succeeded for %s (%d results)", url, len(tier1))
                return tier1

        # ── Fallback tier 2: firecrawl actions ───────────────────────────────
        _log.warning("Firecrawl GET empty for %s — trying firecrawl actions", url)
        tier2 = _firecrawl_idox_actions(url)
        if tier2:
            _log.warning("Firecrawl actions succeeded for %s (%d results)", url, len(tier2))
            return tier2
        # CR-01: exhausting every fallback tier means we could not ask the portal. That is
        # a failure, not an empty result set — reporting it as `ok` with no rows let the
        # backfill stamp fetch_failed across the whole ward.
        _log.warning("All firecrawl fallbacks exhausted for %s", url)
        raise ScrapeFailed(f"all idox fallback tiers exhausted for {url}")

    # CR-01/BL-02: no fallback tier is configured (_firecrawl is None — the default).
    # `scrapling_failed` was set above and then never consulted, so a TOTAL scrapling
    # failure fell through to a bare `return []` labelled "published nothing". With no
    # Firecrawl configured that is the ordinary production path, not an edge case.
    if scrapling_failed:
        raise ScrapeFailed(
            f"scrapling failed for {url} and no firecrawl fallback is configured"
        )

    # Scrapling ran cleanly and found no rows: the portal answered and published nothing.
    return []


# Header labels are normalised (lowercase, single-spaced, trailing colon stripped)
# before comparison, so these sets cover the common Idox theme variants.
_WARD_LABELS = {"ward", "ward name", "ward / parish", "ward/parish", "wards"}
_APPLICANT_LABELS = {
    "applicant", "applicant name", "applicant name(s)", "applicant names",
    "name of applicant", "applicants",
}
_RECEIVED_DATE_LABELS = {
    "date received", "date valid", "date validated", "application date",
    "validation date", "date of receipt",
}


def _norm_label(text: Optional[str]) -> str:
    """Normalise a field label for robust matching: collapse whitespace,
    lowercase, and strip a trailing colon. 'Applicant Name :' → 'applicant name'."""
    return re.sub(r"\s+", " ", text or "").strip().rstrip(":").strip().lower()


def _clean_value(text: Optional[str]) -> Optional[str]:
    """Collapse internal whitespace/newlines in a field value; None if empty."""
    v = re.sub(r"\s+", " ", text or "").strip()
    return v or None


def _extract_detail_fields_from_selector(
    page,
) -> tuple[Optional[str], Optional[str], Optional[str]]:
    """
    Extract (Ward, Applicant Name, received_date) from an Idox application detail page.

    Robust across Idox PublicAccess themes. Earlier versions only scanned a
    table with id="applicationDetails" and matched headers with `==`, which
    silently returned (None, None) on portals that use a different table id,
    a definition-list layout, or labels like "Applicant Name :". That blinded
    the Phoenix/nepotism detectors because every applicant collapsed to
    "Unknown Applicant". This version:

      * scans every <tr><th>…</th><td>…</td></tr> on the page (any table id)
      * also scans <dl><dt>…</dt><dd>…</dd></dl> definition lists
      * normalises labels (case-insensitive, whitespace- and colon-tolerant)
      * matches a set of known label variants for Ward, Applicant, and received date
    """
    ward = None
    applicant = None
    received_date = None

    def _first(els):
        return els[0] if els else None

    def _consider(label_text, value_text):
        nonlocal ward, applicant, received_date
        label = _norm_label(label_text)
        value = _clean_value(value_text)
        if value is None:
            return
        if ward is None and label in _WARD_LABELS:
            ward = value
        elif applicant is None and label in _APPLICANT_LABELS:
            applicant = value
        elif received_date is None and label in _RECEIVED_DATE_LABELS:
            # Normalise portal date (DD/MM/YYYY) to ISO YYYY-MM-DD.
            # Degrade to None on absent or malformed input (Pitfall 7).
            try:
                received_date = datetime.strptime(value, "%d/%m/%Y").strftime("%Y-%m-%d")
            except ValueError:
                received_date = None

    # 1) Table rows anywhere on the page (th = label, td = value).
    for tr in page.css("tr"):
        th = _first(tr.css("th"))
        td = _first(tr.css("td"))
        if th and td:
            _consider(th.text, td.text)
            if ward and applicant and received_date is not None:
                return ward, applicant, received_date

    # 2) Definition lists (<dt> label, <dd> value) — some Idox themes use these.
    dts = page.css("dt")
    dds = page.css("dd")
    for dt, dd in zip(dts, dds):
        _consider(dt.text, dd.text)
        if ward and applicant and received_date is not None:
            break

    return ward, applicant, received_date


def _extract_detail_fields_from_html(
    html: str,
) -> tuple[Optional[str], Optional[str], Optional[str]]:
    """String-input wrapper around _extract_detail_fields_from_selector — used by
    the Firecrawl fallback tiers and by tests (no network call)."""
    try:
        from scrapling import Selector
        return _extract_detail_fields_from_selector(Selector(html))
    except Exception as exc:
        _log.warning("_extract_detail_fields_from_html failed: %s", exc)
        return None, None, None


# Back-compat alias: the primary scrapling path calls this with a fetched page.
def _extract_detail_fields(
    detail_page,
) -> tuple[Optional[str], Optional[str], Optional[str]]:
    return _extract_detail_fields_from_selector(detail_page)


def _extract_base_url(url: str) -> str:
    """Extract scheme + host from a URL, e.g. https://planning.anglesey.gov.wales"""
    parsed = urlparse(url)
    return f"{parsed.scheme}://{parsed.netloc}"


# Idox PublicAccess stores Applicant Name / Agent / Ward on the "details" tab
# (activeTab=details). The search-result summaryLink points at activeTab=summary,
# which only carries Reference/Address/Proposal/Status. Older code targeted
# "furtherInformation", which is empty on some portals (e.g. Isle of Wight) —
# that, plus a missing session cookie, is why every applicant came back blank.
# We try "details" first and fall back to "furtherInformation" for themes that
# use it.
_DETAIL_TABS = ("details", "furtherInformation")

# Maximum number of retries when the portal responds with HTTP 429 (rate limit).
# On each retry the delay doubles: 30s, 60s, 120s. After _RATE_LIMIT_MAX_RETRIES
# attempts the fetch is abandoned and (None, None) is returned so the application
# is recorded with applicant=None rather than silently corrupting attribution.
_RATE_LIMIT_INITIAL_BACKOFF_SECS = 30
_RATE_LIMIT_MAX_RETRIES = 3

# CR-08: total backoff budget for ONE record, shared across every tab in _DETAIL_TABS.
#
# Per-tab the ladder is 30+60+120 = 210s, and _DETAIL_TABS has two entries, so a single
# rate-limited row could sleep 420s — past the Rust client's 300s ceiling. The client then
# reports a Transport failure for a request the sidecar is still working on, which reads
# downstream as a failed ward. Capping the budget keeps the worst case inside the client
# timeout; exhausting it abandons the remaining retries exactly as the retry ladder does.
_RATE_LIMIT_TOTAL_BACKOFF_BUDGET_SECS = 150


def _detail_url(base_url: str, detail_href: str, tab: str = "details") -> str:
    """Build an Idox detail-page URL on the given tab. Handles absolute/relative
    hrefs and uses the correct query separator so activeTab is never malformed."""
    abs_url = detail_href if detail_href.startswith("http") else f"{base_url}{detail_href}"
    if "activeTab=" in abs_url:
        return re.sub(r"activeTab=[^&]*", f"activeTab={tab}", abs_url)
    sep = "&" if "?" in abs_url else "?"
    return f"{abs_url}{sep}activeTab={tab}"


def _fetch_detail_fields(
    fetcher, base_url, detail_href, cookies, timeout_secs
) -> tuple[Optional[str], Optional[str], Optional[str], str]:
    """Fetch an Idox application detail page and extract
    (ward, applicant, received_date, provenance).

    Idox detail pages return an empty <html></html> body unless the request
    carries the JSESSIONID established by the search flow, so the session
    `cookies` MUST be passed through. Tries the 'details' tab first, then
    'furtherInformation', stopping as soon as an applicant is found.

    HTTP 429 (rate limit) handling: when the portal returns 429 the page HTML
    contains no application data. The function detects this via `page.status`,
    backs off with exponential delay (30s, 60s, 120s), and retries up to
    _RATE_LIMIT_MAX_RETRIES times per tab. If all retries are exhausted, the
    tab is skipped (applicant remains None). This prevents the old behaviour
    of silently attributing rate-limited applications to director:unknown-applicant.

    D-04 (parity with Arcus's D-03 three-state contract): the 4th return value,
    `provenance`, is tracked across the _DETAIL_TABS loop by whether ANY tab
    attempt ever returned a non-None `page` (i.e. was not exhausted by
    rate-limit retries):
      - no tab ever returned a page -> "fetch_failed" (every attempt was
        rate-limited/exhausted — this is exactly the case this function was
        originally written to stop silently mis-attributing)
      - a page was fetched but `applicant` is still None -> "absent_at_source"
      - a page was fetched and `applicant` was found -> "published"
    """
    ward = None
    applicant = None
    received_date = None
    any_page_fetched = False
    # CR-08: shared across tabs, not reset per tab — see the constant's note.
    backoff_budget = _RATE_LIMIT_TOTAL_BACKOFF_BUDGET_SECS
    for tab in _DETAIL_TABS:
        url = _detail_url(base_url, detail_href, tab=tab)
        # PS-02: the detail href came from the portal's own HTML, so it is untrusted input
        # on the same footing as `request.url` — a hostile or compromised portal could point
        # it at an internal address, and `/scrape`'s guard only covered the entry URL.
        # Only off-host hrefs need re-checking: a same-host URL was already cleared when the
        # entry URL was validated, and re-resolving it per detail page would add a DNS
        # lookup to every row for no security gain.
        if not _same_host(url, base_url):
            try:
                _validate_scrape_url(url)
            except ScrapeFailed as exc:
                _log.warning("refusing off-host detail url %s: %s", url, exc)
                continue
        page = None
        backoff = _RATE_LIMIT_INITIAL_BACKOFF_SECS
        for attempt in range(_RATE_LIMIT_MAX_RETRIES + 1):
            page = fetcher.get(url, cookies=cookies, timeout=timeout_secs, follow_redirects=False)
            http_status = getattr(page, "status", 200)
            if http_status == 429:
                if attempt < _RATE_LIMIT_MAX_RETRIES and backoff_budget > 0:
                    sleep_for = min(backoff, backoff_budget)
                    _log.warning(
                        "HTTP 429 from detail page %s (attempt %d/%d) — backing off %ds "
                        "(%ds of budget left)",
                        url, attempt + 1, _RATE_LIMIT_MAX_RETRIES, sleep_for, backoff_budget,
                    )
                    time.sleep(sleep_for)
                    backoff_budget -= sleep_for
                    backoff *= 2
                else:
                    _log.warning(
                        "HTTP 429 from detail page %s — all %d retries exhausted, skipping tab",
                        url, _RATE_LIMIT_MAX_RETRIES,
                    )
                    page = None
                continue
            # Non-429: accept the page (even non-200 may contain partial data on
            # some Idox themes; extractor returns None safely if fields are absent).
            break
        if page is None:
            # All retries exhausted for this tab — try the next tab.
            continue
        any_page_fetched = True
        w, a, rd = _extract_detail_fields(page)
        ward = ward or w
        applicant = applicant or a
        received_date = received_date or rd
        if applicant:
            break
    if not any_page_fetched:
        provenance = "fetch_failed"
    elif applicant:
        provenance = "published"
    else:
        provenance = "absent_at_source"
    return ward, applicant, received_date, provenance


# ─── Generic CSS selector strategy (for non-Idox portals) ─────────────────────

def scrape_static_css(url: str, selectors: dict[str, str], timeout_secs: int) -> list[dict]:
    """
    CSS selector-based scraping for non-Idox portals or unknown portal types.
    Uses the selectors dict from the request body.
    """
    from scrapling.fetchers import Fetcher
    try:
        fetcher = Fetcher(auto_match=False)
        page = fetcher.get(url, timeout=timeout_secs, follow_redirects=False)
        container_sel = selectors.get("container", "li.searchresult")
        results = []
        for item in page.css(container_sel):
            def safe_text(sel: str) -> Optional[str]:
                if not sel:
                    return None
                el = item.css_first(sel)
                return el.text.strip() if el else None
            results.append({
                "reference": safe_text(selectors.get("reference", "p.metaInfo")),
                "description": safe_text(selectors.get("description", ".summaryLinkTextClamp")),
                "address": safe_text(selectors.get("address", "p.address")),
                "ward": safe_text(selectors.get("ward", "")),
                "applicant": safe_text(selectors.get("applicant", "")),
            })
        return results
    except Exception as exc:
        # CR-01: a parse/fetch failure here is not an empty ward.
        _log.warning("static-css scrape failed for %s: %s", url, exc)
        raise ScrapeFailed(f"static-css scrape failed: {exc}") from exc


# ─── Swift APAS / SwiftLG APAS portal strategy ────────────────────────────────
# Used by Dudley (w5.dudley.gov.uk/swiftlg/apas/run/) and
# Walsall (planning.walsall.gov.uk/swift/apas/run/).
# The config URL carries the ward name as a ?ward= query parameter:
#   https://planning.walsall.gov.uk/swift/apas/run/wphappcriteria.display?ward=Blakenall
# The scraper extracts the ward name, POSTs the search form, and then fetches
# detail pages for ward/applicant using the same _extract_detail_fields_from_selector
# helper as the Idox strategy (APAS detail pages use similar table/DL layouts).


def _parse_swift_apas_html(html: str) -> list[dict]:
    """Parse Swift APAS / SwiftLG APAS planning search results from HTML.

    APAS results tables use alternating tr.normalRow / tr.alternateRow rows.
    Columns are typically: Application Ref (link) | Location | Proposal |
    Status | Date Received.  Ward and Applicant are not in the list view —
    they require a separate detail-page fetch.
    """
    def _first(els):
        return els[0] if els else None

    try:
        from scrapling import Selector
        page = Selector(html)
        results = []
        for row in page.css("tr.normalRow, tr.alternateRow"):
            try:
                cells = row.css("td")
                if len(cells) < 2:
                    continue
                ref_link = _first(cells[0].css("a"))
                reference = (ref_link.text.strip() if ref_link
                             else cells[0].text.strip())
                if not reference:
                    continue
                detail_href = ref_link["href"] if ref_link else None
                address = cells[1].text.strip() if len(cells) > 1 else None
                description = cells[2].text.strip() if len(cells) > 2 else None
                results.append({
                    "reference": reference,
                    "description": description,
                    "address": address,
                    "ward": None,
                    "applicant": None,
                    "_detail_href": detail_href,
                })
            except Exception:
                continue
        return results
    except Exception as exc:
        _log.warning("_parse_swift_apas_html failed: %s", exc)
        return []


def _fetch_swift_apas_detail(
    fetcher, base_url: str, detail_href: str, timeout_secs: int
) -> tuple[Optional[str], Optional[str], Optional[str]]:
    """Fetch a Swift APAS detail page and return (ward, applicant, received_date).

    APAS detail pages do not require session cookies — a plain GET suffices.
    The key-value layout (table <th>/<td> or <dl>/<dt>/<dd>) is handled by
    the shared _extract_detail_fields_from_selector helper.
    """
    if not detail_href:
        return None, None, None
    url = (detail_href if detail_href.startswith("http")
           else f"{base_url}{detail_href}")
    # PS-02: same untrusted-href reasoning as `_fetch_detail_fields`.
    if not _same_host(url, base_url):
        try:
            _validate_scrape_url(url)
        except ScrapeFailed as exc:
            _log.warning("refusing off-host APAS detail url %s: %s", url, exc)
            return None, None, None
    try:
        page = fetcher.get(url, timeout=timeout_secs, follow_redirects=False)
        return _extract_detail_fields_from_selector(page)
    except Exception as exc:
        _log.warning("Swift APAS detail fetch failed for %s: %s", url, exc)
        return None, None, None


def scrape_swift_apas(url: str, timeout_secs: int) -> list[dict]:
    """Scrape a SwiftLG or Swift APAS planning portal.

    Used by Dudley (SwiftLG APAS) and Walsall (Swift APAS).  The ward name
    is extracted from the ``?ward=WARD_NAME`` query parameter in the config URL.

    Search strategy:
      1. POST to wphappcriteria.display with ward name + 12-month date window.
      2. Parse alternating normalRow/alternateRow table rows for references.
      3. Fetch each application's detail page for ward + applicant names.
      4. Return [] on any failure (preserving graceful-degradation invariant).
    """
    from urllib.parse import parse_qs, urlparse
    from datetime import date, timedelta
    from scrapling.fetchers import Fetcher

    fetcher = Fetcher(auto_match=False)
    parsed = urlparse(url)
    base_url = f"{parsed.scheme}://{parsed.netloc}"
    ward = parse_qs(parsed.query).get("ward", [""])[0]
    search_url = f"{base_url}{parsed.path}"

    today = date.today()
    date_from = (today - timedelta(days=365)).strftime("%d/%m/%Y")
    date_to = today.strftime("%d/%m/%Y")

    try:
        resp = fetcher.post(
            search_url,
            data={
                "ward":        ward,
                "action":      "2",
                "appType":     "",
                "applicant":   "",
                "development": "",
                "dateFrom":    date_from,
                "dateTo":      date_to,
            },
            timeout=timeout_secs,
            follow_redirects=False,
        )
        rows = _parse_swift_apas_html(resp.html_content)
    except Exception as exc:
        _log.warning(
            "Swift APAS search POST failed for %s: %s — returning empty", url, exc
        )
        return []

    if not rows:
        return rows

    enriched = []
    for r in rows:
        time.sleep(0.5)  # polite per-request delay
        href = r.pop("_detail_href", None)
        if href:
            r_ward, applicant, received_date = _fetch_swift_apas_detail(
                fetcher, base_url, href, timeout_secs
            )
            r["ward"] = r_ward or ward or None
            if received_date and not r.get("received_date"):
                r["received_date"] = received_date
            if applicant:
                r["applicant"] = applicant
            else:
                _log.warning(
                    "No applicant extracted from APAS detail for ref %s (%s)",
                    r.get("reference"), href,
                )
        else:
            r["ward"] = ward or None
        enriched.append(r)

    return enriched


# ─── Northgate PlanningExplorer stub ──────────────────────────────────────────

def scrape_northgate_stub(url: str, selectors: dict, timeout_secs: int) -> list[dict]:
    """Northgate PlanningExplorer portals (Birmingham) — implementation pending.

    Birmingham City Council (E08000025) uses Northgate PlanningExplorer at
    https://eplanning.birmingham.gov.uk/Northgate/PlanningExplorer/
    This is a POST-form .aspx system distinct from Idox; ward search requires
    browser-session state (ViewState + anti-forgery tokens).
    Implement via Playwright or Firecrawl browser actions when Birmingham is
    prioritised (101 wards — highest-value single addition for West Midlands coverage).
    """
    _log.warning(
        "northgate portal not yet implemented (Birmingham) — refusing %s", url
    )
    # CR-01: an unimplemented portal type has not measured anything. Reporting `ok` with
    # no rows told the backfill this ward genuinely publishes nothing.
    raise ScrapeFailed("portal_type 'northgate' not implemented")


# ─── Stub strategies for future portal types ──────────────────────────────────

# ─── Arcus Global (Salesforce Experience Cloud) ───────────────────────────────
# Portal family used by e.g. Isle of Anglesey (ioacc.my.site.com). Not JS-scraped:
# the search is a guest Salesforce Aura action returning JSON, so a plain HTTP
# GET (for the framework bootstrap ids) + POST (the query) suffices — no browser.
# Full wire contract: the Anglesey planning-portal DNS investigation notes.

_ARCUS_QUERY_ACTION = "apex://PR_SearchCont/ACTION$query"
_ARCUS_APP_KEY = "APPLICATION@markup://siteforce:communityApp"
_ARCUS_GETRECORD_ACTION = (
    "serviceComponent://ui.force.components.controllers."
    "recordGlobalValueProvider.RecordGvpController/ACTION$getRecord"
)
# Object-level layout id for arcusbuilt__PApplication__c. FULL mode returns every
# field regardless of layout, so this only has to be a valid layout id for the object.
_ARCUS_PAPP_LAYOUT = "01258000000LvYYAA0"
# How many records to fetch per batched getRecord POST (the aura `actions` array).
_ARCUS_DETAIL_BATCH = 50
# Detail field API names on arcusbuilt__PApplication__c.
_ARCUS_F_APPLICANT = "Applicant_Name_Formula__c"
_ARCUS_F_AGENT = "Agent_Name_Formula__c"
_ARCUS_F_RECEIVED = "arcusbuilt__ReceivedDate__c"


def _arcus_ward_from_url(url: str) -> str:
    """The config URL carries the portal ward name in ?ward= (URL-encoded).

    Keeps the PlanningPortalConfig{url, portal_type} contract intact — no extra
    field needed to route the ward through to the Aura search_criteria.
    """
    from urllib.parse import parse_qs, urlparse
    return parse_qs(urlparse(url).query).get("ward", [""])[0]


def _extract_aura_bootstrap(html: str):
    """Pull the Aura framework uid (fwuid) and app uid from a community page.

    Both live URL-encoded inside an inline aura-config blob; decode first, then
    regex. fwuid rotates on every Salesforce release, so it MUST be scraped per
    run — never hardcoded. Returns (None, None) if absent.
    """
    from urllib.parse import unquote
    decoded = unquote(html)
    fwuid_m = re.search(r'"fwuid":"([^"]+)"', decoded)
    app_m = re.search(
        r'"APPLICATION@markup://siteforce:communityApp":"([^"]+)"', decoded
    )
    return (
        fwuid_m.group(1) if fwuid_m else None,
        app_m.group(1) if app_m else None,
    )


def _arcus_getrecord_body(ids: list, fwuid: str, app_uid: str) -> dict:
    """Build a batched Lightning-Data-Service getRecord POST for N record ids.

    The list query (PR_SearchCont.query) returns no applicant/agent — those live on
    the full record, fetched via the standard guest RecordGvpController.getRecord in
    FULL mode. Batching many actions into one POST keeps ~40k records to ~800 requests.
    """
    import json
    actions = [
        {
            "id": f"{i};a",
            "descriptor": _ARCUS_GETRECORD_ACTION,
            "callingDescriptor": "UNKNOWN",
            "params": {
                "recordDescriptor": (
                    f"{rid}.{_ARCUS_PAPP_LAYOUT}.FULL.null.null.null.VIEW.true.null.Name.null"
                )
            },
        }
        for i, rid in enumerate(ids)
    ]
    context = {
        "mode": "PROD",
        "fwuid": fwuid,
        "app": "siteforce:communityApp",
        "loaded": {_ARCUS_APP_KEY: app_uid},
    }
    return {
        "message": json.dumps({"actions": actions}),
        "aura.context": json.dumps(context),
        "aura.pageURI": "/s/pr-english",
        "aura.token": "null",
    }


def _parse_arcus_details(resp_text: str) -> dict:
    """Parse a batched getRecord response into
    {record_id: {applicant, agent, received_date, provenance}}.

    getRecord returns data via the $Record global value provider (action.returnValue is
    null), under context.globalValueProviders[*].values.records.<id>.<obj>.record.fields.
    Degrades to {} on malformed input; missing fields degrade to None.

    D-03: every record that IS present here (i.e. this response's $Record provider
    successfully returned it) carries an explicit "provenance": "published" (a
    non-null applicant field) or "absent_at_source" (the record parsed cleanly but
    the applicant field itself is null/absent — the source genuinely publishes
    nothing for this application). A record id that never appears in the output
    dict at all (this response's batch didn't cover it, e.g. because the id was
    invalid or the whole batch failed) is NOT this function's concern — that is
    _arcus_fetch_details's "fetch_failed" case, attributed by the caller.
    """
    import json
    out: dict = {}
    try:
        gvps = json.loads(resp_text).get("context", {}).get("globalValueProviders", [])
    except (ValueError, AttributeError):
        return {}
    records = None
    for g in gvps:
        vals = g.get("values") if isinstance(g, dict) else None
        if isinstance(vals, dict) and isinstance(vals.get("records"), dict):
            records = vals["records"]
            break
    if not records:
        return {}

    def _val(fields: dict, key: str):
        f = fields.get(key)
        if isinstance(f, dict):
            return f.get("value")
        return f

    for rid, wrapper in records.items():
        if not isinstance(wrapper, dict):
            continue
        # wrapper is keyed by the sobject api name (e.g. arcusbuilt__PApplication__c)
        fields = {}
        for obj in wrapper.values():
            if isinstance(obj, dict) and isinstance(obj.get("record"), dict):
                fields = obj["record"].get("fields", {}) or {}
                break
        applicant = _val(fields, _ARCUS_F_APPLICANT)
        out[rid] = {
            "applicant": applicant,
            "agent": _val(fields, _ARCUS_F_AGENT),
            "received_date": _val(fields, _ARCUS_F_RECEIVED),
            "provenance": "published" if applicant else "absent_at_source",
        }
    return out


def _build_arcus_query_body(fwuid: str, app_uid: str, category: str, ward: str) -> dict:
    """Build the four form fields for the Aura PR_SearchCont.query POST.

    search_criteria is mandatory — an empty criteria (all applications) trips the
    Apex heap governor limit server-side, so every query is ward-scoped.
    """
    import json
    message = {
        "actions": [{
            "id": "1;a",
            "descriptor": _ARCUS_QUERY_ACTION,
            "callingDescriptor": "markup://c:PR_Search",
            "params": {
                "searchable_resources": "be_searchables",
                "resource_name": "be_adv_categories",
                "category_name": category,
                "search_criteria": {"Ward__c": ward},
            },
        }]
    }
    context = {
        "mode": "PROD",
        "fwuid": fwuid,
        "app": "siteforce:communityApp",
        "loaded": {_ARCUS_APP_KEY: app_uid},
    }
    return {
        "message": json.dumps(message),
        "aura.context": json.dumps(context),
        "aura.pageURI": "/s/pr-english",
        "aura.token": "null",  # guest community — literal string, no auth
    }


def _parse_arcus_response(resp_text: str, ward: str) -> list[dict]:
    """Parse the (double-encoded) Aura envelope into planning-item dicts.

    returnValue is itself a JSON string → parse twice.

    CR-01/BL-01: raises ScrapeFailed on a server-side ERROR state (e.g. heap limit) or any
    malformed input. It used to degrade to [] for both, which the caller could not tell
    apart from a ward that genuinely has no applications — so a Salesforce error state was
    reported to Rust as a successful empty ward. [] now means exactly one thing: the action
    succeeded and returned no records.

    ward is supplied by the caller (the record projection carries no ward field).
    """
    import json
    try:
        action = json.loads(resp_text)["actions"][0]
        if action.get("state") != "SUCCESS":
            _log.warning("arcus: action state=%s (not SUCCESS)", action.get("state"))
            raise ScrapeFailed(f"arcus action state={action.get('state')} (not SUCCESS)")
        records = json.loads(action["returnValue"])["records"]
    except (KeyError, IndexError, TypeError, ValueError) as exc:
        _log.warning("arcus: failed to parse response: %s", exc)
        raise ScrapeFailed(f"failed to parse arcus response: {exc}") from exc

    results = []
    for rec in records:
        reference = rec.get("Name")
        if not reference:
            continue  # mapper requires a reference
        results.append({
            "reference": reference,
            "description": rec.get("arcusbuilt__Proposal__c"),
            "address": rec.get("Site_Address_Free_Text__c"),
            "ward": ward or None,
            "applicant": None,
            "received_date": None,
            # Internal: Salesforce record id, used to fetch applicant/date from the
            # detail record (getRecord). Stripped by _strip_internal before return.
            "_sf_id": rec.get("Id"),
        })
    return results


def scrape_arcus(url: str, selectors: dict, timeout_secs: int) -> list[dict]:
    """Scrape an Arcus Global (Salesforce Experience Cloud) planning register.

    No browser: GET the register page for the Aura bootstrap ids, POST the guest
    search action scoped to the config ward, then batch-fetch each record's detail
    (getRecord) to fill applicant/agent/received_date — the list projection omits them.

    Raises ScrapeFailed if the portal could not be asked (CR-01). Returns [] only when
    the portal answered and published nothing — the caller relies on that distinction to
    decide between leaving provenance untouched and stamping fetch_failed.
    """
    from scrapling.fetchers import Fetcher

    ward = _arcus_ward_from_url(url)
    if not ward:
        # CR-01/BL-01: without a ward we cannot ask the portal anything. That is a failure
        # to measure, not a ward that published nothing.
        _log.warning("arcus: no ?ward= in url: %s", url)
        raise ScrapeFailed(f"no ?ward= in arcus url: {url}")

    base_url = _extract_base_url(url)
    aura_url = f"{base_url}/s/sfsites/aura"
    fetcher = Fetcher(auto_match=False)

    try:
        page = fetcher.get(
            f"{base_url}/s/pr-english?language=en_GB",
            timeout=timeout_secs,
            follow_redirects=False,
        )
        fwuid, app_uid = _extract_aura_bootstrap(page.html_content)
        if not (fwuid and app_uid):
            # CR-01/BL-01: this IS the rotated-fwuid case the class docstring calls the
            # single most likely real-world failure — Salesforce rotates fwuid on every
            # release. It previously returned [] here without raising, so the most likely
            # failure in production still reported a successful empty ward and let the
            # backfill stamp fetch_failed across it. Every configured portal is arcus.
            _log.warning("arcus: could not extract aura bootstrap from %s", base_url)
            raise ScrapeFailed(f"could not extract aura bootstrap from {base_url}")

        body = _build_arcus_query_body(fwuid, app_uid, "PApplication", ward)
        resp = fetcher.post(
            f"{aura_url}?r=1&other.PR_SearchCont.query=1",
            data=body,
            timeout=timeout_secs,
            follow_redirects=False,
        )
        # .body is the raw response bytes; .html_content would mangle the JSON.
        items = _parse_arcus_response(resp.body.decode("utf-8", "replace"), ward)

        # Detail pass: batch getRecord to fill applicant/agent/received_date.
        ids = [it["_sf_id"] for it in items if it.get("_sf_id")]
        details = _arcus_fetch_details(fetcher, aura_url, ids, fwuid, app_uid, timeout_secs)
        for it in items:
            d = details.get(it.get("_sf_id"))
            if d:
                it["applicant"] = d.get("applicant") or it["applicant"]
                it["agent"] = d.get("agent")
                it["received_date"] = d.get("received_date") or it["received_date"]
            # D-03: an item whose _sf_id never produced a detail-dict entry at all
            # (no id extractable, or — should not happen given _arcus_fetch_details's
            # full-coverage guarantee, but guard anyway — its id was never attempted)
            # degrades to fetch_failed, never a silently-missing key.
            it["applicant_provenance"] = d.get("provenance") if d else "fetch_failed"
        return _strip_internal(items)
    except ScrapeFailed:
        raise
    except Exception as exc:
        # CR-01: was `return []`, which the endpoint reported as a successful empty ward.
        # Network errors inside fetcher.get/post land here. The rotated-fwuid, missing
        # bootstrap and Salesforce-ERROR-state cases raise ScrapeFailed directly at their
        # own sites above (they never reached this handler, which is why the first pass at
        # this fix missed them). None of them mean "this ward has no applications".
        _log.warning("arcus scrape failed for %s: %s", url, exc)
        raise ScrapeFailed(f"arcus scrape failed: {exc}") from exc


def _arcus_fetch_details(fetcher, aura_url: str, ids: list, fwuid: str, app_uid: str,
                         timeout_secs: int) -> dict:
    """Batch-fetch record details, returning
    {id: {applicant, agent, received_date, provenance}} for every id in `ids`.

    D-03 investigation (Open Question 4, the phase 24 research notes, LOW confidence flagged):
    this session could not reach the live sidecar (`curl :8765/health` refused —
    the process was not running in this environment) to inspect whether the Aura
    `getRecord` batch response exposes a documented, reliable per-id/per-action
    `state` field analogous to the single-action `actions[0].state` that
    `_parse_arcus_response` already reads for the list query. Branch (b) — no
    per-id state, the documented fallback/expected default — is implemented here.

    Failure signal used (deliberately observable, not dependent on unverified Aura
    per-action semantics): a chunk is treated as failed if the POST/parse raised an
    exception, OR if any id in the attempted chunk is missing from the parsed
    result (this covers a non-SUCCESS top-level `actions[0].state` too, since
    `_parse_arcus_details` degrades to `{}` — no ids covered — on any response that
    isn't a clean $Record payload). On failure, if `len(chunk) > 1` the chunk is
    bisected and each half is retried independently (same POST + parse logic)
    until every surviving sub-chunk either succeeds or has shrunk to size 1. A
    size-1 chunk that still fails is the ONLY case that writes
    `provenance: "fetch_failed"` — this bounds worst-case extra HTTP calls to
    O(log2(_ARCUS_DETAIL_BATCH)) per failing batch (currently
    _ARCUS_DETAIL_BATCH = 50, so at most ~6 extra calls), and guarantees every id
    in the input `ids` list ends up with an explicit entry in the returned dict —
    never silently absent.
    """
    out: dict = {}

    def _attempt(chunk: list) -> None:
        try:
            body = _arcus_getrecord_body(chunk, fwuid, app_uid)
            resp = fetcher.post(
                f"{aura_url}?r=1&other.RecordGvp.getRecord={len(chunk)}",
                data=body,
                timeout=timeout_secs,
                follow_redirects=False,
            )
            parsed = _parse_arcus_details(resp.body.decode("utf-8", "replace"))
        except Exception as exc:
            _log.warning("arcus: detail batch failed (%d ids): %s", len(chunk), exc)
            parsed = {}

        out.update(parsed)
        missing = [rid for rid in chunk if rid not in parsed]
        if not missing:
            return
        if len(chunk) == 1:
            for rid in missing:
                out[rid] = {
                    "applicant": None, "agent": None, "received_date": None,
                    "provenance": "fetch_failed",
                }
            return
        _log.warning(
            "arcus: batch of %d ids had %d missing after fetch — bisecting",
            len(chunk), len(missing),
        )
        mid = len(chunk) // 2
        _attempt(chunk[:mid])
        _attempt(chunk[mid:])

    for i in range(0, len(ids), _ARCUS_DETAIL_BATCH):
        _attempt(ids[i:i + _ARCUS_DETAIL_BATCH])
    return out


def scrape_wfs_stub(url: str, selectors: dict, timeout_secs: int) -> list[dict]:
    """WFS (Web Feature Service) data source — e.g. Scotland Spatial Hub.

    Scotland's Spatial Hub (spatialhub.scot) provides a WFS endpoint covering
    all 34 Scottish planning authorities — far more efficient than per-council scraping.
    Implement for Scotland bulk ingestion phase.
    """
    # CR-01: not implemented is a failure to measure, not an empty result.
    raise ScrapeFailed("portal_type 'wfs' not implemented")


def scrape_planapi_stub(url: str, selectors: dict, timeout_secs: int) -> list[dict]:
    """PlanAPI commercial aggregator — TBD (requires API key).

    PlanAPI (planapi.co.uk) provides a unified REST API covering all UK LPAs.
    Implement if commercial aggregator is approved for the project.
    """
    # CR-01: not implemented is a failure to measure, not an empty result.
    raise ScrapeFailed("portal_type 'planapi' not implemented")


# ─── Endpoints ─────────────────────────────────────────────────────────────────

@app.get("/health")
async def health():
    """Health check endpoint — returns ok when sidecar is running."""
    return {"status": "ok", "service": "scrapling-sidecar"}


@app.post("/scrape", response_model=ScrapeResponse)
def scrape(request: ScrapeRequest):
    """
    Dispatch to the appropriate scraping strategy based on portal_type.

    CR-08: declared `def`, not `async def`, deliberately. Every strategy below does
    fully synchronous blocking work — `fetcher.get/post`, `time.sleep(1)` per Idox detail
    row, `time.sleep(0.5)` per APAS row, and a 429 backoff of up to 30/60/120s per tab. On
    an `async def` handler all of that runs directly on the event loop, so for the length
    of a ward re-list (measured at 157.6s) the process served nothing else: `/health`
    hung against a perfectly healthy sidecar — which `ingest_planning` checks *before*
    scraping — and no two wards could ever be scraped concurrently. A sync handler is run
    by FastAPI in a threadpool, leaving the loop free.

    portal_type is the authoritative routing key — the Rust engine sets this
    from PlanningPortalConfig.portal_type in config.toml. Using portal_type
    (not URL pattern matching) ensures routing survives URL restructuring from
    council reorganisations.
    """
    strategy = request.portal_type
    try:
        _validate_scrape_url(request.url)
        if strategy == "idox":
            results = scrape_idox(request.url, request.timeout)
        elif strategy == "swift_apas":
            results = scrape_swift_apas(request.url, request.timeout)
        elif strategy == "northgate":
            results = scrape_northgate_stub(request.url, request.selectors, request.timeout)
        elif strategy == "arcus":
            results = scrape_arcus(request.url, request.selectors, request.timeout)
        elif strategy == "wfs":
            results = scrape_wfs_stub(request.url, request.selectors, request.timeout)
        elif strategy == "planapi":
            results = scrape_planapi_stub(request.url, request.selectors, request.timeout)
        else:
            # Fallback: generic CSS selector strategy for unknown portal types
            results = scrape_static_css(request.url, request.selectors, request.timeout)
        return ScrapeResponse(status="ok", results=results)
    except ScrapeFailed as e:
        # CR-01: an explicit "could not ask the portal". Rust maps status="error" to
        # ScrapeOutcome::Failed -> WardAction::SkipUnmeasured, which leaves provenance
        # untouched instead of stamping fetch_failed across the ward.
        _log.warning("scrape failed for %s (%s): %s", request.url, strategy, e)
        return ScrapeResponse(status="error", results=[], reason=str(e))
    except Exception as e:
        # Never let the sidecar 500 — return structured error so Rust can log/skip
        return ScrapeResponse(status="error", results=[], reason=str(e))


if __name__ == "__main__":
    port = int(os.environ.get("SIDECAR_PORT", "8765"))
    # CR-07: default to loopback. The service is documented as a
    # `localhost:8765` process, but `host="0.0.0.0"` published it on every interface —
    # exposing an unauthenticated fetch proxy to the whole network. Override with
    # SIDECAR_HOST only for a deployment that genuinely needs it, alongside
    # SIDECAR_ALLOWED_HOSTS.
    host = os.environ.get("SIDECAR_HOST", "127.0.0.1")
    uvicorn.run(app, host=host, port=port, log_level="info")
