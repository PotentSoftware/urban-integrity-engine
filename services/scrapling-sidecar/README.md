# Scrapling Sidecar (obsolete prototype)

> **Do not use this copy.** It is an early generic-CSS prototype that nothing starts or tests. The
> sidecar the engine uses is [`scrapling-sidecar/`](../../scrapling-sidecar/README.md) at the repo root,
> launched with `scripts/sidecar-start.sh`.

HTTP sidecar for Urban Integrity planning portal scraping. Runs as a local Python service on `localhost:8765`.

## Setup

```bash
cd services/scrapling-sidecar
pip install -r requirements.txt
uvicorn main:app --port 8765 --host 127.0.0.1
```

## API

### `GET /health`
Returns `{"status": "ok"}` when the sidecar is running.

### `POST /scrape`

Scrape a URL and extract structured data using CSS selectors.

**Request:**
```json
{
  "url": "https://planning.example.invalid/search",
  "selectors": {
    "container": ".planning-app-item",
    "reference": ".app-ref",
    "description": ".app-description",
    "address": ".app-address"
  },
  "timeout": 30
}
```

**`selectors` convention:**
- The **first** key is the **container** selector — selects the repeating item elements
- Subsequent keys are **field** selectors relative to each container element

**Response (success):**
```json
{
  "status": "ok",
  "results": [
    {
      "reference": "23/01234/FUL",
      "description": "Erection of warehouse",
      "address": "1 Industrial Way, Leeds, LS1 1AA"
    }
  ]
}
```

**Response (error):**
```json
{
  "status": "error",
  "reason": "timeout"
}
```

## Integration with Rust

The Rust client in `crates/ingestor/src/planning/sidecar.rs` calls this sidecar.
If the sidecar is unavailable, the Rust client returns an empty result and logs a warning — it never panics.

## Environment

No API keys required. The sidecar only accepts connections from `127.0.0.1` by default.
