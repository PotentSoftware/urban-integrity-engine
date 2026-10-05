//! **Proof that `/alerts`' filter rail is mounted once and never torn down by a facets update —
//! the 46-UAT test 3 regression this plan (46-12) exists to close.**
//!
//! The 46-UAT P_c-range investigation diagnosed the root cause: `AlertFiltersRail`
//! used to be rendered inside `move || facets.get().map(|f| view! { <AlertFiltersRail facets=f
//! .../> })`. Every accepted keystroke in a P_c field set `min_score`/`max_score`, which triggered
//! a refetch whose success handler always called `facets.set(...)` — and `RwSignal::set` notifies
//! unconditionally in Leptos 0.8. That destroyed and rebuilt the whole rail, including the focused
//! `<input>`, after the first character: typing "0.85" stopped at "0".
//!
//! This crate has no DOM runner (phase 42 research Pitfall 10), so — like `abbr_reachable.rs` and
//! `pivot_reachable.rs` — every test here is host-side source reading over `include_str!`, not a
//! rendered check. What it proves: the mount site's own text shape, not what a browser does with
//! it (that is 46-14's human-verify gate).
//!
//! Four things asserted:
//! 1. `<AlertFiltersRail` appears exactly once in `alerts/mod.rs`, and nothing in the 160
//!    whitespace-normalised characters immediately before it re-enters a `facets`-reading closure.
//! 2. That one mount site passes `facets=facets` — the signal itself — not a closure-bound value.
//! 3. `filters.rs`'s `pc_bound_field` builder renders exactly one `<input>`, and that input commits
//!    on `on:change` (fires on blur/Enter/arrow-key-stepping, never mid-keystroke), never
//!    `on:input`.
//! 4. `pc_bound_field` has exactly two call sites (Minimum and Maximum P_c), not zero and not more.
//!
//! Non-vacuity, run once by hand during 46-12's execution and noted in the SUMMARY: temporarily
//! re-wrap the mount site in `{move || facets.get().map(|f| view! { <AlertFiltersRail facets=f
//! ... /> })}` and confirm
//! [`alert_filters_rail_mounted_exactly_once_outside_a_facets_closure`] fails, then revert.

const MOD_RS: &str = include_str!("../src/alerts/mod.rs");
const FILTERS_RS: &str = include_str!("../src/alerts/filters.rs");

