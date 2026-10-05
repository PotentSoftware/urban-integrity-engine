# Scrapling Sidecar

HTTP sidecar for planning portal scraping. The UrbanIntegrity Rust engine POSTs
to this service to scrape planning application data from local planning authority
portals across England, Scotland and Wales.

## Supported Portal Systems

| `portal_type` | Strategy | Description | Coverage |
|--------------|----------|-------------|----------|
| `"idox"` | `scrape_idox()` — static HTML via `Fetcher()` | Idox PublicAccess | ~69% of UK LPAs |
| `"arcus"` | `scrape_arcus()` — Salesforce/Aura guest API | Arcus Global cloud portals | Growing |
| `"wfs"` | Stub (TBD) | WFS data source e.g. Scotland Spatial Hub | Scotland (34 LPAs via 1 endpoint) |
| `"planapi"` | Stub (TBD) | PlanAPI commercial aggregator | All UK LPAs (paid) |
| _fallback_ | `scrape_static_css()` — selectors from request | Generic CSS portals | Any static HTML |

The `portal_type` is set in `config.toml` per portal entry and passed through by
the Rust engine. Adding support for a new portal system only requires a new
`portal_type` value and a corresponding scraper function — no Rust changes needed.

## Wire Protocol

```
POST /scrape
  Request:  { "url": str, "selectors": {}, "timeout": int, "portal_type": str }
  Response: { "status": "ok"|"error", "results": [...], "reason": str|null }

GET /health
  Response: { "status": "ok", "service": "scrapling-sidecar" }
```

Each result object:
```json
{
  "reference": "FPL/2024/00100",
  "description": "Construction of 3 dwellings",
  "address": "1 High Street, Exampleton, XX1 1AA",
  "ward": "Example North",
  "applicant": "Smith Construction Ltd"
}
```

## Setup

Python 3.11+ (the version the maintainers run). From the repo root:

```bash
python3 -m venv scrapling-sidecar/.venv
scrapling-sidecar/.venv/bin/pip install -r scrapling-sidecar/requirements.txt
```

## Run

Preferred — from the repo root, as a detached user service (falls back to `nohup` without systemd),
using the `.venv` above:

```bash
scripts/sidecar-start.sh       # stop: systemctl --user stop urban-sidecar (or kill "$(cat data/sidecar.pid)")
```

Or in the foreground, for debugging:

```bash
cd scrapling-sidecar
.venv/bin/python sidecar.py                  # port 8765 (default)
SIDECAR_PORT=9000 .venv/bin/python sidecar.py  # custom port
```

Tests: `cd scrapling-sidecar && .venv/bin/pip install pytest && .venv/bin/python -m pytest`.

## Test Health

```bash
curl http://localhost:8765/health
# {"status":"ok","service":"scrapling-sidecar"}
```

## Test Scrape (Idox portal)

The host below is a fictional placeholder (`.invalid` never resolves) — substitute your own
council's Idox PublicAccess search URL and one of its ward values.

```bash
curl -X POST http://localhost:8765/scrape \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://planning.example.invalid/online-applications/advancedSearchResults.do?action=firstPage&searchCriteria.ward=EXAMPLE%20NORTH",
    "selectors": {},
    "timeout": 30,
    "portal_type": "idox"
  }'
```

## Adding a new portal system

1. Add a new strategy function: `scrape_{system}(url, selectors, timeout) -> list[dict]`
2. Add an `elif strategy == "{system}":` branch in the `/scrape` endpoint dispatch
3. Add `portal_type = "{system}"` to the relevant `[[ingest.planning_portals]]` entries in `config.toml`

## Idox-specific notes

- **Rate limiting:** 1 second sleep between detail page fetches (ethical scraping)
- **Two-step fetch:** Results list page → per-application detail page for ward/applicant
- **Selectors confirmed** against live `publicaccess.iow.gov.uk` (same structure across all Idox instances)
- **Ward values** in Idox `<select>` dropdowns are uppercase (e.g. `EXAMPLE NORTH`) — verify the
  exact values against the portal's live dropdown.
