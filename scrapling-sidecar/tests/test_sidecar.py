import os
import sys
from unittest.mock import MagicMock, patch

import sidecar as s


def test_firecrawl_client_none_when_key_absent(monkeypatch):
    """_firecrawl is None when FIRECRAWL_API_KEY is not set."""
    monkeypatch.delenv("FIRECRAWL_API_KEY", raising=False)
    if "sidecar" in sys.modules:
        del sys.modules["sidecar"]
    import sidecar as s
    assert s._firecrawl is None


def test_firecrawl_client_initialized_when_key_present(monkeypatch):
    """_firecrawl is a FirecrawlApp instance when FIRECRAWL_API_KEY is set."""
    monkeypatch.setenv("FIRECRAWL_API_KEY", "fc-testkey")
    if "sidecar" in sys.modules:
        del sys.modules["sidecar"]
    import sidecar as s
    assert s._firecrawl is not None


def test_firecrawl_get_returns_html_on_success():
    if "sidecar" in sys.modules:
        del sys.modules["sidecar"]
    import sidecar as s

    mock_resp = MagicMock()
    mock_resp.html = "<html><body>test</body></html>"
    mock_app = MagicMock()
    mock_app.scrape_url.return_value = mock_resp

    original = s._firecrawl
    s._firecrawl = mock_app
    try:
        result = s._firecrawl_get("https://example.com")
    finally:
        s._firecrawl = original

    assert result == "<html><body>test</body></html>"


def test_firecrawl_get_returns_none_when_client_absent():
    if "sidecar" in sys.modules:
        del sys.modules["sidecar"]
    import sidecar as s

    original = s._firecrawl
    s._firecrawl = None
    try:
        result = s._firecrawl_get("https://example.com")
    finally:
        s._firecrawl = original

    assert result is None


def test_firecrawl_get_returns_none_on_api_error():
    if "sidecar" in sys.modules:
        del sys.modules["sidecar"]
    import sidecar as s

    mock_app = MagicMock()
    mock_app.scrape_url.side_effect = Exception("API error")

    original = s._firecrawl
    s._firecrawl = mock_app
    try:
        result = s._firecrawl_get("https://example.com")
    finally:
        s._firecrawl = original

    assert result is None


IDOX_FIXTURE_HTML = """
<ul>
  <li class="searchresult">
    <p class="metaInfo">Ref. No: FPL/2024/00100 | Date: 01/01/2024</p>
    <span class="summaryLinkTextClamp">Erection of warehouse unit</span>
    <p class="address">1 High Street, Aethwy LL59 5AA</p>
  </li>
  <li class="searchresult">
    <p class="metaInfo">Ref. No: FPL/2024/00200 | Date: 02/01/2024</p>
    <span class="summaryLinkTextClamp">Change of use to residential</span>
    <p class="address">22 Mill Road, Aethwy LL60 6BB</p>
  </li>
</ul>
"""


def test_parse_idox_html_extracts_results():
    if "sidecar" in sys.modules:
        del sys.modules["sidecar"]
    import sidecar as s

    results = s._parse_idox_html(IDOX_FIXTURE_HTML, url="https://example.com")
    assert len(results) == 2
    assert results[0]["reference"] == "FPL/2024/00100"
    assert results[0]["description"] == "Erection of warehouse unit"
    assert results[0]["address"] == "1 High Street, Aethwy LL59 5AA"
    assert results[1]["reference"] == "FPL/2024/00200"


def test_parse_idox_html_returns_empty_on_no_results():
    if "sidecar" in sys.modules:
        del sys.modules["sidecar"]
    import sidecar as s

    results = s._parse_idox_html("<html><body><p>Nothing here</p></body></html>", url="https://example.com")
    assert results == []


def test_parse_idox_html_skips_rows_without_reference():
    if "sidecar" in sys.modules:
        del sys.modules["sidecar"]
    import sidecar as s

    html = """
    <ul>
      <li class="searchresult">
        <p class="metaInfo">No reference here</p>
        <span class="summaryLinkTextClamp">Description</span>
      </li>
    </ul>
    """
    results = s._parse_idox_html(html, url="https://example.com")
    assert results == []


def test_firecrawl_idox_actions_returns_parsed_results():
    if "sidecar" in sys.modules:
        del sys.modules["sidecar"]

    mock_resp = MagicMock()
    mock_resp.html = IDOX_FIXTURE_HTML  # already defined in the test file
    mock_app = MagicMock()
    mock_app.scrape_url.return_value = mock_resp

    with patch("sidecar._firecrawl", mock_app):
        import sidecar as s_local
        url = "https://example.com/online-applications/advancedSearchResults.do?action=firstPage&searchCriteria.ward=AETHWY"
        results = s_local._firecrawl_idox_actions(url)

    assert len(results) == 2
    assert results[0]["reference"] == "FPL/2024/00100"
    call_kwargs = mock_app.scrape_url.call_args
    assert "actions" in call_kwargs.kwargs
    assert isinstance(call_kwargs.kwargs["actions"], list)
    assert len(call_kwargs.kwargs["actions"]) > 0


