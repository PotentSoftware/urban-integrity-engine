//! `CanvasFilterRequest` / `CanvasFilterView` — CNV-03's five canvas filters on the wire.
//!
//! # Structured pickers, and why that is a product decision
//!
//! CNV-03's own parenthesis is "structured pickers (no query DSL)". The audience is an analyst,
//! not a query author, and a text box that gets parsed is a query language whoever wrote it calls
//! it or not. So every field below is one of four things and nothing else: a **checklist
//! selection** (`node_kinds`, `edge_types`), a **picked id** (`ward_id`), a **date**
//! (`from_date` / `to_date`) or a **number** (`min_p_c`). There is no field on this request that a
//! server parses into a predicate — the predicate *is* the shape of the struct.
//!
//! That also makes the request the same kind of object `AlgorithmRequest` (plan 44-01) already is,
//! which is the point: the tool rail's edge-type checklist and typed parameter forms are the idiom
//! this extends rather than a second filter vocabulary on the same page.
//!
//! # The rule every filter here keeps: remove on evidence, never on the absence of it
//!
//! Three of the five filters test a fact that this corpus often does not record.
//!
//! * **Dates.** Measured 2026-09-15 and restated in [`crate::temporal`]: `effective_to` is null on
//!   511,754 of 511,754 `EdgeAdded` events and `effective_from` on 384,363 of them. A date range
//!   that silently dropped every undated edge would empty the canvas on almost any input, and the
//!   analyst would read that as a finding about the entity rather than as a property of the
//!   dataset.
//! * **`P_c`.** Only an entity named by a `MotifDetected` alert carries one at all.
//! * **Edge type.** A node with no edges inside the current view has no edge type to test.
//!
//! So the absence of the evidence is never assumed to mean failure: it is either kept (the edge
//! type rule, and the default for the other two) or it is **the analyst's own explicit choice**,
//! made through [`MissingEvidence`] — a two-option picker, not a hidden default. And
//! [`FilterFacets`] states the size of each untestable population, so the choice is made against a
//! number rather than against a guess.

use serde::{Deserialize, Serialize};

use crate::algorithm::Truncation;
use crate::entity_ref::EntityRef;

/// The `P_c` threshold's picker options — **the one option list in CNV-03 that is not measured
/// from the data**, and the reason is worth stating rather than leaving as an apparent oversight.
///
/// These are not a property of any population: they are this project's own risk thresholds, the
/// ones `queries::classify_risk` has classified an alert score by since v1 (`>= 0.9` critical,
/// `>= 0.7` high, `>= 0.5` medium), and the same three words the entity profile's alert rows
/// already carry as a severity band. Bands measured off the data would be a different set of
/// numbers on every analysis, and two screens in one product would then disagree about what "high"
/// means.
///
/// Kept in sync with `classify_risk` **by hand**, the same way `tool_rail::BETWEENNESS_PIVOT_HELP`
/// is kept in sync with the pivot count: `crates/core` is not reachable from a wasm target, so
/// there is no import that would make this automatic. If those thresholds change, this changes.
pub const P_C_THRESHOLD_BANDS: [(&str, f64); 3] = [
    ("Medium and above (0.5)", 0.5),
    ("High and above (0.7)", 0.7),
    ("Critical (0.9)", 0.9),
];

/// What a filter does with a candidate it has no evidence about.
///
/// Two variants and no default in the serde sense — the client always sends one, so "keep the
/// undated edges" is a visible position on a control rather than something that happened because
/// nobody thought about it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MissingEvidence {
    /// The candidate stays. A record that does not state a date has not stated a date *outside*
    /// the window either.
    Keep,
    /// The candidate goes, because the analyst asked for only what could be tested.
    Exclude,
}

/// CNV-03's five filters, as one request.
///
/// `scope_ids` is what makes a filter **compose** with plan 45-13's expansion rather than replace
/// it: the filter runs over the ids the canvas is currently focused on, and an empty `scope_ids`
/// means the whole population. A filter that ignored the expansion would be a second answer to
/// "what is visible", which is precisely the disagreement CNV-03's save exists to rule out.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CanvasFilterRequest {
    /// The node-kind checklist. **Nothing checked means nothing passes** — that is a legible
    /// state with its own empty-canvas sentence, and it is the honest reading of an empty
    /// checklist. (An empty list silently meaning "everything" is how a control comes to have no
    /// effect at one end of its range.)
    pub node_kinds: Vec<String>,
    /// The edge-type checklist, with the same reading of an empty selection.
    pub edge_types: Vec<String>,
    /// Inclusive lower bound, `YYYY-MM-DD`. `None` is half an open window, not an absent filter —
    /// the range filter is active whenever either bound is set.
    pub from_date: Option<String>,
    /// Inclusive upper bound, `YYYY-MM-DD`, taken to the end of that day.
    pub to_date: Option<String>,
    /// What to do with an edge whose record states no date at all.
    pub undated_edges: MissingEvidence,
    /// A ward node id, picked from [`FilterFacets::wards`]. `None` is no ward filter.
    pub ward_id: Option<String>,
    /// `P_c` floor in `[0, 1]`. `None` is no threshold.
    pub min_p_c: Option<f64>,
    /// What to do with a node that carries no `P_c` at all.
    pub unscored_nodes: MissingEvidence,
    /// The canvas's current focus set (45-13's `visible_ids_with_collapsed`), or empty for the
    /// whole population.
    pub scope_ids: Vec<String>,
}

