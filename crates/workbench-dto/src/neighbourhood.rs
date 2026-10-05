//! `NeighbourhoodView` — CNV-01's expansion, and a node cap that **cannot** be applied without
//! being reported.
//!
//! # The failure this module is shaped against
//!
//! CNV-01's own words are "reported, never silently truncated", and D-12 says the report reuses
//! phase 44's mandatory [`Truncation`] block rather than a second convention. Phase 44 made
//! `Truncation` a non-`Option` field on `AlgorithmResult` so a handler could not *omit* it — but a
//! handler could still write `Truncation { truncated: false, .. }` beside a list it had just cut,
//! and nothing would fail. That is one careless line away, and the resulting screen is a shorter
//! graph that says it is complete.
//!
//! So the cut and the report are **one operation**, and the type system carries it:
//!
//! * [`ReportedTruncation`] wraps `Truncation` in a newtype whose field is **private to this
//!   module**. No code outside can construct one.
//! * [`NeighbourhoodView`] requires a `ReportedTruncation`, so no code outside can construct a
//!   view either.
//! * [`NeighbourhoodView::cut`] is the only constructor. It takes the complete node list and the
//!   cap, performs the cut, and derives the truncation from the same two numbers — so a view
//!   reporting `truncated: false` while `nodes.len() < total_discovered` is not something a
//!   handler can forget to avoid; it is something it cannot express.
//!
//! `Deserialize` still reaches the private field (serde's derive is generated inside this module),
//! which is what lets the browser read a response. That is an inbound path from the server that
//! already did the cut, not a second producer.
//!
//! # What the renderer composes, and what it does not
//!
//! `detail` is a server-side statement of what was cut, never the pre-rendered UI status line —
//! the same rule `Truncation`'s own doc comment states. The phase 45 UI spec's line
//! ("Showing {n} of {total} nodes within {h} hops — {cut} not shown (node cap {cap}).") is composed
//! in the frontend from [`NeighbourhoodView`]'s fields, every one of which is present for that
//! purpose: `nodes.len()`, `total_discovered`, `hops`, and `node_cap`.

use serde::{Deserialize, Serialize};

use crate::algorithm::Truncation;
use crate::entity_ref::EntityRef;
use crate::temporal::Temporal;

/// One node of an expanded neighbourhood — the wire mirror of
/// `crates/core::persistence::queries::NeighbourhoodNode`, with the id resolved to a named
/// [`EntityRef`] so the canvas can label it without a second lookup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NeighbourhoodNodeView {
    pub entity: EntityRef,
    /// Hops from the seed; always `>= 1` (the seed is not a member of its own neighbourhood).
    pub hop: u32,
    /// The edges joining this node to [`Self::reached_from`] — the edges the walk crossed, not
    /// every edge incident on the node — each with its **type and its recorded date range**
    /// (CNV-02).
    ///
    /// Replaced an earlier `edge_types: Vec<String>`. Two fields, one derived from the other, is
    /// how a type list and a range list come to disagree about how many edges there were;
    /// [`Self::edge_types`] derives that view on demand instead.
    pub edges: Vec<NeighbourEdgeView>,
    /// The id of the node this one was first reached through, so an expansion renders as the tree
    /// it is rather than as a flat set.
    pub reached_from: String,
}

impl NeighbourhoodNodeView {
    /// The distinct edge types joining this node to [`Self::reached_from`], in wire order.
    pub fn edge_types(&self) -> Vec<String> {
        let mut out: Vec<String> = self.edges.iter().map(|e| e.edge_type.clone()).collect();
        out.dedup();
        out
    }
}

/// One edge of an expanded neighbourhood: **what kind of relationship it is, and when the record
/// says it was in force** (CNV-02).
///
/// # Why the range is a `Temporal` rather than two date strings
///
/// Because the answer is usually "the record does not say", and that has to be a *statement* rather
/// than an empty cell. `EffectiveRange::Unrecorded` carries the fields that were consulted, which
/// is what separates "this dataset records no dates" from "this edge lost its dates" — and on this
/// corpus the first is the overwhelming majority: measured 2026-09-15, `effective_to` is null on
/// every `EdgeAdded` event in the store and `effective_from` on three quarters of them. The
/// measurement lives in `crate::temporal`'s module doc, which is also where the rule for it does.
///
/// `status` is [`crate::temporal::AsAtStatus::NotFiltered`] on every edge this route emits: the canvas runs no
/// as-at filter, and an edge that has not been tested against a date must not claim to be in force
/// on one. That is 45-06's distinction, kept rather than collapsed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NeighbourEdgeView {
    pub edge_type: String,
    pub temporal: Temporal,
}