def test_firecrawl_idox_actions_returns_empty_when_client_absent():
    with patch("sidecar._firecrawl", None):
        if "sidecar" in sys.modules:
            del sys.modules["sidecar"]
        import sidecar as s_local
        result = s_local._firecrawl_idox_actions("https://example.com")
    assert result == []


def test_firecrawl_idox_actions_returns_empty_on_api_error():
    mock_app = MagicMock()
    mock_app.scrape_url.side_effect = Exception("browser timeout")
    with patch("sidecar._firecrawl", mock_app):
        if "sidecar" in sys.modules:
            del sys.modules["sidecar"]
        import sidecar as s_local
        result = s_local._firecrawl_idox_actions("https://example.com")
    assert result == []


def test_scrape_idox_falls_back_to_firecrawl_get_on_exception():
    """When scrapling raises, firecrawl GET is tried next."""
    mock_resp = MagicMock()
    mock_resp.html = IDOX_FIXTURE_HTML
    mock_app = MagicMock()
    mock_app.scrape_url.return_value = mock_resp

    # patch.object(s, ...) targets the module object s actually refers to,
    # which is robust against earlier tests reloading sys.modules["sidecar"].
    # Fetcher is imported locally inside scrape_idox — patch at source.
    with patch.object(s, "_firecrawl", mock_app), \
         patch("scrapling.fetchers.Fetcher") as mock_fetcher_cls:
        mock_fetcher_cls.return_value.get.side_effect = Exception("connection refused")
        results = s.scrape_idox(
            "https://example.com/online-applications/advancedSearchResults.do?action=firstPage&searchCriteria.ward=AETHWY",
            timeout_secs=5,
        )

    assert len(results) == 2
    assert results[0]["reference"] == "FPL/2024/00100"


def test_scrape_idox_falls_back_to_actions_when_get_empty():
    """When scrapling raises AND firecrawl GET returns nothing, actions are tried."""
    empty_resp = MagicMock()
    empty_resp.html = "<html><body></body></html>"
    actions_resp = MagicMock()
    actions_resp.html = IDOX_FIXTURE_HTML
    mock_app = MagicMock()
    # First call (GET via _firecrawl_get) → empty html, second call (actions) → fixture
    mock_app.scrape_url.side_effect = [empty_resp, actions_resp]

    with patch.object(s, "_firecrawl", mock_app), \
         patch("scrapling.fetchers.Fetcher") as mock_fetcher_cls:
        mock_fetcher_cls.return_value.get.side_effect = Exception("blocked")
        results = s.scrape_idox(
            "https://example.com/online-applications/advancedSearchResults.do?action=firstPage&searchCriteria.ward=AETHWY",
            timeout_secs=5,
        )

    assert len(results) == 2
    assert results[0]["reference"] == "FPL/2024/00100"


def test_scrape_idox_raises_when_scrapling_fails_and_no_firecrawl():
    """BL-02: a total scrapling failure with no fallback configured is a failure.

    `scrapling_failed` was set and then never consulted, so this path returned [] —
    labelled "the portal answered and published nothing". With Firecrawl unconfigured
    (the default) that is the ordinary production path, not an edge case, and it let the
    backfill stamp fetch_failed across the whole ward.
    """
    import pytest
    with patch.object(s, "_firecrawl", None), \
         patch("scrapling.fetchers.Fetcher") as mock_fetcher_cls:
        mock_fetcher_cls.return_value.get.side_effect = Exception("blocked")
        with pytest.raises(s.ScrapeFailed):
            s.scrape_idox(
                "https://example.com/online-applications/advancedSearchResults.do?action=firstPage&searchCriteria.ward=AETHWY",
                timeout_secs=5,
            )


# ─── Detail-page field extraction (the applicant-attribution fix) ──────────────

# Realistic Idox "Further Information" tab: table id varies by theme, headers
# carry trailing colons and mixed case, applicant value has surrounding whitespace.
IDOX_DETAIL_TABLE = """
<html><body>
<table id="simpleDetailsTable">
  <tbody>
    <tr><th scope="row">Reference</th><td>FPL/2024/00100</td></tr>
    <tr><th scope="row">Application Type</th><td>Full</td></tr>
    <tr><th scope="row">Ward :</th><td>  Aethwy  </td></tr>
    <tr><th scope="row">Applicant Name:</th><td>
        Smith Construction Ltd
    </td></tr>
    <tr><th scope="row">Agent Name</th><td>Jones Planning</td></tr>
  </tbody>
</table>
</body></html>
"""