impl CanvasFilterRequest {
    /// Whether any of the five filters would actually test anything.
    ///
    /// The canvas asks for facets with an inactive filter at mount, and an inactive filter is
    /// answered with the facets and **no id list** — see [`CanvasFilterView::filter_active`].
    ///
    /// `node_kinds` / `edge_types` are active only when they are a *proper* subset of what is in
    /// view, which is why this takes the available sets rather than deciding on its own: a
    /// checklist with every box ticked removes nothing and must not make the canvas start
    /// answering from a filtered id list.
    pub fn is_active(&self, available_kinds: &[String], available_edge_types: &[String]) -> bool {
        let kinds_narrow = !is_superset(&self.node_kinds, available_kinds);
        let edges_narrow = !is_superset(&self.edge_types, available_edge_types);
        kinds_narrow
            || edges_narrow
            || self.from_date.is_some()
            || self.to_date.is_some()
            || self.ward_id.is_some()
            || self.min_p_c.is_some()
    }
}

fn is_superset(selected: &[String], available: &[String]) -> bool {
    available.iter().all(|a| selected.iter().any(|s| s == a))
}

/// One edge of the visible subgraph: what CNV-03's save writes as a `PinTarget::Edge`.
///
/// 45-07 designed edge pins for exactly this: *"the visible edge set is not recoverable from the
/// node set — the canvas shows an edge-type-filtered subset of the induced edges, so re-deriving
/// at export time would silently restore edges the analyst had filtered out."*
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct FilteredEdgeView {
    pub from: String,
    pub to: String,
    pub relation: String,
}

/// How many candidates each filter removed, **attributed in the order the filters ran**.
///
/// A node excluded by three filters is counted once, against the first one that would have
/// removed it, so the five counts sum to exactly `candidate_nodes - node_ids.len()` before the
/// result cap. That invariant is what lets the empty-canvas sentence name a cause instead of
/// listing suspects: it is a partition, not five overlapping tallies.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FilterRemovalCounts {
    pub by_node_type: u64,
    pub by_ward: u64,
    pub by_p_c: u64,
    pub by_date_range: u64,
    pub by_edge_type: u64,
}

impl FilterRemovalCounts {
    pub fn total(&self) -> u64 {
        self.by_node_type + self.by_ward + self.by_p_c + self.by_date_range + self.by_edge_type
    }
}

/// A ward the analyst can pick, with the size of the set picking it would leave.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WardOptionView {
    pub entity: EntityRef,
    /// How many nodes **in the current view** this ward represents, the ward node included.
    pub member_count: u64,
}

/// A checklist option and how many candidates carry it — so a box with nothing behind it says so
/// rather than looking like a box that simply was not ticked.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FacetCountView {
    pub value: String,
    pub count: u64,
}

/// Where the pickers' options come from: **measured over the candidate set**, never a hand-kept
/// list in the browser.
///
/// The one thing a hand-kept list cannot do is tell the analyst that an option is empty. A
/// checklist of eleven edge types over a neighbourhood that contains two of them is ten clicks
/// that change nothing and one that changes everything, with no way to tell which is which.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FilterFacets {
    /// Every node kind present in the candidate set, most common first.
    pub node_kinds: Vec<FacetCountView>,
    /// Every edge type present among the candidate edges, most common first.
    pub edge_types: Vec<FacetCountView>,
    /// Every ward that a candidate node belongs to, or that is itself a candidate.
    pub wards: Vec<WardOptionView>,
    /// How many candidate nodes carry a `P_c` at all. The `P_c` threshold's whole universe: on
    /// this corpus only an entity named by a live `MotifDetected` alert has one.
    pub nodes_with_a_score: u64,
    /// The largest `P_c` in view, so a threshold above it is visibly a threshold nothing can meet.
    pub highest_score: Option<f64>,
    /// How many candidate edges state at least one of `effective_from` / `effective_to`.
    pub edges_with_a_date: u64,
    /// How many state neither. On this corpus this is the overwhelming majority, and it is the
    /// number that turns "the date filter emptied the canvas" from a finding into a fact about
    /// the dataset.
    pub edges_without_a_date: u64,
}

