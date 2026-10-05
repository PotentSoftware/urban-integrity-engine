//! Embedded static assets (Trunk's `dist/`) with an SPA fallback and an honest "not built" stub
//! (D-14). This is the first `rust-embed` use in this codebase — there is no in-repo analog to
//! follow (the phase 42 pattern notes' "No Analog Found" table); the shape below follows the community
//! idiom the phase 42 research notes drafted, confirmed against the installed `rust-embed` 8.12.0 crate's
//! own `tests/allow_missing.rs` (not merely its README) before being relied on here.

use std::borrow::Cow;

use axum::{
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

/// `#[allow_missing = true]` makes a missing `dist/` folder compile to an empty embedded asset
/// set instead of a hard build error — exactly D-14's "`cargo build --workspace` must keep
/// working on a machine with no Trunk, no Node and no wasm target" requirement. `dist/` is
/// produced by `scripts/build-workbench.sh` (plan 42-04) and is never committed (gitignored).
///
/// The crate enables rust-embed's `debug-embed` feature (see `Cargo.toml`), so this map is
/// baked in at compile time in debug builds too. Without it, rust-embed's default debug path
/// reads `dist/<path>` from disk on every request — still traversal-safe (it canonicalises and
/// checks the `dist/` prefix), but a `cargo run` workbench would serve whatever was on disk
/// under `dist/` at request time, contradicting the threat-model note on `static_handler`.
/// The trade-off: a debug binary must be rebuilt after `scripts/build-workbench.sh` to see a
/// new `dist/`, exactly as a release binary already must.
#[derive(RustEmbed)]
#[folder = "dist/"]
#[allow_missing = true]
struct Assets;

const NOT_BUILT_HTML: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>UrbanIntegrity Workbench</title></head><body>\
<h1>Workbench assets not built</h1>\
<p>Run `scripts/build-workbench.sh` to build the frontend, or restart after a Trunk build \
completes. This is a native-only binary with no bundled UI yet.</p>\
</body></html>";

/// The SPA fallback's routing decision for one request path — see [`resolve_asset`].
#[derive(Debug, PartialEq)]
pub(crate) enum Resolution {
    /// Serve `data` and derive the `Content-Type` from `served_path` — which is the path of the
    /// file ACTUALLY being served (`index.html` on the SPA fallback), never the requested path.
    Asset {
        served_path: String,
        data: Cow<'static, [u8]>,
    },
    /// An asset-shaped path (has a file extension) or an `api/` path that does not exist: a
    /// real `404`, never `index.html` mislabelled as JavaScript/JSON.
    NotFound,
    /// Neither the path nor `index.html` exists — `dist/` was never built at all (D-14).
    NotBuilt,
}

/// A path is a client-side SPA route (and so may fall back to `index.html`) iff it carries no
/// file extension AND is not under `api/` — `/api/...` is the workbench's own JSON namespace
/// and must never resolve to an HTML page.
fn is_spa_route(path: &str) -> bool {
    !path.starts_with("api/") && std::path::Path::new(path).extension().is_none()
}

/// The pure routing core of [`static_handler`]: exact path first, then `index.html` for SPA
/// routes only, then the not-built stub. Takes the asset lookup as a closure so the decision
/// table is unit-testable against a fake asset map on every CI build — `dist/` is gitignored
/// and absent on CI, so a test that only ran against the real embedded map could never observe
/// the `Asset` arm (this is exactly how the mislabelled-`Content-Type` defect this function
/// replaces went unnoticed: `GET /` served `index.html` as `application/octet-stream`, a
/// browser download, and no test could see it).
pub(crate) fn resolve_asset(
    path: &str,
    get: impl Fn(&str) -> Option<Cow<'static, [u8]>>,
) -> Resolution {
    if let Some(data) = get(path) {
        return Resolution::Asset {
            served_path: path.to_string(),
            data,
        };
    }
    if !is_spa_route(path) {
        return Resolution::NotFound;
    }
    match get("index.html") {
        Some(data) => Resolution::Asset {
            served_path: "index.html".to_string(),
            data,
        },
        None => Resolution::NotBuilt,
    }
}

/// SPA fallback: tries the exact requested path, falls back to `index.html` for client-side
/// routes (extension-less, non-`api/` paths), returns a real `404` for a missing asset or an
/// unknown `api/` path, and — only when BOTH the path and `index.html` are absent (i.e. `dist/`
/// was never built at all) — returns a `200 OK` honest stub page rather than a hard error or an
/// empty `404` (D-14). The `Content-Type` is always derived from the file actually served, so
/// `GET /` is `text/html` even though the requested path (`""`) has no extension. Path traversal
/// is structurally impossible here: `Assets::get` performs a lookup against a compile-time
/// embedded map, never a filesystem read at request time (threat T-42-04) — stated explicitly
/// per the threat model's `security_requirements` note rather than left implicit.
/// `Cache-Control` for a served asset path. `index.html` is the SPA shell and the only thing that
/// names the current bundle hashes, so it is always revalidated (`no-cache` — cacheable, but
/// never used without asking the server first). Trunk stamps a 16-hex content hash into every
/// bundle filename (`name-<hash>.ext` / `name-<hash>_bg.wasm`), so a hashed asset can never
/// change under its own name and is pinned as `immutable`. Anything else gets `no-cache`.
///
/// Without this the server sent no caching header at all, and Chrome's heuristic freshness kept
/// the operator on a stale build across three rebuilds on 2026-09-06.
pub(crate) fn cache_control_for(served_path: &str) -> &'static str {
    if served_path == "index.html" {
        return "no-cache";
    }
    let file_name = served_path.rsplit('/').next().unwrap_or(served_path);
    let stem = file_name.split('.').next().unwrap_or(file_name);
    let stem = stem.strip_suffix("_bg").unwrap_or(stem);
    let has_hash = stem
        .rsplit_once('-')
        .is_some_and(|(_, h)| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()));
    if has_hash {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    match resolve_asset(path, |p| Assets::get(p).map(|file| file.data)) {
        Resolution::Asset { served_path, data } => {
            let mime = mime_guess::from_path(&served_path).first_or_octet_stream();
            (
                [
                    (header::CONTENT_TYPE, mime.as_ref()),
                    (header::CACHE_CONTROL, cache_control_for(&served_path)),
                ],
                data,
            )
                .into_response()
        }
        Resolution::NotFound => StatusCode::NOT_FOUND.into_response(),
        Resolution::NotBuilt => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html")],
            NOT_BUILT_HTML,
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use std::collections::HashMap;

    /// A fake embedded-asset map standing in for a built `dist/` — `dist/` is gitignored and
    /// never present on CI, so these are the ONLY tests that can exercise the `Asset` arm.
    fn fake_dist() -> HashMap<&'static str, &'static [u8]> {
        HashMap::from([
            (
                "index.html",
                b"<!doctype html><div id=app></div>".as_slice(),
            ),
            ("workbench-web-abc123.js", b"export {};".as_slice()),
            ("workbench-web-abc123_bg.wasm", b"\0asm".as_slice()),
        ])
    }

    fn lookup(
        map: HashMap<&'static str, &'static [u8]>,
    ) -> impl Fn(&str) -> Option<Cow<'static, [u8]>> {
        move |p| map.get(p).map(|bytes| Cow::Borrowed(*bytes))
    }

    fn served_path(resolution: &Resolution) -> Option<&str> {
        match resolution {
            Resolution::Asset { served_path, .. } => Some(served_path),
            _ => None,
        }
    }

    /// `GET /` (path `""`) must serve `index.html` AND report it as `index.html`, so the
    /// `Content-Type` derived from it is `text/html` — not `application/octet-stream` from the
    /// empty requested path, which browsers treat as a download.
    ///
    /// PROVE THIS CAN FAIL: run against the pre-fix handler shape (`served_path = path`), this
    /// assertion fails with `Some("")`.
    #[test]
    fn root_path_falls_back_to_index_html_and_reports_index_html_as_served() {
        let resolved = resolve_asset("", lookup(fake_dist()));
        assert_eq!(served_path(&resolved), Some("index.html"));
        assert_eq!(
            mime_guess::from_path("index.html").first_or_octet_stream(),
            mime_guess::mime::TEXT_HTML
        );
    }

    /// An extension-less client-side route (`/spike/cosmograph`) is an SPA route: `index.html`.
    #[test]
    fn extensionless_route_falls_back_to_index_html() {
        let resolved = resolve_asset("spike/cosmograph", lookup(fake_dist()));
        assert_eq!(served_path(&resolved), Some("index.html"));
    }

    /// An asset that exists is served under its OWN path (so `.js` is `application/javascript`,
    /// `.wasm` is `application/wasm`).
    #[test]
    fn existing_asset_is_served_under_its_own_path() {
        let resolved = resolve_asset("workbench-web-abc123.js", lookup(fake_dist()));
        assert_eq!(served_path(&resolved), Some("workbench-web-abc123.js"));
        let resolved = resolve_asset("workbench-web-abc123_bg.wasm", lookup(fake_dist()));
        assert_eq!(served_path(&resolved), Some("workbench-web-abc123_bg.wasm"));
    }

    /// A stale hashed bundle request after a rebuild (`/workbench-web-<oldhash>.js`) must be a
    /// `404`, never `index.html` labelled `application/javascript` (a syntax error in the
    /// browser with no clue as to why).
    #[test]
    fn missing_asset_with_an_extension_is_not_found_not_index_html() {
        let resolved = resolve_asset("workbench-web-old000.js", lookup(fake_dist()));
        assert_eq!(resolved, Resolution::NotFound);
    }

    /// `/api/...` is the workbench's JSON namespace: an unknown path there is a `404`, never a
    /// `200` HTML page.
    #[test]
    fn unknown_api_path_is_not_found_not_index_html() {
        let resolved = resolve_asset("api/does-not-exist", lookup(fake_dist()));
        assert_eq!(resolved, Resolution::NotFound);
    }

    /// With no `index.html` at all (`dist/` never built), an SPA route yields the stub, while
    /// an asset-shaped path is still a plain `404`.
    #[test]
    fn without_index_html_spa_routes_are_not_built_and_assets_are_not_found() {
        let empty = lookup(HashMap::new());
        assert_eq!(resolve_asset("", &empty), Resolution::NotBuilt);
        assert_eq!(resolve_asset("some/route", &empty), Resolution::NotBuilt);
        assert_eq!(resolve_asset("app.js", &empty), Resolution::NotFound);
        assert_eq!(resolve_asset("api/x", &empty), Resolution::NotFound);
    }

    /// On a build where `crates/workbench/dist/` does not exist (the state of this worktree,
    /// and every CI `test`/`lint` job), a request for an extension-less path returns `200 OK`
    /// with the exact D-14 / the phase 42 UI spec "Empty state heading" text — never a hard error
    /// and never an empty 404.
    #[tokio::test]
    async fn static_handler_returns_the_not_built_stub_when_dist_is_absent() {
        if Assets::get("index.html").is_some() {
            eprintln!("[skip] dist/ is built on this machine — the not-built stub is unreachable");
            return;
        }
        let uri: Uri = "/some/nonexistent/path".parse().unwrap();
        let resp = static_handler(uri).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            text.contains("Workbench assets not built"),
            "stub body must contain the exact D-14 heading: {text}"
        );
    }

    /// Requesting the SPA's own root path behaves identically when `dist/` is absent — the
    /// stub, not a panic or a bare 404 — since `dist/index.html` is equally absent.
    #[tokio::test]
    async fn static_handler_returns_the_stub_for_the_root_path_too() {
        if Assets::get("index.html").is_some() {
            eprintln!("[skip] dist/ is built on this machine — the not-built stub is unreachable");
            return;
        }
        let uri: Uri = "/".parse().unwrap();
        let resp = static_handler(uri).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html"
        );
    }

    /// A missing asset-shaped path is a `404` through the real handler regardless of whether
    /// `dist/` is built.
    #[tokio::test]
    async fn static_handler_returns_404_for_a_missing_asset_path() {
        let uri: Uri = "/workbench-web-definitely-not-there.js".parse().unwrap();
        let resp = static_handler(uri).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// Only meaningful on a machine where `scripts/build-workbench.sh` has produced `dist/`:
    /// `GET /` through the real embedded map is `text/html`, not a download.
    #[tokio::test]
    async fn static_handler_serves_root_as_text_html_when_dist_is_built() {
        if Assets::get("index.html").is_none() {
            eprintln!("[skip] dist/ is not built on this machine — run scripts/build-workbench.sh");
            return;
        }
        let uri: Uri = "/".parse().unwrap();
        let resp = static_handler(uri).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(
            content_type.starts_with("text/html"),
            "GET / must be served as text/html, got {content_type}"
        );
    }

    #[test]
    fn cache_policy_revalidates_index_html_and_pins_hashed_assets() {
        // The SPA shell must be revalidated on every load — it is the only thing that names the
        // current wasm/js/css hashes, and a heuristically-cached copy kept the operator on a
        // stale build after three rebuilds on 2026-09-06.
        assert_eq!(cache_control_for("index.html"), "no-cache");
        // Trunk stamps a 16-hex content hash into every bundle name, so those may live forever.
        assert_eq!(
            cache_control_for("urban-integrity-workbench-web-85fe2393dc39bd77_bg.wasm"),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            cache_control_for("tokens-e60a18b6158d230e.css"),
            "public, max-age=31536000, immutable"
        );
        // Anything without a content hash in its name gets the safe default.
        assert_eq!(cache_control_for("snippets/foo/inline0.js"), "no-cache");
        assert_eq!(cache_control_for("favicon.ico"), "no-cache");
    }

    #[tokio::test]
    async fn static_handler_sends_no_cache_for_root_when_dist_is_built() {
        if Assets::get("index.html").is_none() {
            eprintln!("[skip] dist/ is not built on this machine — run scripts/build-workbench.sh");
            return;
        }
        let uri: Uri = "/".parse().unwrap();
        let resp = static_handler(uri).await;
        assert_eq!(
            resp.headers()
                .get(header::CACHE_CONTROL)
                .map(|v| v.to_str().unwrap()),
            Some("no-cache"),
            "GET / must tell the browser to revalidate the SPA shell"
        );
    }
}