# Definition-list themed detail page.
IDOX_DETAIL_DL = """
<html><body>
<dl class="furtherInformation">
  <dt>Application Type</dt><dd>Householder</dd>
  <dt>WARD</dt><dd>Bembridge</dd>
  <dt>Name of Applicant</dt><dd>Acme Developments Ltd</dd>
</dl>
</body></html>
"""


def test_extract_detail_fields_table_with_colons_and_whitespace():
    ward, applicant, _ = s._extract_detail_fields_from_html(IDOX_DETAIL_TABLE)
    assert ward == "Aethwy"
    assert applicant == "Smith Construction Ltd"


def test_extract_detail_fields_definition_list_layout():
    ward, applicant, _ = s._extract_detail_fields_from_html(IDOX_DETAIL_DL)
    assert ward == "Bembridge"
    assert applicant == "Acme Developments Ltd"


def test_extract_detail_fields_missing_applicant_returns_none():
    html = """<table><tr><th>Ward</th><td>Aethwy</td></tr>
              <tr><th>Status</th><td>Pending</td></tr></table>"""
    ward, applicant, _ = s._extract_detail_fields_from_html(html)
    assert ward == "Aethwy"
    assert applicant is None


def test_extract_detail_fields_legacy_application_details_id_still_works():
    # Back-compat: the original layout (id="applicationDetails", exact headers).
    html = """<table id="applicationDetails">
        <tr><th>Ward</th><td>Aethwy</td></tr>
        <tr><th>Applicant Name</th><td>Smith Construction Ltd</td></tr>
      </table>"""
    ward, applicant, _ = s._extract_detail_fields_from_html(html)
    assert ward == "Aethwy"
    assert applicant == "Smith Construction Ltd"


def test_detail_url_rewrites_summary_to_details_tab():
    # Applicant/Ward live on the 'details' tab, not 'summary' or 'furtherInformation'.
    out = s._detail_url("https://x.gov.uk", "/online-applications/applicationDetails.do?keyVal=ABC&activeTab=summary")
    assert out == "https://x.gov.uk/online-applications/applicationDetails.do?keyVal=ABC&activeTab=details"


def test_detail_url_absolute_href_without_tab_appends_details_tab():
    out = s._detail_url("https://x.gov.uk", "https://x.gov.uk/online-applications/applicationDetails.do?keyVal=ABC")
    assert out == "https://x.gov.uk/online-applications/applicationDetails.do?keyVal=ABC&activeTab=details"


def test_detail_url_honours_explicit_tab():
    out = s._detail_url("https://x.gov.uk", "/d.do?keyVal=ABC&activeTab=summary", tab="furtherInformation")
    assert out.endswith("activeTab=furtherInformation")


def test_fetch_detail_fields_passes_cookies_and_uses_details_tab_first():
    from scrapling import Selector

    calls = []

    def fake_get(url, cookies=None, timeout=None, **_kwargs):
        calls.append((url, cookies))
        return Selector(IDOX_DETAIL_TABLE)  # details tab carries the fields

    fetcher = MagicMock()
    fetcher.get.side_effect = fake_get
    cookies = {"JSESSIONID": "sess123"}
    ward, applicant, _, provenance = s._fetch_detail_fields(
        fetcher, "https://x.gov.uk",
        "/online-applications/applicationDetails.do?keyVal=ABC&activeTab=summary",
        cookies, timeout_secs=5,
    )
    assert applicant == "Smith Construction Ltd"
    assert ward == "Aethwy"
    assert provenance == "published"
    assert calls[0][1] == cookies                  # cookies passed through
    assert "activeTab=details" in calls[0][0]       # details tab tried first
    assert len(calls) == 1                          # stopped once applicant found


def test_fetch_detail_fields_falls_back_to_further_information_tab():
    from scrapling import Selector

    seq = [Selector("<html></html>"), Selector(IDOX_DETAIL_TABLE)]  # details empty → FI has data

    def fake_get(url, cookies=None, timeout=None, **_kwargs):
        return seq.pop(0)

    fetcher = MagicMock()
    fetcher.get.side_effect = fake_get
    ward, applicant, _, provenance = s._fetch_detail_fields(
        fetcher, "https://x.gov.uk", "/d.do?keyVal=ABC&activeTab=summary",
        {"JSESSIONID": "s"}, timeout_secs=5,
    )
    assert applicant == "Smith Construction Ltd"
    assert provenance == "published"


