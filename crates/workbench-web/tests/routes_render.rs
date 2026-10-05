//! D-24's route-render-without-panic proof, host-side (`#[test]`, no wasm, no DB, no
//! headless-browser runner — this workspace has none wired in, phase 42 research Pitfall 10).
//!
//! This is a proof about the ROUTE TABLE ITSELF, not a re-run of the individual pages'
//! already-tested pure formatting functions (43-06/07/08 own those). It reads the real
//! `app.rs` source via `include_str!` and asserts that each of the expected
//! `(path, component)` pairs appears in a `<Route path=path!("<path>") view=<Component> />`
//! entry EXACTLY once (whitespace-normalised), and that the total count of `<Route ` entries in
//! the file is exactly 5 — so a stray sixth route, a leftover `spike/cosmograph` mount, or a
//! duplicated/missing entry fails this test.
//!
//! Non-vacuity: temporarily delete any one `<Route ...>` line from `app.rs` and re-run this
//! test — it fails (both the per-pair assertion for the deleted route AND the total-count
//! assertion). Revert after confirming.

const APP_RS: &str = include_str!("../src/app.rs");

/// Collapses all runs of ASCII whitespace (including newlines) into a single space, so a
/// `<Route ... />` written across multiple source lines still matches a single-line expected
/// pattern.
fn normalize_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The 14 expected `(path, component)` pairs the route table wires, in no particular order (route
/// table ordering is not part of this contract — presence and uniqueness are).
///
/// `/entity` and `/entity/:id` are plan 45-10's (PIV-01/PIV-02): the search surface a multi-match
/// query renders its candidate list on, and the profile screen itself. Both are listed here rather
/// than only in `abbr_reachable.rs`, because the total-count assertion below is what catches a
/// route silently disappearing.
const EXPECTED_ROUTES: &[(&str, &str)] = &[
    ("", "HomePage"),
    ("analysis", "AnalysisPage"),
    // TRI-01/TRI-02, plan 46-09. The alert / motif triage inbox.
    ("alerts", "AlertsPage"),
    // TRI-01/TRI-02, plan 46-10. The alert detail route, deep-linkable from a row's "Open alert →".
    ("alert/:id", "AlertDetailPage"),
    ("graph", "GraphPage"),
    ("entity", "EntitySearchPage"),
    ("entity/:id", "EntityProfilePage"),
    // FRB-04, plan 50-07. The finding board's detail route, seeded from a finding's own
    // `RecordRef`s.
    ("finding/:probe", "FindingDetailPage"),
    ("help", "HelpPage"),
    ("help/glossary", "GlossaryPage"),
    ("help/algorithms", "AlgorithmsIndexPage"),
    ("help/algorithms/:slug", "AlgorithmHelpPage"),
    // PIV-05, plan 45-15. `/investigations` is the shell destination the header links to;
    // `/investigations/:id` is one investigation reopened whole.
    ("investigations", "InvestigationsPage"),
    ("investigations/:id", "InvestigationPage"),
];

#[test]
fn each_expected_route_path_component_pair_appears_exactly_once() {
    let normalized = normalize_whitespace(APP_RS);
    for (path, component) in EXPECTED_ROUTES {
        let expected = format!(r#"<Route path=path!("{path}") view={component} />"#);
        let occurrences = normalized.matches(&expected).count();
        assert_eq!(
            occurrences, 1,
            "expected exactly one `{expected}` entry in app.rs, found {occurrences}"
        );
    }
}

#[test]
fn the_route_table_has_exactly_the_expected_route_entries() {
    let count = APP_RS.matches("<Route ").count();
    assert_eq!(
        count,
        EXPECTED_ROUTES.len(),
        "expected exactly {} `<Route ` entries in app.rs (a stray extra route, a leftover \
         spike/cosmograph mount, or a missing entry would change this count), found {count}",
        EXPECTED_ROUTES.len()
    );
}

#[test]
fn the_deleted_spike_cosmograph_route_is_not_mounted() {
    assert!(
        !APP_RS.contains(r#"path!("spike/cosmograph")"#),
        "the /spike/cosmograph route must not be mounted in app.rs's route table (D-07) — the \
         shim module itself stays in the tree for a future milestone to remount, only the \
         mount is removed here"
    );
}

#[test]
fn the_sidebar_left_nav_is_gone() {
    assert!(
        !APP_RS.contains(r#"aria-disabled="true""#),
        "app.rs must not contain the deleted left-nav's disabled stub rows"
    );
}