/// A [`Truncation`] that can only have come from [`NeighbourhoodView::cut`].
///
/// The private field is the whole mechanism — see this module's doc comment. `#[serde(transparent)]`
/// so the wire shape is exactly phase 44's `Truncation` object and a reader cannot tell (or need
/// to care) that a newtype is enforcing it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct ReportedTruncation(Truncation);

impl std::ops::Deref for ReportedTruncation {
    type Target = Truncation;

    fn deref(&self) -> &Truncation {
        &self.0
    }
}

/// What `GET /api/entity/neighbourhood` returns.
///
/// Construct with [`Self::cut`]; there is no other way, by design.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NeighbourhoodView {
    /// The entities the expansion started from (Phase 50, D-12) — one for a single-id request,
    /// several for a finding-seeded one. Never empty: [`Self::cut`] is the only constructor and a
    /// caller with no seed has nothing to expand, which is a `bad_request` at the handler, not a
    /// `NeighbourhoodView` with an empty list.
    pub seeds: Vec<EntityRef>,
    /// The hop budget actually walked, after the server's clamp — never the raw requested value.
    pub hops: u32,
    /// The node cap actually applied, after the server's clamp. Reported even when it did not
    /// bite, because "the cap was 250 and 40 nodes came back" is a different statement from "40
    /// nodes is all there is", and the analyst deciding whether to raise it needs both.
    pub node_cap: u32,
    /// This window of the neighbourhood: hop ascending, then id ascending, so a cut keeps the
    /// nearest nodes and keeps the same ones on every call.
    pub nodes: Vec<NeighbourhoodNodeView>,
    /// How many nodes the walk actually reached, before the cap. `> nodes.len()` exactly when the
    /// cap bit.
    pub total_discovered: u64,
    pub truncation: ReportedTruncation,
}

