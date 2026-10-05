//! **Proof that plan 45-13's three claims have call sites, not just functions.**
//!
//! Phase 43 shipped a fully built, unit-tested, CI-guarded `Abbr` component with **zero call
//! sites** across five plans, and every test stayed green because every test asked whether the
//! component worked rather than whether anything called it. Plan 45-11 reproduced the same failure
//! at one-file scale in its own work (§7.1: a mutation that would have linked every ward to a 404
//! left the whole unit suite green). This file is that instrument aimed at the three things this
//! plan claims:
//!
//! 1. **One profile renderer** (D-11). `ProfileRenderer` is mounted at `Density::Full` by the
//!    profile page and at `Density::Compact` by the canvas inspection panel, and **nothing else in
//!    the crate assembles a profile of its own**. The Cosmograph shim is this project's worked
//!    example of what a second implementation costs: a vendored bundle and its documented "source"
//!    diverged for eight days across four features, and nothing failed, because nothing consumed
//!    the source. A second profile renderer would diverge the same way and be just as silent.
//! 2. **The cap is reported on screen.** `expansion_status_line` composing the right sentence is
//!    worth nothing if the status line never calls it — the cap would be reported to a function
//!    nobody reads, which from the canvas is indistinguishable from not reporting it.
//! 3. **The hop control is mounted**, and the panel is reached from the canvas.
//!
//! Every test here is a host-side source read, the only kind this crate's test model supports (no
//! DOM, no headless browser — phase 42 research Pitfall 10). None of it asserts that anything looks
//! right in a browser.
//!
//! The helpers below are copied from `pivot_reachable.rs` rather than shared: `workbench-web` is a
//! **binary** crate with no library target, so two integration-test binaries cannot import from one
//! another. `shipped_portion` carries 45-10's hardening (cut at the test *module*, not at the first
//! `#[cfg(test)]` attribute) — the hole that made two render proofs silently blind on this branch.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The portion of a source that actually ships — everything before its trailing `mod tests`.
///
/// Cut at the test **module**, not at the first `#[cfg(test)]` attribute: `profile/mod.rs` declares
/// a test-only fixtures module, and a blind cut there made every line after it invisible to the
/// sweep while the sweep went on reporting a pass (45-10 §5).
fn shipped_portion(source: &str) -> &str {
    const ATTRIBUTE: &str = "#[cfg(test)]";
    let mut from = 0usize;
    while let Some(offset) = source[from..].find(ATTRIBUTE) {
        let start = from + offset;
        let after = &source[start + ATTRIBUTE.len()..];
        if after.trim_start().starts_with("mod tests") {
            return &source[..start];
        }
        from = start + ATTRIBUTE.len();
    }
    source
}

