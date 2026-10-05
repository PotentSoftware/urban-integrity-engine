"""Tests for the Arcus Global (Salesforce Experience Cloud) planning scraper.

Target portal: Isle of Anglesey — https://ioacc.my.site.com/s/pr-english
The search is a Salesforce Aura action (apex://PR_SearchCont/ACTION$query); the
wire contract these tests pin down is captured live in
the Anglesey planning-portal DNS investigation notes.

Pure helpers (no network) are unit-tested here. The end-to-end scrape_arcus()
orchestrator is integration-only and exercised against the live portal separately.
"""
import json
from unittest.mock import MagicMock

import sidecar as s


# ── _arcus_ward_from_url: the config URL carries the ward in ?ward= ────────────

def test_arcus_ward_from_url_extracts_ward():
    url = "https://ioacc.my.site.com/s/pr-english?ward=Aethwy"
    assert s._arcus_ward_from_url(url) == "Aethwy"


def test_arcus_ward_from_url_decodes_spaces():
    url = "https://ioacc.my.site.com/s/pr-english?ward=Bro+Rhosyr"
    assert s._arcus_ward_from_url(url) == "Bro Rhosyr"


def test_arcus_ward_from_url_missing_ward_returns_empty():
    assert s._arcus_ward_from_url("https://ioacc.my.site.com/s/pr-english") == ""


# ── _extract_aura_bootstrap: fwuid + app uid live (url-encoded) in page HTML ───

def test_extract_aura_bootstrap_finds_ids():
    # Real page embeds these url-encoded inside an aura config blob.
    html = (
        'window.blah=1;'
        '%7B%22fwuid%22%3A%22ABC123fwuid%22%2C'
        '%22APPLICATION%40markup%3A%2F%2Fsiteforce%3AcommunityApp%22%3A%221692_appUID%22%7D'
    )
    fwuid, app_uid = s._extract_aura_bootstrap(html)
    assert fwuid == "ABC123fwuid"
    assert app_uid == "1692_appUID"


def test_extract_aura_bootstrap_missing_returns_none():
    fwuid, app_uid = s._extract_aura_bootstrap("<html>no aura here</html>")
    assert fwuid is None
    assert app_uid is None


# ── _build_arcus_query_body: the four aura form fields ─────────────────────────

def test_build_arcus_query_body_shapes_message_and_criteria():
    body = s._build_arcus_query_body("FW", "1692_app", "PApplication", "Aethwy")

    assert set(body) == {"message", "aura.context", "aura.pageURI", "aura.token"}
    assert body["aura.token"] == "null"  # guest community — literal string

    msg = json.loads(body["message"])
    action = msg["actions"][0]
    assert action["descriptor"] == "apex://PR_SearchCont/ACTION$query"
    params = action["params"]
    assert params["category_name"] == "PApplication"
    assert params["search_criteria"] == {"Ward__c": "Aethwy"}

    ctx = json.loads(body["aura.context"])
    assert ctx["fwuid"] == "FW"
    assert ctx["loaded"]["APPLICATION@markup://siteforce:communityApp"] == "1692_app"


# ── _parse_arcus_response: double-encoded aura envelope → result dicts ─────────

def _success_envelope(records):
    return json.dumps({
        "actions": [{
            "id": "1;a",
            "state": "SUCCESS",
            "returnValue": json.dumps({"records": records}),
        }]
    })


def test_parse_arcus_response_maps_record_fields():
    record = {
        "attributes": {"type": "arcusbuilt__PApplication__c"},
        "Name": "MB/273D",
        "Id": "a1G5800000HywiOEAR",
        "Site_Address_Free_Text__c": "Hyland, Menai Bridge",
        "arcusbuilt__Proposal__c": "ALTERATIONS AND EXTENSIONS TO",
        "Approved_Decision__c": "Caniatáu / Permitted",
    }
    rows = s._parse_arcus_response(_success_envelope([record]), "Aethwy")

    assert len(rows) == 1
    r = rows[0]
    assert r["reference"] == "MB/273D"
    assert r["description"] == "ALTERATIONS AND EXTENSIONS TO"
    assert r["address"] == "Hyland, Menai Bridge"
    assert r["ward"] == "Aethwy"          # supplied by the query, not the record
    assert r["applicant"] is None
    assert r["received_date"] is None


def test_parse_arcus_response_skips_record_without_reference():
    record = {"Site_Address_Free_Text__c": "Nowhere", "arcusbuilt__Proposal__c": "x"}
    rows = s._parse_arcus_response(_success_envelope([record]), "Aethwy")
    assert rows == []


def test_parse_arcus_response_error_state_raises():
    """BL-01: a server-side ERROR state is a failure to measure, not an empty ward.

    This previously degraded to [], which the caller could not distinguish from a ward
    that genuinely publishes nothing — so an Apex heap-limit error was reported to Rust
    as a successful empty ward and the backfill stamped fetch_failed across it.
    """
    import pytest
    envelope = json.dumps({
        "actions": [{
            "id": "1;a",
            "state": "ERROR",
            "returnValue": None,
            "error": [{"message": "JSON string exceeds heap size limit"}],
        }]
    })
    with pytest.raises(s.ScrapeFailed):
        s._parse_arcus_response(envelope, "Aethwy")


def test_parse_arcus_response_success_with_no_records_returns_empty():
    """The distinction must cut both ways: a SUCCESS action with no records is [] ."""
    envelope = json.dumps({
        "actions": [{
            "id": "1;a",
            "state": "SUCCESS",
            "returnValue": json.dumps({"records": []}),
        }]
    })
    assert s._parse_arcus_response(envelope, "Aethwy") == []