def test_parse_idox_html_captures_detail_href_internal_key():
    html = """
    <ul><li class="searchresult">
      <p class="metaInfo">Ref. No: FPL/2024/00100</p>
      <a class="summaryLink" href="/online-applications/applicationDetails.do?keyVal=ABC&activeTab=summary">desc</a>
      <span class="summaryLinkTextClamp">Erection of warehouse</span>
      <p class="address">1 High Street LL59 5AA</p>
    </li></ul>
    """
    rows = s._parse_idox_html(html, url="https://x.gov.uk")
    assert len(rows) == 1
    assert rows[0]["_detail_href"].endswith("activeTab=summary")


def test_enrich_idox_rows_strips_internal_keys_when_firecrawl_absent():
    rows = [{"reference": "R1", "description": "d", "address": "a",
             "ward": None, "applicant": None, "_detail_href": "/x"}]
    with patch.object(s, "_firecrawl", None):
        out = s._enrich_idox_rows_via_firecrawl(rows, "https://x.gov.uk")
    assert "_detail_href" not in out[0]
    assert out[0]["applicant"] is None


def test_enrich_idox_rows_fills_applicant_via_firecrawl():
    rows = [{"reference": "R1", "description": "d", "address": "a",
             "ward": None, "applicant": None,
             "_detail_href": "/online-applications/applicationDetails.do?keyVal=ABC"}]
    mock_resp = MagicMock()
    mock_resp.html = IDOX_DETAIL_TABLE
    mock_app = MagicMock()
    mock_app.scrape_url.return_value = mock_resp
    with patch.object(s, "_firecrawl", mock_app):
        out = s._enrich_idox_rows_via_firecrawl(rows, "https://x.gov.uk")
    assert out[0]["applicant"] == "Smith Construction Ltd"
    assert out[0]["ward"] == "Aethwy"
    assert "_detail_href" not in out[0]


def test_scrape_idox_primary_path_populates_applicant_end_to_end():
    """Regression for the applicant-attribution bug: the primary scrapling path
    must carry Applicant Name from the detail page into the result row. Before
    the fix, every applicant came back None and collapsed to 'Unknown Applicant'."""
    from scrapling import Selector

    results_html = """
    <ul><li class="searchresult">
      <p class="metaInfo">Ref. No: FPL/2024/00100 | Received: 01/01/2024</p>
      <a class="summaryLink" href="/online-applications/applicationDetails.do?keyVal=ABC&activeTab=summary">link</a>
      <span class="summaryLinkTextClamp">Erection of warehouse</span>
      <p class="address">1 High Street, Aethwy LL59 5AA</p>
    </li></ul>
    """

    class FormResp:
        html_content = '<input name="_csrf" value="tok123"/>'
        cookies = {"JSESSIONID": "x"}

    def fake_get(url, cookies=None, timeout=None, **_kwargs):
        # First .get is the search form; later .get is the detail page (with cookies).
        if "applicationDetails.do" in url:
            assert "activeTab=details" in url   # details tab targeted
            assert cookies == {"JSESSIONID": "x"}  # session cookies passed through
            return Selector(IDOX_DETAIL_TABLE)
        return FormResp()

    def fake_post(url, cookies=None, data=None, timeout=None, **_kwargs):
        return Selector(results_html)

    mock_fetcher = MagicMock()
    mock_fetcher.get.side_effect = fake_get
    mock_fetcher.post.side_effect = fake_post

    with patch("scrapling.fetchers.Fetcher", return_value=mock_fetcher), \
         patch.object(s, "time") as mock_time:
        mock_time.sleep = lambda *_: None
        results = s.scrape_idox(
            "https://x.gov.uk/online-applications/advancedSearchResults.do?action=firstPage&searchCriteria.ward=AETHWY",
            timeout_secs=5,
        )

    assert len(results) == 1
    assert results[0]["reference"] == "FPL/2024/00100"
    assert results[0]["applicant"] == "Smith Construction Ltd"
    assert results[0]["ward"] == "Aethwy"
    # D-04: applicant_provenance/detail_handle are always present on Idox rows.
    assert results[0]["applicant_provenance"] == "published"
    assert results[0]["detail_handle"] == (
        "/online-applications/applicationDetails.do?keyVal=ABC&activeTab=summary"
    )


# ─── HTTP 429 rate-limit handling in _fetch_detail_fields ─────────────────────