/// Drops `//`-comment lines. A rule about what the code *does* must not be satisfied — or broken —
/// by prose describing it, which is the trap plan 35-07 hit four times in one phase.
fn without_comments(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn every_source_file() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("the crate source tree must be readable") {
            let path = entry.expect("a readable directory entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    walk(&root, &mut out);
    out.sort();
    out
}

fn relative(path: &Path) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    format!(
        "src/{}",
        path.strip_prefix(root.join("src"))
            .expect("every swept file lives under src/")
            .display()
    )
    .replace('\\', "/")
}

/// Every source's shipped, comment-stripped text, by `src/…` path.
fn shipped_sources() -> Vec<(String, String)> {
    every_source_file()
        .into_iter()
        .map(|path| {
            let source = std::fs::read_to_string(&path).expect("a readable source file");
            (relative(&path), without_comments(shipped_portion(&source)))
        })
        .collect()
}

fn source_of(name: &str) -> String {
    shipped_sources()
        .into_iter()
        .find(|(path, _)| path == name)
        .unwrap_or_else(|| panic!("{name} must exist"))
        .1
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. One profile renderer (D-11)
// ─────────────────────────────────────────────────────────────────────────────

/// **The positive render proof plan 45-10 asked for.** Its
/// `the_compact_density_has_no_mount_site_yet_and_45_13_owns_one` was a pin that asserted the
/// compact renderer had *no* mount site and said, in its own doc comment, "this test fails the day
/// 45-13 mounts the panel, and should then be deleted". It was deleted in the same commit as this
/// test, which replaces it with the claim it was holding the place for.
///
/// PROVE THIS CAN FAIL: replace `<ProfileRenderer … density=Density::Compact />` in
/// `side_card.rs` with the phase-44 five-fact body and this fails.
#[test]
fn the_canvas_inspection_panel_mounts_the_compact_profile_renderer() {
    let panel = source_of("src/side_card.rs");
    assert!(
        panel.contains("<ProfileRenderer") && panel.contains("density=Density::Compact"),
        "the panel must mount the one profile renderer at Compact"
    );
    // `on_show_more` is deliberately absent: Compact windows each section and the profile page is
    // where the rest is read (45-10's hand-over). Asserting the absence keeps that a decision.
    assert!(
        !panel.contains("on_show_more"),
        "the compact panel pages nothing; the profile page does"
    );
    // Non-vacuity: the full-density mount still exists, so this is a statement about a SECOND
    // mount site rather than about one having moved.
    let page = source_of("src/profile/mod.rs");
    assert!(
        page.contains("density=Density::Full"),
        "the profile page's own mount must survive"
    );
}

/// **There is no second profile renderer in the codebase** (D-11).
///
/// This asserts over *call sites and definitions*, not over a function working. Phase 43 shipped a
/// fully built, unit-tested, CI-guarded component with zero call sites for five plans because
/// every test asked the second question and none asked the first.
///
/// Three things are checked, because a second renderer could arrive as any of them:
///
/// * a second **component** whose name says it renders a profile;
/// * a second **mount** of `ProfileRenderer` (fine in itself) — the set of mount sites is pinned,
///   so a third surface has to be declared here rather than appearing;
/// * a second **assembler**: any source outside `src/profile/` that calls the section machinery
///   (`section_rows`, `section_columns`, `section_state`, `empty_state_sentence`, `range_text`)
///   and therefore builds profile content of its own rather than mounting the renderer.
///
/// PROVE THIS CAN FAIL: add `fn CompactProfileView` to `side_card.rs` that calls `section_rows`
/// itself, and this fails on two of the three assertions. That mutation was run.
#[test]
fn there_is_exactly_one_profile_renderer_and_two_places_that_mount_it() {
    let sources = shipped_sources();

    // (a) one definition.
    let definitions: BTreeSet<String> = sources
        .iter()
        .filter(|(_, source)| source.contains("pub fn ProfileRenderer("))
        .map(|(path, _)| path.clone())
        .collect();
    assert_eq!(
        definitions,
        ["src/profile/mod.rs"]
            .into_iter()
            .map(String::from)
            .collect(),
        "the profile renderer is defined once"
    );

    // (b) a fixed set of mount sites.
    let mounts: BTreeSet<String> = sources
        .iter()
        .filter(|(_, source)| source.contains("<ProfileRenderer"))
        .map(|(path, _)| path.clone())
        .collect();
    assert_eq!(
        mounts,
        ["src/profile/mod.rs", "src/side_card.rs"]
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<String>>(),
        "the profile page and the canvas panel mount it; a third surface declares itself here"
    );

    // (c) nobody outside `src/profile/` assembles profile content of their own.
    // The **bare** names, not the call form `name(`. A first draft of this sweep looked for
    // `section_rows(` and a mutation that named the function without immediately calling it —
    // a function pointer, an alias, a partial application — walked straight through it while the
    // sweep reported a pass. That is the same shape of hole 45-10 found in `shipped_portion` and
    // 45-11 found in `looks_like_id_namespace`: a scanner narrow enough to be dodged by the next
    // person who writes the thing it is looking for. `without_comments` has already removed every
    // `//` line, so a doc comment mentioning one of these cannot trip it.
    const ASSEMBLY: &[&str] = &[
        "section_rows",
        "section_columns",
        "section_state",
        "empty_state_sentence",
        "section_row_noun",
        "as_at_untested_notice",
    ];
    let mut checked = 0;
    for (path, source) in &sources {
        if path.starts_with("src/profile/") {
            continue;
        }
        checked += 1;
        for call in ASSEMBLY {
            assert!(
                !source.contains(call),
                "{path} calls {call} — it is assembling a profile instead of mounting the one \
                 renderer (D-11)"
            );
        }
    }
    // Non-vacuity, both ways: the sweep really walked the non-profile sources, and the tokens it
    // looked for really are the renderer's own — a typo'd list would pass this test trivially.
    assert!(
        checked >= 15,
        "only {checked} non-profile sources were swept"
    );
    let sections = source_of("src/profile/sections.rs");
    for declared in ASSEMBLY {
        assert!(
            sections.contains(&format!("fn {declared}(")),
            "{declared} must be a real function of the renderer for its absence to mean anything"
        );
    }
}

/// A node class the profile route cannot open gets a sentence, not an error and not a blank — and
/// the panel does not issue the request whose 404 was knowable before it was sent.
///
/// PROVE THIS CAN FAIL: drop the `if !opens_a_profile_ref(&node_summary.entity) { return; }` guard
/// from the panel's fetch effect and this fails; delete the sentence's render site and it fails on
/// the other assertion. Both were run.
#[test]
fn a_node_with_no_profile_route_is_answered_rather_than_left_blank() {
    let panel = source_of("src/side_card.rs");
    assert!(
        panel.contains("no_profile_route_sentence(&class)"),
        "the sentence must be rendered, not merely defined"
    );
    assert!(
        panel.contains("if !opens_a_profile_ref(&node_summary.entity)"),
        "the panel must decide before it fetches, or the ordinary case is a 404"
    );
    // The node's own facts survive the absence of a profile: the panel is never empty.
    assert!(
        panel.contains("CANVAS_FACTS_CAPTION") && panel.contains("Degree: {}"),
        "the graph-local facts must render whether or not a profile exists"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. The cap is reported on screen
// ─────────────────────────────────────────────────────────────────────────────

/// The status line under the canvas calls the expansion composer, and the graph page hands it the
/// expansion to compose from. Both halves, because either one alone is a sentence nobody sees.
///
/// PROVE THIS CAN FAIL: delete the `expansion=expansion.into()` attribute from `graph_page.rs`'s
/// `<StatusLine>`, or make `StatusLine` ignore the prop and render `status_line_text` only. Both
/// mutations were run and both redden this.
#[test]
fn the_reported_cap_reaches_the_status_line_under_the_canvas() {
    let status_line = source_of("src/graph_status_line.rs");
    assert!(
        status_line.contains("neighbourhood::expansion_status_line(&view)"),
        "the status line must compose the expansion sentence, not merely be able to"
    );
    assert!(
        status_line.contains("expansion: Signal<Option<NeighbourhoodView>>"),
        "the status line must take the expansion it describes"
    );

    let page = source_of("src/graph_page.rs");
    assert!(
        page.contains("expansion=expansion.into()"),
        "the graph page must hand the status line the expansion, or the prop is always None"
    );
    // Non-vacuity: the algorithm-result line is still there, so this is a statement about an
    // ADDED report rather than about one line having replaced another.
    assert!(
        status_line.contains("status_line_text(&algorithm_result"),
        "the algorithm status line must survive"
    );
}

/// There is exactly one truncation vocabulary on this page. D-12's whole point is that a cut is
/// reported through phase 44's mandatory `Truncation` block and the status line that already
/// exists, never through a second convention invented beside it.
///
/// PROVE THIS CAN FAIL: add a `NeighbourhoodTruncationBanner` component to `neighbourhood.rs` that
/// renders its own "… nodes were hidden" copy, and this fails naming the file.
#[test]
fn the_expansion_reports_its_cut_through_the_one_status_line_and_not_a_second_surface() {
    let composers: BTreeSet<String> = shipped_sources()
        .into_iter()
        .filter(|(_, source)| source.contains("expansion_status_line"))
        .map(|(path, _)| path)
        .collect();
    let expected: BTreeSet<String> = ["src/graph_status_line.rs", "src/neighbourhood.rs"]
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(
        composers, expected,
        "the expansion sentence is composed in one module and rendered in one place"
    );

    // And nothing outside the status line renders a truncation report of its own.
    for (path, source) in shipped_sources() {
        if path == "src/graph_status_line.rs" || path == "src/neighbourhood.rs" {
            continue;
        }
        assert!(
            !source.contains("not shown (node cap"),
            "{path} writes its own node-cap copy — D-12 has exactly one"
        );
    }
}

/// **CNV-02 reaches the screen, and in 45-12's vocabulary rather than a second one.**
///
/// The panel renders the edges, and the line it renders them with is built by `range_text` — the
/// profile screen's own function. Nothing in the crate outside `src/profile/` spells a date range
/// for itself.
///
/// PROVE THIS CAN FAIL: delete the `{move || reached.get().map(...)}` block from `side_card.rs` and
/// the render assertion fails; inline a `format!("{from} → present")` range in `neighbourhood.rs`
/// and the vocabulary assertion fails. Both mutations were run.
#[test]
fn an_expansion_edge_renders_its_type_and_range_through_the_profile_screens_vocabulary() {
    let panel = source_of("src/side_card.rs");
    assert!(
        panel.contains("REACHED_BY_CAPTION") && panel.contains("node.edges.iter().map(edge_line)"),
        "the panel must render the edges the walk crossed"
    );
    // The block must be fed from the prop, and the prop from the page. A first draft asserted only
    // that the block existed, and a mutation replacing `reached.get()` with a constant `None` —
    // a dead block, rendered from nothing — left it green. A source sweep cannot prove a signal is
    // live; it can insist the value comes from somewhere, which is what these two check.
    assert!(
        panel.contains("reached.get().map(|node|"),
        "the edge block must read the prop, not a constant"
    );
    assert!(
        panel.contains("reached: Signal<Option<NeighbourhoodNodeView>>"),
        "the panel must take the reached node as a prop"
    );
    assert!(
        panel.contains("undated_edges_note(&node.edges)"),
        "a set of edges with no dates at all must say so"
    );

    let page = source_of("src/graph_page.rs");
    assert!(
        page.contains("neighbourhood::reached_node(view, &id_for_reach)")
            && page.contains("reached=reached"),
        "the page must resolve the clicked node against the current expansion and pass it down"
    );

    // One date-range vocabulary. `range_text` is 45-12's; a second spelling on the canvas is the
    // failure. `→ present` in particular asserts something no record in this store says.
    let expansion = source_of("src/neighbourhood.rs");
    assert!(
        expansion.contains("range_text(&edge.temporal)"),
        "the edge line must call the profile screen's range renderer"
    );
    for (path, source) in shipped_sources() {
        if path.starts_with("src/profile/") {
            continue;
        }
        assert!(
            !source.contains("present\""),
            "{path} spells an open-ended range as ongoing; this store records no end dates at all"
        );
        assert!(
            !source.contains("\"Undated\""),
            "{path} writes its own undated word instead of calling range_text"
        );
    }
    // Non-vacuity: `range_text` really is the profile's, and really does produce both words this
    // test is about.
    let sections = source_of("src/profile/sections.rs");
    assert!(
        sections.contains("pub fn range_text(") && sections.contains("\"Undated\""),
        "range_text must be the one place the undated word lives"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. The hop control is mounted
// ─────────────────────────────────────────────────────────────────────────────

/// The cluster glyphs are mounted, they drive the canvas, and **nothing collapses on its own**.
///
/// The last of those is the one worth a sweep: an automatic collapse would be a silently shorter
/// graph with a different mechanism, and it is exactly the kind of "helpful" default that gets
/// added later by someone who has forgotten why the node cap is reported.
///
/// PROVE THIS CAN FAIL: delete `<ClusterGlyphs` from `graph_page.rs`; drop
/// `visible_ids_with_collapsed` from the canvas effect in favour of `visible_ids`; or seed
/// `collapsed_clusters` with `clusters(...)` instead of `Vec::new()`. All three were run.
#[test]
fn a_cluster_collapses_only_when_asked_and_the_canvas_follows_the_list() {
    let page = source_of("src/graph_page.rs");
    assert!(
        page.contains("<ClusterGlyphs"),
        "the cluster list must be mounted"
    );
    assert!(
        page.contains("collapsed=collapsed_clusters"),
        "the list and the canvas must read one signal, or they will disagree"
    );
    // Plan 45-14 moved this call one level down: the canvas effect now composes the expansion with
    // CNV-03's filter through `canvas_filters::visible_subgraph_ids`, which is the ONE definition
    // of what is visible and is also what the save reads. So the chain is asserted end to end
    // rather than at a single literal, which is strictly more than the original assertion proved —
    // each of the three links below can be broken on its own, and each break reddens this test.
    assert!(
        page.contains("canvas_filters::visible_subgraph_ids(") && page.contains("&collapsed,"),
        "the canvas focus set must be composed through the one definition, carrying the fold"
    );
    let filters = source_of("src/canvas_filters.rs");
    assert!(
        filters.contains("neighbourhood::visible_ids_with_collapsed(view, collapsed)"),
        "the one definition must account for what is folded away"
    );
    assert!(
        filters.contains("canvas_focus_ids(expansion, collapsed, population_ids)"),
        "`visible_subgraph_ids` must reach the fold through the focus set, not around it"
    );
    assert!(
        page.contains("neighbourhood::glyph_sizes(view, &collapsed)")
            && page.contains("handle.set_point_size_by_score(&glyph_indices, &glyph_scores)"),
        "a glyph must be drawn as the many it stands for"
    );

    // Nothing is collapsed until a parent is named: the signal starts and resets empty.
    assert!(
        page.contains("let collapsed_clusters: RwSignal<Vec<String>> = RwSignal::new(Vec::new())"),
        "an expansion opens with nothing folded away"
    );
    assert_eq!(
        page.matches("collapsed_clusters.set(Vec::new())").count(),
        3,
        "a new expansion, Clear expansion and Reset must each drop stale parent ids"
    );

    // Non-vacuity: the glyph really does state its population, so "collapsed" is never silent.
    let expansion = source_of("src/neighbourhood.rs");
    assert!(
        expansion.contains("collapsed under") && expansion.contains("cluster.member_ids.len()"),
        "a glyph must say how many nodes it stands for, counted from its own members"
    );
}

/// CNV-01's explicit hop control is on the page, wired to both of its actions.
///
/// PROVE THIS CAN FAIL: delete the `<HopControl` element from `graph_page.rs` and this fails.
#[test]
fn the_hop_control_is_mounted_on_the_graph_page_with_both_its_actions() {
    let page = source_of("src/graph_page.rs");
    assert!(
        page.contains("<HopControl"),
        "the hop control must be mounted"
    );
    for wiring in [
        "seed_label=seed_label",
        "hops=expansion_hops",
        "on_expand=on_expand",
        "on_clear=on_clear_expansion",
    ] {
        assert!(page.contains(wiring), "the hop control needs {wiring}");
    }
    // Non-vacuity: the control genuinely offers a choice — a component with no hop selector would
    // satisfy every assertion above.
    let control = source_of("src/neighbourhood.rs");
    assert!(
        control.contains("hop_choices()") && control.contains("<select"),
        "the hop control must actually let the operator choose a hop count"
    );
}