def test_parse_arcus_response_malformed_raises():
    """BL-01: an unparseable envelope means we could not read the portal's answer."""
    import pytest
    with pytest.raises(s.ScrapeFailed):
        s._parse_arcus_response("not json at all", "Aethwy")


# ── _arcus_getrecord_body: batched LDS getRecord to fetch applicant/agent/date ─

def test_arcus_getrecord_body_one_action_per_id():
    body = s._arcus_getrecord_body(["a1G001", "a1G002"], "FW", "1692_app")
    assert set(body) == {"message", "aura.context", "aura.pageURI", "aura.token"}
    assert body["aura.token"] == "null"
    msg = json.loads(body["message"])
    assert len(msg["actions"]) == 2
    a0 = msg["actions"][0]
    assert a0["descriptor"].endswith("RecordGvpController/ACTION$getRecord")
    # recordDescriptor is "{id}.{layout}.FULL...."
    assert a0["params"]["recordDescriptor"].startswith("a1G001.")
    assert ".FULL." in a0["params"]["recordDescriptor"]
    ctx = json.loads(body["aura.context"])
    assert ctx["fwuid"] == "FW"
    assert ctx["loaded"]["APPLICATION@markup://siteforce:communityApp"] == "1692_app"


# ── _parse_arcus_details: id → {applicant, agent, received_date} ──────────────

def _detail_envelope(records: dict) -> str:
    # Mirrors the real batched getRecord response: record data lives under
    # context.globalValueProviders[*].values.records.<id>.arcusbuilt__PApplication__c.record.fields
    recs = {}
    for rid, f in records.items():
        recs[rid] = {"arcusbuilt__PApplication__c": {"record": {"id": rid, "fields": {
            k: {"displayValue": None, "value": v} for k, v in f.items()
        }}}}
    return json.dumps({
        "actions": [{"id": "1;a", "state": "SUCCESS", "returnValue": None}],
        "context": {"globalValueProviders": [
            {"type": "$Locale", "values": {}},
            {"type": "$Record", "values": {"records": recs}},
        ]},
    })


def test_parse_arcus_details_maps_fields_by_id():
    env = _detail_envelope({
        "a1G001": {"Applicant_Name_Formula__c": "Mr Alex Example",
                   "Agent_Name_Formula__c": "Mr Jordan Sample",
                   "arcusbuilt__ReceivedDate__c": "2024-09-03"},
        "a1G002": {"Applicant_Name_Formula__c": "Jane Example",
                   "Agent_Name_Formula__c": None,
                   "arcusbuilt__ReceivedDate__c": "2023-01-10"},
    })
    out = s._parse_arcus_details(env)
    assert out["a1G001"]["applicant"] == "Mr Alex Example"
    assert out["a1G001"]["agent"] == "Mr Jordan Sample"
    assert out["a1G001"]["received_date"] == "2024-09-03"
    assert out["a1G002"]["applicant"] == "Jane Example"
    assert out["a1G002"]["agent"] is None


def test_parse_arcus_details_missing_fields_are_none():
    env = _detail_envelope({"a1G003": {"Name": "X/1"}})  # no applicant field present
    out = s._parse_arcus_details(env)
    assert out["a1G003"]["applicant"] is None
    assert out["a1G003"]["received_date"] is None


def test_parse_arcus_details_malformed_returns_empty():
    assert s._parse_arcus_details("not json") == {}
    assert s._parse_arcus_details(json.dumps({"actions": []})) == {}


def test_parse_arcus_details_sets_provenance_published_and_absent_at_source():
    env = _detail_envelope({
        "a1G001": {"Applicant_Name_Formula__c": "Mr Alex Example"},
        "a1G002": {"Applicant_Name_Formula__c": None},
    })
    out = s._parse_arcus_details(env)
    assert out["a1G001"]["provenance"] == "published"
    assert out["a1G002"]["provenance"] == "absent_at_source"


# ── _arcus_fetch_details: D-03 per-record failure attribution (bisection) ──────

def test_arcus_fetch_details_partial_batch_failure():
    """A chunk of 4 ids where exactly one sub-call (post-bisection) fails: all 4
    ids must appear as keys in the result, the failing id carries
    provenance == "fetch_failed", and the succeeding ids carry
    "published"/"absent_at_source" per their fixture data."""
    ids = ["a1G001", "a1G002", "a1G003", "a1G004"]
    good_records = {
        "a1G001": {"Applicant_Name_Formula__c": "Mr Alex Example"},
        "a1G002": {"Applicant_Name_Formula__c": None},  # absent_at_source
        "a1G004": {"Applicant_Name_Formula__c": "Jane Example"},
    }

    def fake_post(url, data=None, timeout=None, **_kwargs):
        # Recover which ids this POST covers from the request body.
        msg = json.loads(data["message"])
        requested = [
            a["params"]["recordDescriptor"].split(".", 1)[0] for a in msg["actions"]
        ]
        resp = MagicMock()
        if "a1G003" in requested:
            # This sub-call always fails, regardless of bisection depth, until
            # it is isolated to the single id a1G003 — malformed body triggers
            # the exception/degrade-to-{} path in _parse_arcus_details.
            resp.body = b"not json"
            return resp
        subset = {rid: good_records[rid] for rid in requested if rid in good_records}
        resp.body = _detail_envelope(subset).encode("utf-8")
        return resp

    fetcher = MagicMock()
    fetcher.post.side_effect = fake_post

    out = s._arcus_fetch_details(fetcher, "https://x/aura", ids, "FW", "APP", timeout_secs=5)

    assert set(out.keys()) == set(ids)
    assert out["a1G001"]["provenance"] == "published"
    assert out["a1G002"]["provenance"] == "absent_at_source"
    assert out["a1G004"]["provenance"] == "published"
    assert out["a1G003"]["provenance"] == "fetch_failed"
    assert out["a1G003"]["applicant"] is None