def test_fetch_detail_fields_retries_on_429_and_succeeds():
    """When the portal returns 429 on the first attempt, backoff and retry succeeds."""
    from scrapling import Selector

    rate_limited_page = Selector("<html><body>Too Many Requests</body></html>")
    rate_limited_page.status = 429  # type: ignore[attr-defined]
    good_page = Selector(IDOX_DETAIL_TABLE)
    # good_page has no .status set — defaults to 200 via getattr(..., 200)

    call_count = [0]

    def fake_get(url, cookies=None, timeout=None, **_kwargs):
        call_count[0] += 1
        if call_count[0] == 1:
            return rate_limited_page
        return good_page

    fetcher = MagicMock()
    fetcher.get.side_effect = fake_get

    sleep_calls = []

    with patch.object(s, "time") as mock_time:
        mock_time.sleep.side_effect = lambda n: sleep_calls.append(n)
        ward, applicant, _, provenance = s._fetch_detail_fields(
            fetcher, "https://x.gov.uk",
            "/d.do?keyVal=ABC&activeTab=summary",
            {"JSESSIONID": "s"}, timeout_secs=5,
        )

    assert applicant == "Smith Construction Ltd"
    assert ward == "Aethwy"
    assert provenance == "published"
    assert len(sleep_calls) == 1                                          # backed off once
    assert sleep_calls[0] == s._RATE_LIMIT_INITIAL_BACKOFF_SECS          # correct initial delay
    assert call_count[0] == 2                                             # retried once


def test_fetch_detail_fields_returns_none_after_all_retries_exhausted():
    """When all retry attempts return 429, (None, None, None, "fetch_failed") is
    returned — not corrupt data, and an explicit fetch_failed provenance (D-04)."""
    from scrapling import Selector

    rate_limited_page = Selector("<html><body>Too Many Requests</body></html>")
    rate_limited_page.status = 429  # type: ignore[attr-defined]

    fetcher = MagicMock()
    fetcher.get.return_value = rate_limited_page

    sleep_calls = []

    with patch.object(s, "time") as mock_time:
        mock_time.sleep.side_effect = lambda n: sleep_calls.append(n)
        ward, applicant, received_date, provenance = s._fetch_detail_fields(
            fetcher, "https://x.gov.uk",
            "/d.do?keyVal=ABC&activeTab=summary",
            {"JSESSIONID": "s"}, timeout_secs=5,
        )

    assert applicant is None
    assert ward is None
    assert received_date is None
    assert provenance == "fetch_failed"
    # CR-08: the retry ladder is bounded by a per-record backoff budget shared across
    # tabs, not by attempt count alone. Unbounded, one rate-limited record could sleep
    # len(_DETAIL_TABS) * (30+60+120) = 420s — past the Rust client's 300s ceiling, so the
    # client reported a Transport failure for a request the sidecar was still working on.
    #
    # Assert the property that matters (total sleep stays within budget) rather than an
    # exact call count, which would just re-encode the ladder's shape.
    assert sum(sleep_calls) <= s._RATE_LIMIT_TOTAL_BACKOFF_BUDGET_SECS, (
        f"backoff exceeded its budget: slept {sum(sleep_calls)}s"
    )
    assert len(sleep_calls) <= len(s._DETAIL_TABS) * s._RATE_LIMIT_MAX_RETRIES
    # It must still actually retry — a budget of zero would be a different bug.
    assert sleep_calls, "no backoff attempted at all"


# D-04/D-03 parity: explicit provenance tests for _fetch_detail_fields (24-01
# Task 2 — closes the D-03 Wave-0 test gap for the two provenance edges).

def test_fetch_detail_fields_all_retries_exhausted_is_fetch_failed():
    """Every tab's retries exhausted (HTTP 429 on every attempt) -> provenance is
    explicitly "fetch_failed", never silently None/absent."""
    from scrapling import Selector

    rate_limited_page = Selector("<html><body>Too Many Requests</body></html>")
    rate_limited_page.status = 429  # type: ignore[attr-defined]

    fetcher = MagicMock()
    fetcher.get.return_value = rate_limited_page

    with patch.object(s, "time"):
        _, applicant, _, provenance = s._fetch_detail_fields(
            fetcher, "https://x.gov.uk",
            "/d.do?keyVal=ABC&activeTab=summary",
            {"JSESSIONID": "s"}, timeout_secs=5,
        )

    assert applicant is None
    assert provenance == "fetch_failed"