/// Collapses runs of whitespace to single spaces — used for the mount-site checks, where the
/// exact indentation the formatter picked is not the thing under test.
fn normalize_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Strips `//`, `///` and `//!` line comments (anything from the first `//` on a line to its end).
/// Good enough for this file: no string literal in `filters.rs` contains `//`.
fn strip_line_comments(s: &str) -> String {
    s.lines()
        .map(|line| match line.find("//") {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn alert_filters_rail_mounted_exactly_once_outside_a_facets_closure() {
    let count = MOD_RS.matches("<AlertFiltersRail").count();
    assert_eq!(
        count, 1,
        "AlertFiltersRail must be mounted exactly once in alerts/mod.rs, found {count}"
    );

    let idx = MOD_RS.find("<AlertFiltersRail").expect("mount site");
    let before = &MOD_RS[..idx];
    // Look at a generous raw window before the mount site, normalise it, then take the last 160
    // normalised characters — the window this test actually judges.
    let raw_window_start = before.len().saturating_sub(600);
    let raw_window = &before[raw_window_start..];
    let normalized = normalize_whitespace(raw_window);
    let tail_start = normalized
        .char_indices()
        .rev()
        .nth(159)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let tail = &normalized[tail_start..];

    for forbidden in ["move ||", "facets.get()", "facets.with("] {
        assert!(
            !tail.contains(forbidden),
            "the 160 characters immediately before <AlertFiltersRail must not re-enter a facets \
             closure, but found {forbidden:?} in: {tail:?}"
        );
    }
}

#[test]
fn alert_filters_rail_receives_the_facets_signal_not_a_closure_bound_value() {
    let idx = MOD_RS.find("<AlertFiltersRail").expect("mount site");
    let close_idx = MOD_RS[idx..]
        .find("/>")
        .map(|i| idx + i)
        .expect("closing />");
    let element = normalize_whitespace(&MOD_RS[idx..close_idx]);
    assert!(
        element.contains("facets=facets"),
        "expected the mount site to pass facets=facets (the signal itself), got: {element:?}"
    );
}

#[test]
fn pc_bound_field_input_commits_on_change_not_on_input() {
    let start_marker = "\nfn pc_bound_field(";
    let start = FILTERS_RS
        .find(start_marker)
        .expect("fn pc_bound_field definition");
    let body_start = start + 1; // resume scanning right at "fn pc_bound_field("
    let rest = &FILTERS_RS[body_start..];
    let delimiters = ["\nfn ", "\npub fn ", "\npub(crate) fn ", "\n#["];
    // Skip past the definition's own opening line before hunting for the NEXT top-level item.
    let after_signature = rest
        .find(") -> impl IntoView {")
        .map(|i| i + 1)
        .unwrap_or(0);
    let hunt_from = &rest[after_signature..];
    let end_offset = delimiters
        .iter()
        .filter_map(|d| hunt_from.find(d))
        .min()
        .unwrap_or(hunt_from.len());
    let body = &rest[..after_signature + end_offset];

    let input_count = body.matches("<input").count();
    assert_eq!(
        input_count, 1,
        "pc_bound_field must render exactly one <input>, found {input_count}"
    );

    let input_idx = body.find("<input").unwrap();
    let close_idx = body[input_idx..]
        .find("/>")
        .map(|i| input_idx + i)
        .expect("input's closing />");
    let input_attrs = normalize_whitespace(&body[input_idx..close_idx]);

    assert!(
        input_attrs.contains("on:change"),
        "input must commit on:change: {input_attrs:?}"
    );
    assert!(
        !input_attrs.contains("on:input"),
        "input must NOT commit on:input (per-keystroke), which is the 46-UAT test 3 regression: \
         {input_attrs:?}"
    );
    assert!(
        input_attrs.contains("min=\"0\""),
        "input must be bounded min=\"0\": {input_attrs:?}"
    );
    assert!(
        input_attrs.contains("max=\"1\""),
        "input must be bounded max=\"1\": {input_attrs:?}"
    );
    assert!(
        input_attrs.contains("step=PC_INPUT_STEP"),
        "input must use the shared PC_INPUT_STEP constant: {input_attrs:?}"
    );
}

#[test]
fn pc_bound_field_has_exactly_two_call_sites_for_min_and_max() {
    let stripped = strip_line_comments(FILTERS_RS);
    let marker = "pc_bound_field(";
    let mut call_sites = Vec::new();
    let mut search_start = 0;
    while let Some(pos) = stripped[search_start..].find(marker) {
        let abs = search_start + pos;
        // Exclude the `fn pc_bound_field(` definition itself: the three bytes immediately before
        // this occurrence spell "fn ".
        let is_definition = abs >= 3 && &stripped[abs - 3..abs] == "fn ";
        if !is_definition {
            call_sites.push(abs);
        }
        search_start = abs + marker.len();
    }
    assert_eq!(
        call_sites.len(),
        2,
        "expected exactly two pc_bound_field(...) call sites (Minimum and Maximum P_c), found {}",
        call_sites.len()
    );

    let mut found_min = false;
    let mut found_max = false;
    for &abs in &call_sites {
        let end = (abs + 300).min(stripped.len());
        let snippet = &stripped[abs..end];
        if snippet.contains("\"alert-filter-min-score\"") {
            found_min = true;
        }
        if snippet.contains("\"alert-filter-max-score\"") {
            found_max = true;
        }
    }
    assert!(
        found_min,
        "one pc_bound_field call site must pass \"alert-filter-min-score\""
    );
    assert!(
        found_max,
        "one pc_bound_field call site must pass \"alert-filter-max-score\""
    );
}

/// 46-REVIEW-GAPS WR-02: an inverted `P_c` range (`min > max`) must surface a visible hint inside
/// the `P_c range` group, otherwise the empty result set reads exactly like "no alerts match".
/// Source-level only (no DOM runner): the hint must be gated on the pure `pc_range_inverted`
/// predicate (unit-tested in `filters.rs`), read both bound signals, sit between the Maximum `P_c`
/// field and the "Detected between" group, and never read `facets` — the hint's own `move ||`
/// closure must not become a new facets-reactive scope around the `P_c` inputs.
#[test]
fn pc_range_group_renders_an_inverted_range_hint() {
    let stripped = strip_line_comments(FILTERS_RS);

    let gate = "pc_range_inverted(min_score.get(), max_score.get())";
    let gate_count = stripped.matches(gate).count();
    assert_eq!(
        gate_count, 1,
        "expected exactly one inverted-range hint gated on `{gate}`, found {gate_count}"
    );

    let max_field = stripped
        .find("\"alert-filter-max-score\"")
        .expect("Maximum P_c field call site");
    let detected_between = stripped
        .find("\"Detected between\"")
        .expect("Detected between group");
    let gate_idx = stripped.find(gate).unwrap();
    assert!(
        max_field < gate_idx && gate_idx < detected_between,
        "the inverted-range hint must render inside the P_c range group (after the Maximum P_c \
         field, before \"Detected between\")"
    );

    let hint_block = &stripped[gate_idx..detected_between];
    assert!(
        hint_block.contains("{PC_RANGE_INVERTED_HINT}"),
        "the gated block must render PC_RANGE_INVERTED_HINT"
    );
    assert!(
        hint_block.contains("var(--destructive)"),
        "the hint must use the --destructive token so it is not mistaken for help text"
    );
    let hint_end = hint_block
        .find("})}")
        .expect("the hint closure must close with `})}`");
    assert!(
        !hint_block[..hint_end].contains("facets"),
        "the inverted-range hint closure must not read facets"
    );
}