impl NeighbourhoodView {
    /// Apply `node_cap` to `discovered` and report what that did — the only way to obtain a
    /// [`NeighbourhoodView`].
    ///
    /// `reason` is `"node_cap"`, joining phase 44's `hop_cap`/`result_cap`/`wall_clock`/`sample`
    /// vocabulary rather than starting a second one.
    pub fn cut(
        seeds: Vec<EntityRef>,
        hops: u32,
        node_cap: u32,
        mut discovered: Vec<NeighbourhoodNodeView>,
    ) -> Self {
        let total_discovered = discovered.len() as u64;
        let cap = node_cap as usize;
        let cut = discovered.len().saturating_sub(cap);
        discovered.truncate(cap);
        let truncation = if cut == 0 {
            Truncation {
                truncated: false,
                reason: None,
                detail: String::new(),
            }
        } else {
            Truncation {
                truncated: true,
                reason: Some("node_cap".to_string()),
                detail: format!(
                    "stopped at node cap {node_cap}: {cut} of {total_discovered} nodes within \
                     {hops} hops are not shown"
                ),
            }
        };
        Self {
            seeds,
            hops,
            node_cap,
            nodes: discovered,
            total_discovered,
            truncation: ReportedTruncation(truncation),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::temporal::EffectiveRange;

    fn seed() -> EntityRef {
        EntityRef {
            id: "GB-COH:X06999001".to_string(),
            name: "Example Holdings Ltd".to_string(),
            kind: "Entity".to_string(),
        }
    }

    fn nodes(n: u32) -> Vec<NeighbourhoodNodeView> {
        (0..n)
            .map(|i| NeighbourhoodNodeView {
                entity: EntityRef {
                    id: format!("contract:X9999-c{i:04}"),
                    name: format!("Contract {i}"),
                    kind: "Contract".to_string(),
                },
                hop: 1,
                edges: vec![NeighbourEdgeView {
                    edge_type: "AwardedTo".to_string(),
                    temporal: Temporal::unfiltered(EffectiveRange::Unrecorded {
                        basis: "awardedto.meta.effective_from / awardedto.meta.effective_to"
                            .to_string(),
                    }),
                }],
                reached_from: seed().id,
            })
            .collect()
    }

    /// A cut is reported, with the figures the status line needs.
    #[test]
    fn a_cut_neighbourhood_reports_what_was_cut() {
        let view = NeighbourhoodView::cut(vec![seed()], 1, 10, nodes(25));
        assert_eq!(view.nodes.len(), 10);
        assert_eq!(view.total_discovered, 25);
        assert!(view.truncation.truncated);
        assert_eq!(view.truncation.reason.as_deref(), Some("node_cap"));
        assert!(
            view.truncation.detail.contains("15"),
            "the detail must name what was cut: {}",
            view.truncation.detail
        );
    }

    /// An uncut neighbourhood says so, and still reports the cap it was measured against.
    #[test]
    fn an_uncut_neighbourhood_reports_no_truncation_and_still_names_its_cap() {
        let view = NeighbourhoodView::cut(vec![seed()], 2, 250, nodes(4));
        assert_eq!(view.nodes.len(), 4);
        assert_eq!(view.total_discovered, 4);
        assert!(!view.truncation.truncated);
        assert_eq!(view.truncation.reason, None);
        assert_eq!(view.node_cap, 250);
    }

    /// **The invariant the whole module exists for**: `nodes.len() < total_discovered` implies
    /// `truncated`, for every cap and every population, with no way to construct a counterexample.
    ///
    /// PROVE THIS CAN FAIL: replace `cut == 0` with `true` in [`NeighbourhoodView::cut`] — i.e.
    /// cut the list and report `truncated: false`, the exact defect CNV-01 names — and this test
    /// fails on the first capped case. That mutation was run before this test was committed.
    ///
    /// Note what the test does NOT have to do: it never constructs a `NeighbourhoodView` literal
    /// with a mismatched truncation, **because it cannot** — `ReportedTruncation`'s field is
    /// private, so the only producer in existence is the function under test.
    #[test]
    fn a_shortened_list_always_reports_itself_shortened() {
        let mut capped_cases = 0;
        for population in [0u32, 1, 5, 10, 11, 100] {
            for cap in [1u32, 5, 10, 250] {
                let view = NeighbourhoodView::cut(vec![seed()], 1, cap, nodes(population));
                let shortened = (view.nodes.len() as u64) < view.total_discovered;
                capped_cases += usize::from(shortened);
                assert_eq!(
                    shortened,
                    view.truncation.truncated,
                    "population {population} under cap {cap}: showed {} of {}, reported \
                     truncated={}",
                    view.nodes.len(),
                    view.total_discovered,
                    view.truncation.truncated
                );
            }
        }
        assert!(
            capped_cases >= 6,
            "NON-VACUITY: only {capped_cases} of the cases actually hit the cap, so this sweep \
             would pass against an implementation that never truncates at all"
        );
    }

    /// Every edge states a type **and** a range, and an edge nobody filtered does not claim to
    /// have been in force.
    ///
    /// PROVE THIS CAN FAIL: replace `Temporal::unfiltered(...)` in the producer with a `Temporal`
    /// carrying `AsAtStatus::InForce` and the status assertion fails; drop the `temporal` field
    /// and the crate stops compiling, which is the stronger half.
    #[test]
    fn every_neighbourhood_edge_carries_a_type_and_a_range_it_has_not_been_filtered_on() {
        let view = NeighbourhoodView::cut(vec![seed()], 1, 250, nodes(3));
        assert_eq!(view.nodes.len(), 3, "Non-vacuity: the fixture has edges");
        for node in &view.nodes {
            assert!(!node.edges.is_empty(), "an edge is how the walk got here");
            for edge in &node.edges {
                assert!(!edge.edge_type.is_empty());
                assert_eq!(
                    edge.temporal.status,
                    crate::temporal::AsAtStatus::NotFiltered,
                    "the canvas runs no as-at filter, so no edge may claim to be in force"
                );
                // An unrecorded range names the fields consulted — the difference between "this
                // dataset records no dates" and "this edge lost its dates".
                if let EffectiveRange::Unrecorded { basis } = &edge.temporal.range {
                    assert!(
                        !basis.is_empty(),
                        "an unrecorded range must say what it read"
                    );
                }
            }
            assert_eq!(node.edge_types(), vec!["AwardedTo".to_string()]);
        }

        let json = serde_json::to_value(&view).expect("serialize");
        assert_eq!(json["nodes"][0]["edges"][0]["edge_type"], "AwardedTo");
        // `Temporal.range` is an internally-tagged `EffectiveRange`, so its own discriminant sits
        // one level further in — `temporal.range.range`. Asserted at the path it actually occupies
        // rather than at the one it reads like.
        assert_eq!(
            json["nodes"][0]["edges"][0]["temporal"]["range"]["range"],
            "unrecorded"
        );
        assert_eq!(
            json["nodes"][0]["edges"][0]["temporal"]["status"]["as_at"],
            "not_filtered"
        );
        let back: NeighbourhoodView = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, view);
    }

    /// The wire shape is phase 44's `Truncation` object, unwrapped — D-12's "reuses both rather
    /// than inventing a second truncation convention", checked rather than asserted in prose.
    #[test]
    fn the_wire_shape_is_phase_forty_fours_truncation_block() {
        let view = NeighbourhoodView::cut(vec![seed()], 1, 2, nodes(5));
        let json = serde_json::to_value(&view).expect("serialize");
        assert_eq!(json["truncation"]["truncated"], true);
        assert_eq!(json["truncation"]["reason"], "node_cap");
        assert!(json["truncation"]["detail"].is_string());

        let block: Truncation =
            serde_json::from_value(json["truncation"].clone()).expect("a plain Truncation");
        assert!(block.truncated);

        let back: NeighbourhoodView = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, view);
    }
}