def test_fetch_detail_fields_no_applicant_field_is_absent_at_source():
    """A 200 page fetched successfully but with no applicant markup at all ->
    provenance is "absent_at_source", distinct from a fetch failure."""
    from scrapling import Selector

    page_no_applicant = Selector(
        "<table><tr><th>Ward</th><td>Aethwy</td></tr></table>"
    )  # 200 response, no .status set -> defaults to 200

    fetcher = MagicMock()
    fetcher.get.return_value = page_no_applicant

    ward, applicant, _, provenance = s._fetch_detail_fields(
        fetcher, "https://x.gov.uk",
        "/d.do?keyVal=ABC&activeTab=summary",
        {"JSESSIONID": "s"}, timeout_secs=5,
    )

    assert applicant is None
    assert ward == "Aethwy"
    assert provenance == "absent_at_source"


def test_fetch_detail_fields_no_backoff_on_normal_200():
    """A normal 200 response must not trigger any sleep calls."""
    from scrapling import Selector

    good_page = Selector(IDOX_DETAIL_TABLE)

    fetcher = MagicMock()
    fetcher.get.return_value = good_page

    sleep_calls = []

    with patch.object(s, "time") as mock_time:
        mock_time.sleep.side_effect = lambda n: sleep_calls.append(n)
        ward, applicant, _, provenance = s._fetch_detail_fields(
            fetcher, "https://x.gov.uk",
            "/d.do?keyVal=ABC&activeTab=summary",
            {"JSESSIONID": "s"}, timeout_secs=5,
        )

    assert applicant == "Smith Construction Ltd"
    assert provenance == "published"
    assert sleep_calls == []   # no backoff for a normal fetch


# ─── Swift APAS / SwiftLG APAS tests ──────────────────────────────────────────

# Realistic APAS search results table (normalRow / alternateRow pattern).
SWIFT_APAS_RESULTS_HTML = """
<html><body>
<table class="apas-results">
  <thead>
    <tr>
      <th>Application Ref</th><th>Location</th><th>Proposal</th>
      <th>Status</th><th>Date Received</th>
    </tr>
  </thead>
  <tbody>
    <tr class="normalRow">
      <td><a href="/swift/apas/run/wphappreport.display?appNumber=DC%2F2024%2F00001%2FFUL&amp;appType=Planning">DC/2024/00001/FUL</a></td>
      <td>1 High Street, Walsall WS1 1AA</td>
      <td>Erection of single storey rear extension</td>
      <td>Pending Consideration</td>
      <td>15/01/2024</td>
    </tr>
    <tr class="alternateRow">
      <td><a href="/swift/apas/run/wphappreport.display?appNumber=DC%2F2024%2F00002%2FFUL&amp;appType=Planning">DC/2024/00002/FUL</a></td>
      <td>22 Mill Road, Walsall WS2 2BB</td>
      <td>Change of use from A1 to A3</td>
      <td>Approved</td>
      <td>20/01/2024</td>
    </tr>
  </tbody>
</table>
</body></html>
"""

# Realistic APAS detail page — table layout with applicant and ward.
SWIFT_APAS_DETAIL_HTML = """
<html><body>
<table class="apas-report-table">
  <tr><th>Application Reference</th><td>DC/2024/00001/FUL</td></tr>
  <tr><th>Ward</th><td>Blakenall</td></tr>
  <tr><th>Applicant Name</th><td>Jones Developments Ltd</td></tr>
  <tr><th>Proposal</th><td>Erection of single storey rear extension</td></tr>
</table>
</body></html>
"""


def test_parse_swift_apas_html_extracts_two_rows():
    rows = s._parse_swift_apas_html(SWIFT_APAS_RESULTS_HTML)
    assert len(rows) == 2
    assert rows[0]["reference"] == "DC/2024/00001/FUL"
    assert rows[0]["address"] == "1 High Street, Walsall WS1 1AA"
    assert rows[0]["description"] == "Erection of single storey rear extension"
    assert rows[0]["applicant"] is None
    assert rows[0]["ward"] is None
    assert "/wphappreport.display" in (rows[0]["_detail_href"] or "")


def test_parse_swift_apas_html_alternating_rows():
    rows = s._parse_swift_apas_html(SWIFT_APAS_RESULTS_HTML)
    assert rows[1]["reference"] == "DC/2024/00002/FUL"


def test_parse_swift_apas_html_empty_on_no_rows():
    rows = s._parse_swift_apas_html("<html><body><p>No results</p></body></html>")
    assert rows == []


def test_fetch_swift_apas_detail_extracts_ward_and_applicant():
    from scrapling import Selector

    fetcher = MagicMock()
    fetcher.get.return_value = Selector(SWIFT_APAS_DETAIL_HTML)

    ward, applicant, _ = s._fetch_swift_apas_detail(
        fetcher, "https://planning.walsall.gov.uk",
        "/swift/apas/run/wphappreport.display?appNumber=DC%2F2024%2F00001",
        timeout_secs=5,
    )

    assert ward == "Blakenall"
    assert applicant == "Jones Developments Ltd"


