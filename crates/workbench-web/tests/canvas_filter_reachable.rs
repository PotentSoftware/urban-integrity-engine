//! Plan 45-14 (CNV-03): **the filter surface is mounted, and there is exactly one definition of
//! what is visible.**
//!
//! # What a test in this crate can and cannot prove
//!
//! 45-13 §7.2 established the honest boundary and it has not moved: these are host-side source
//! reads. There is no DOM, no headless browser, no running shim. A source sweep can prove
//! **wiring** — that a component is mounted, that a value comes from somewhere, that a second
//! definition does not exist — and it cannot prove **liveness**.
//!
//! `workbench-web` is a **binary** crate with no library target, so an integration test cannot
//! call its functions. The load-bearing proof — `saved_is_exactly_what_was_visible` — is therefore
//! a unit test inside `src/canvas_filters.rs`, where it can compare the two call paths for real.
//! This file proves that those paths are the ones the page actually mounts.

fn source_of(relative: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()))
}

/// **There is exactly one definition of what is visible, and the save is one of its call sites.**
///
/// The same shape as 45-13's one-renderer sweep, aimed at the thing this requirement forbids. A
/// second definition could arrive as a second function, or as the save quietly re-deriving the set
/// inline, so both are checked — and the tokens are bare names, because 45-13 §7.1 found that a
/// sweep looking for a *call* is walked through by a bare mention.
#[test]
fn there_is_one_definition_of_visible_and_the_save_reads_it() {
    let filters = source_of("src/canvas_filters.rs");
    let page = source_of("src/graph_page.rs");

    assert_eq!(
        filters.matches("pub fn visible_subgraph_ids(").count(),
        1,
        "exactly one definition"
    );
    for source in ["src/graph_page.rs", "src/side_card.rs", "src/tool_rail.rs"] {
        assert!(
            !source_of(source).contains("fn visible_subgraph_ids"),
            "{source} must not define a second one"
        );
    }
    assert!(
        filters.contains("expected_node_ids: visible_subgraph_ids("),
        "the save's payload must BE the visible set, not a set derived beside it"
    );
    assert!(
        page.contains("canvas_filters::save_payload(")
            && page.contains("<SaveVisibleSubgraph payload=save_payload />"),
        "the mounted control must be handed that payload and nothing else"
    );
    // The control must have no other source of ids: one prop, so it cannot form an opinion.
    let component = &filters[filters
        .find("pub fn SaveVisibleSubgraph(")
        .expect("the save control must exist")..];
    assert!(
        component.starts_with("pub fn SaveVisibleSubgraph(payload: Signal<SaveSubgraphRequest>)"),
        "the save control takes exactly one prop"
    );
}

/// The filter block and its five pickers are mounted, and there is **no text input among them**.
///
/// The no-DSL requirement is not a claim about intent: it is a claim about what is on the page.
/// The only `type="text"` in the CNV-03 surface is the investigation's name, which is a name and
/// not a query, and it is asserted by position rather than by counting to one.
///
/// PROVE THIS CAN FAIL: add a `<input type="text">` bound to any filter signal and the count
/// assertion fails; delete `<CanvasFilters` from `graph_page.rs` and the mount assertion fails.
#[test]
fn the_five_pickers_are_mounted_and_none_of_them_is_a_text_box() {
    let filters = source_of("src/canvas_filters.rs");
    let page = source_of("src/graph_page.rs");

    assert!(page.contains("<CanvasFilters state=canvas_filter_state />"));
    assert!(page.contains("<SaveVisibleSubgraph payload=save_payload />"));

    // Five pickers, each reading its own signal off the shared struct.
    for control in [
        "state.node_kinds",
        "state.edge_types",
        "state.from_date",
        "state.to_date",
        "state.ward_id",
        "state.min_p_c",
    ] {
        assert!(
            filters.contains(control),
            "the {control} picker must be rendered"
        );
    }

    let text_inputs = filters.matches("type=\"text\"").count();
    assert_eq!(
        text_inputs, 1,
        "the only text input in the CNV-03 surface is the investigation's NAME"
    );
    let name_input = filters
        .find("id=\"save-subgraph-name\"")
        .expect("the name field must exist");
    let only_text = filters.find("type=\"text\"").expect("counted above");
    assert!(
        only_text.abs_diff(name_input) < 200,
        "the one text input must be the name field, not a filter field that happens to be elsewhere"
    );

    // And the checklists extend the tool rail's idiom rather than a second one beside it.
    assert!(
        filters.contains("tool_rail::checklist_row("),
        "both checklists must render through the tool rail's own row"
    );
    assert!(
        filters.contains("tool_rail::date_field(") && filters.contains("P_C_THRESHOLD_BANDS"),
        "the date bounds and the score bands are structured controls, never parsed strings"
    );
}

/// **An empty canvas says why, on the canvas.**
///
/// `empty_canvas_sentence` composing the right words is worth nothing if nothing renders it — the
/// explanation would be produced for a function nobody reads, which from the operator's chair is
/// indistinguishable from a blank rectangle. 45-13 §7.2's lesson applies and bounds what this can
/// prove: it asserts the block reads a prop that is derived from the visible set, not that the
/// signal is ever updated.
///
/// PROVE THIS CAN FAIL: delete the `empty_sentence.get().map(...)` block from `graph_page.rs`, or
/// derive `empty_sentence` from a constant `None` — the first fails the render assertion, the
/// second fails the derivation assertion. Both mutations were applied and reverted.
#[test]
fn the_empty_canvas_sentence_is_rendered_over_the_canvas() {
    let page = source_of("src/graph_page.rs");
    let filters = source_of("src/canvas_filters.rs");

    assert!(
        filters.matches("pub fn empty_canvas_sentence(").count() == 1,
        "exactly one definition of why the canvas is empty"
    );
    assert!(
        page.contains("canvas_filters::empty_canvas_sentence("),
        "the page must compose the sentence"
    );
    assert!(
        page.contains("empty_sentence.get().map(|sentence|"),
        "and it must render it"
    );
    // The value has to come from somewhere: the derivation must read the visible set, not a
    // constant. This is the shape 45-13 §7.2 had to add after a dead block passed its own proof.
    assert!(
        page.contains("payload.expected_node_ids.len()"),
        "the sentence must be derived from the same visible set the save sends"
    );
    assert!(
        page.contains("role=\"status\""),
        "an explanation nobody's screen reader reaches is half an explanation"
    );
}

/// The empty-canvas explanation is **not** a status-line clause, and the status line's own filter
/// clause is not an empty-canvas explanation. Two surfaces, two jobs.
///
/// A single sentence doing both is how "filters keep 0 of 31 nodes" comes to be the whole answer
/// an analyst gets for an empty screen — technically true, and silent about which of the two facts
/// it is.
#[test]
fn the_status_line_and_the_empty_canvas_explanation_are_two_surfaces() {
    let filters = source_of("src/canvas_filters.rs");
    let status = source_of("src/graph_status_line.rs");

    assert!(
        status.contains("canvas_filters::filter_status_clause("),
        "the status line reports the filter's counts"
    );
    assert!(
        !status.contains("empty_canvas_sentence"),
        "and it must not be where an empty canvas is explained"
    );
    assert!(
        filters.contains("pub fn filter_status_clause(")
            && filters.contains("pub fn empty_canvas_sentence("),
        "both live here, and they are two functions"
    );
}