/// What `POST /api/graph/canvas-filter` returns.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CanvasFilterView {
    /// Whether the request actually narrowed anything. `false` means the canvas keeps showing its
    /// own focus set and this response was only ever about [`Self::facets`].
    pub filter_active: bool,
    /// How many nodes the filter ran over — the expansion's focus set, or the whole population.
    pub candidate_nodes: u64,
    /// How many edges it ran over, induced on the candidate nodes.
    pub candidate_edges: u64,
    /// The visible node set, in candidate order. Empty when [`Self::filter_active`] is `false`:
    /// the canvas already knows its own focus set, and echoing three hundred thousand ids back to
    /// prove it would be a payload with no reader.
    pub node_ids: Vec<String>,
    /// The visible edge set — every edge with both endpoints visible that survived the date and
    /// edge-type filters.
    pub edges: Vec<FilteredEdgeView>,
    pub removed: FilterRemovalCounts,
    pub facets: FilterFacets,
    /// Phase 44's mandatory block. A filtered result longer than
    /// [`crate::caps::CANVAS_FILTER_NODE_CAP`] is a **window** on what passed, and a window that
    /// did not say so would be the node cap's defect wearing a filter.
    pub truncation: Truncation,
    /// How many nodes passed every filter before the cap. Equal to `node_ids.len()` when the cap
    /// did not bite, and the number the cap clause subtracts from when it did.
    pub passed_before_cap: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> CanvasFilterRequest {
        CanvasFilterRequest {
            node_kinds: vec!["Entity".to_string(), "Contract".to_string()],
            edge_types: vec!["AwardedTo".to_string()],
            from_date: None,
            to_date: None,
            undated_edges: MissingEvidence::Keep,
            ward_id: None,
            min_p_c: None,
            unscored_nodes: MissingEvidence::Exclude,
            scope_ids: vec![],
        }
    }

    /// The whole request round-trips, and `undated_edges` / `unscored_nodes` carry their two
    /// positions across the wire distinctly.
    ///
    /// PROVE THIS CAN FAIL: renaming `MissingEvidence::Keep`'s serde token to match `Exclude`'s
    /// makes the second assertion fail.
    #[test]
    fn a_filter_request_round_trips_with_both_missing_evidence_positions() {
        for evidence in [MissingEvidence::Keep, MissingEvidence::Exclude] {
            let mut req = request();
            req.undated_edges = evidence;
            req.unscored_nodes = evidence;
            let json = serde_json::to_string(&req).expect("request must serialize");
            let back: CanvasFilterRequest =
                serde_json::from_str(&json).expect("request must deserialize");
            assert_eq!(back, req);
        }
        let keep = serde_json::to_string(&MissingEvidence::Keep).unwrap();
        let exclude = serde_json::to_string(&MissingEvidence::Exclude).unwrap();
        assert_ne!(
            keep, exclude,
            "the two positions must be distinguishable on the wire"
        );
    }

    /// A checklist with every available box ticked is not a filter, and a checklist missing one is.
    ///
    /// This is the gate that decides whether the canvas answers from its own focus set or from a
    /// filtered id list, so it has to be exactly right at the boundary.
    ///
    /// PROVE THIS CAN FAIL: making `is_superset` return `true` unconditionally makes the second
    /// and third assertions fail.
    #[test]
    fn a_fully_ticked_checklist_is_not_an_active_filter_and_one_box_short_is() {
        let kinds = vec!["Entity".to_string(), "Contract".to_string()];
        let edges = vec!["AwardedTo".to_string()];

        let mut all_ticked = request();
        all_ticked.node_kinds = kinds.clone();
        all_ticked.edge_types = edges.clone();
        assert!(
            !all_ticked.is_active(&kinds, &edges),
            "every box ticked removes nothing, so it is not an active filter"
        );

        let mut one_short = all_ticked.clone();
        one_short.node_kinds = vec!["Entity".to_string()];
        assert!(one_short.is_active(&kinds, &edges));

        let mut none_ticked = all_ticked.clone();
        none_ticked.node_kinds = vec![];
        assert!(
            none_ticked.is_active(&kinds, &edges),
            "an empty checklist excludes everything, which is very much an active filter"
        );

        // Non-vacuity: the inactive case above is inactive because of the checklists, not because
        // the other four fields happened to be unset in a way that would mask a broken superset
        // test.
        let mut only_a_date = all_ticked.clone();
        only_a_date.from_date = Some("2019-01-01".to_string());
        assert!(only_a_date.is_active(&kinds, &edges));
        let mut only_a_ward = all_ticked.clone();
        only_a_ward.ward_id = Some("ward:X05999001".to_string());
        assert!(only_a_ward.is_active(&kinds, &edges));
        let mut only_a_score = all_ticked.clone();
        only_a_score.min_p_c = Some(0.5);
        assert!(only_a_score.is_active(&kinds, &edges));
    }

    /// The five removal counts are a partition, so `total()` is the whole removal.
    #[test]
    fn the_removal_counts_add_up_to_the_whole_removal() {
        let counts = FilterRemovalCounts {
            by_node_type: 3,
            by_ward: 1,
            by_p_c: 5,
            by_date_range: 2,
            by_edge_type: 4,
        };
        assert_eq!(counts.total(), 15);
        assert_eq!(FilterRemovalCounts::default().total(), 0);
    }
}