def test_fetch_swift_apas_detail_returns_none_when_href_absent():
    fetcher = MagicMock()
    ward, applicant, received_date = s._fetch_swift_apas_detail(
        fetcher, "https://planning.walsall.gov.uk", None, timeout_secs=5
    )
    assert ward is None
    assert applicant is None
    assert received_date is None
    fetcher.get.assert_not_called()


def test_scrape_swift_apas_end_to_end():
    """APAS primary path: POST search → parse rows → fetch detail → populated result."""
    from scrapling import Selector

    class FakeResp:
        html_content = SWIFT_APAS_RESULTS_HTML

    def fake_get(url, timeout=None, **_kwargs):
        # Detail page fetch
        return Selector(SWIFT_APAS_DETAIL_HTML)

    def fake_post(url, data=None, timeout=None, **_kwargs):
        assert data.get("ward") == "Blakenall"
        return FakeResp()

    mock_fetcher = MagicMock()
    mock_fetcher.get.side_effect = fake_get
    mock_fetcher.post.side_effect = fake_post

    with patch("scrapling.fetchers.Fetcher", return_value=mock_fetcher), \
         patch.object(s, "time") as mock_time:
        mock_time.sleep = lambda *_: None
        results = s.scrape_swift_apas(
            "https://planning.walsall.gov.uk/swift/apas/run/wphappcriteria.display?ward=Blakenall",
            timeout_secs=5,
        )

    assert len(results) == 2
    assert results[0]["reference"] == "DC/2024/00001/FUL"
    assert results[0]["applicant"] == "Jones Developments Ltd"
    assert results[0]["ward"] == "Blakenall"
    assert "_detail_href" not in results[0]


def test_scrape_swift_apas_returns_empty_on_post_failure():
    with patch("scrapling.fetchers.Fetcher") as mock_cls:
        mock_cls.return_value.post.side_effect = Exception("connection refused")
        results = s.scrape_swift_apas(
            "https://planning.walsall.gov.uk/swift/apas/run/wphappcriteria.display?ward=Blakenall",
            timeout_secs=5,
        )
    assert results == []


# ─── D-07: received_date extraction tests ────────────────────────────────────

def test_extract_received_date_from_list_page_metainfo():
    """The p.metaInfo list-page fixture '| Date: 01/01/2024' yields ISO 2024-01-01."""
    if "sidecar" in sys.modules:
        del sys.modules["sidecar"]
    import sidecar as s_local

    results = s_local._parse_idox_html(IDOX_FIXTURE_HTML, url="https://example.com")
    assert len(results) >= 1
    assert results[0]["received_date"] == "2024-01-01", (
        f"Expected '2024-01-01' from '| Date: 01/01/2024', got {results[0]['received_date']!r}"
    )


IDOX_DETAIL_TABLE_WITH_DATE = """
<html><body>
<table id="simpleDetailsTable">
  <tbody>
    <tr><th scope="row">Reference</th><td>FPL/2024/00100</td></tr>
    <tr><th scope="row">Ward :</th><td>Aethwy</td></tr>
    <tr><th scope="row">Applicant Name:</th><td>Smith Construction Ltd</td></tr>
    <tr><th scope="row">Date Received</th><td>15/03/2024</td></tr>
  </tbody>
</table>
</body></html>
"""


def test_extract_received_date_from_detail_page_label():
    """A detail-page row with label 'Date Received' yields ISO date."""
    ward, applicant, received_date = s._extract_detail_fields_from_html(
        IDOX_DETAIL_TABLE_WITH_DATE
    )
    assert ward == "Aethwy"
    assert applicant == "Smith Construction Ltd"
    assert received_date == "2024-03-15", (
        f"Expected '2024-03-15' from '15/03/2024', got {received_date!r}"
    )


def test_extract_received_date_absent_yields_none():
    """When no date label is present, received_date is None (no exception)."""
    html = """<table><tr><th>Ward</th><td>Aethwy</td></tr>
              <tr><th>Applicant Name</th><td>Smith Construction Ltd</td></tr></table>"""
    ward, applicant, received_date = s._extract_detail_fields_from_html(html)
    assert ward == "Aethwy"
    assert applicant == "Smith Construction Ltd"
    assert received_date is None


def test_extract_received_date_malformed_date_yields_none():
    """An unparseable date string in the detail page yields None (no exception)."""
    html = """<table><tr><th>Ward</th><td>Aethwy</td></tr>
              <tr><th>Date Received</th><td>not-a-date</td></tr></table>"""
    _, _, received_date = s._extract_detail_fields_from_html(html)
    assert received_date is None


def test_parse_idox_html_absent_date_yields_none():
    """A list-page row without any '| Date:' segment yields received_date None."""
    html = """
    <ul><li class="searchresult">
      <p class="metaInfo">Ref. No: FPL/2024/00999</p>
      <span class="summaryLinkTextClamp">Extension</span>
      <p class="address">1 Test St LL77 1AA</p>
    </li></ul>
    """
    if "sidecar" in sys.modules:
        del sys.modules["sidecar"]
    import sidecar as s_local

    results = s_local._parse_idox_html(html, url="https://example.com")
    assert len(results) == 1
    assert results[0]["received_date"] is None


# ── CR-01: internal failure must never be reported as a successful empty ward ──
#
# Before this, every internal failure path returned [], which the endpoint wrapped as
# {"status": "ok", "results": []}. Rust read that as ScrapeOutcome::Items(vec![]) ->
# WardAction::Process(vec![]), and the backfill's step-(6) set-difference then stamped
# fetch_failed across every unresolved Contract in the ward. Anglesey is portal_type
# "arcus" and Salesforce rotates fwuid on each release, so the most likely real failure
# reproduced the exact UAT-2026-07-27 defect ward_action exists to prevent.

def test_arcus_internal_failure_raises_rather_than_returning_empty():
    """A blown-up arcus scrape is a failure, not a ward with no applications.

    Simulates the most likely real cause: the Aura bootstrap fetch failing, which is what
    a rotated Salesforce fwuid, a Salesforce error state or a network blip all look like
    from here.
    """
    import pytest
    with patch("scrapling.fetchers.Fetcher") as mock_fetcher_cls:
        mock_fetcher_cls.return_value.get.side_effect = RuntimeError("fwuid rotated")
        with pytest.raises(s.ScrapeFailed):
            s.scrape_arcus("https://ioacc.my.site.com/s/pr-english?ward=Aethwy", {}, 30)


def test_unimplemented_portal_stubs_raise_rather_than_returning_empty():
    """An unimplemented portal type has measured nothing — it has not found nothing."""
    import pytest
    for fn in (s.scrape_northgate_stub, s.scrape_wfs_stub, s.scrape_planapi_stub):
        with pytest.raises(s.ScrapeFailed):
            fn("https://example.gov.uk/planning", {}, 30)


def test_scrape_endpoint_reports_failure_as_error_status():
    """ScrapeFailed surfaces as status='error', which Rust maps to SkipUnmeasured."""
    req = s.ScrapeRequest(
        url="https://ioacc.my.site.com/s/pr-english?ward=Aethwy",
        selectors={},
        timeout=30,
        portal_type="arcus",
    )
    with patch.object(s, "scrape_arcus", side_effect=s.ScrapeFailed("boom")):
        resp = s.scrape(req)
    assert resp.status == "error", "a failed scrape must not be reported as ok"
    assert resp.results == []
    assert "boom" in (resp.reason or "")


def test_scrape_endpoint_still_reports_a_genuinely_empty_ward_as_ok():
    """The distinction has to cut both ways: asked and told nothing is still success."""
    req = s.ScrapeRequest(
        url="https://ioacc.my.site.com/s/pr-english?ward=Aethwy",
        selectors={},
        timeout=30,
        portal_type="arcus",
    )
    with patch.object(s, "scrape_arcus", return_value=[]):
        resp = s.scrape(req)
    assert resp.status == "ok"
    assert resp.results == []


# ── CR-07: /scrape must not be an open SSRF primitive ──────────────────────────

def test_scrape_url_guard_refuses_non_http_schemes():
    import pytest
    for bad in ("file:///etc/passwd", "gopher://x/", "ftp://x/"):
        with pytest.raises(s.ScrapeFailed):
            s._validate_scrape_url(bad)


def test_scrape_url_guard_refuses_internal_targets():
    """The addresses that make SSRF dangerous: metadata service, loopback, private LAN."""
    import pytest
    for bad in (
        "http://169.254.169.254/latest/meta-data/",  # cloud metadata
        "http://127.0.0.1:8000/sql",                 # SurrealDB's own HTTP API
        "http://localhost:8000/sql",
        "http://10.0.0.5/admin",
        "http://192.168.1.1/",
    ):
        with pytest.raises(s.ScrapeFailed):
            s._validate_scrape_url(bad)


def test_scrape_url_guard_allows_a_real_planning_portal():
    # Resolves publicly; must not be refused.
    s._validate_scrape_url("https://ioacc.my.site.com/s/pr-english?ward=Aethwy")
