//! Proof that every worked example the algorithm help pages render is a fact about the planted
//! fixture, not a plausible-sounding sentence someone typed once.
//!
//! The help literals live in `crates/workbench-dto` because the help pages compile to wasm; the
//! manifest lives in `crates/test-support`, which is native-only. This test is the one place both
//! are in scope, so it is the only thing standing between a fixture change and help that quietly
//! describes a graph the operator no longer has. Host-side (`#[test]`, no wasm, no DB).

use urban_integrity_test_support::planted::{
    planted_graph, DEFAULT_EDGES, DEFAULT_PLUS_LOCATED_AT,
};
use urban_integrity_workbench_dto::planted_examples::{ExampleCheck, PLANTED_EXAMPLES};

#[test]
fn every_worked_example_equals_the_manifest() {
    let manifest = planted_graph().manifest;

    // Non-vacuity: an empty example list would pass every assertion below without checking a
    // thing, and so would a list whose entries all fell through an unmatched arm.
    assert_eq!(
        PLANTED_EXAMPLES.len(),
        6,
        "one worked example per rail algorithm"
    );
    let mut checked = 0usize;

    for example in PLANTED_EXAMPLES {
        match &example.check {
            ExampleCheck::ShortestPath { from, to, node_ids } => {
                let path = manifest
                    .shortest_paths
                    .iter()
                    .find(|p| p.from == *from && p.to == *to)
                    .unwrap_or_else(|| panic!("{}: manifest has no such path", example.algorithm));
                assert_eq!(path.node_ids, *node_ids, "{}", example.algorithm);
            }
            ExampleCheck::AllSimplePaths {
                from,
                to,
                hop_cap,
                count,
                with_located_at,
            } => {
                let edges = if *with_located_at {
                    DEFAULT_PLUS_LOCATED_AT
                } else {
                    DEFAULT_EDGES
                };
                let set = manifest
                    .all_simple_paths
                    .iter()
                    .find(|p| {
                        p.from == *from
                            && p.to == *to
                            && p.hop_cap == *hop_cap
                            && p.edge_types_included == edges
                    })
                    .unwrap_or_else(|| {
                        panic!("{}: manifest has no such path set", example.algorithm)
                    });
                assert_eq!(set.count, *count, "{}", example.algorithm);
            }
            ExampleCheck::CommunityCount { count, hidden } => {
                let communities = manifest
                    .communities
                    .iter()
                    .find(|c| c.edge_types_included == DEFAULT_EDGES)
                    .unwrap_or_else(|| {
                        panic!(
                            "{}: manifest has no default-edge community set",
                            example.algorithm
                        )
                    });
                assert_eq!(communities.sets.len(), *count, "{}", example.algorithm);
                assert_eq!(communities.hidden_count, *hidden, "{}", example.algorithm);
            }
            ExampleCheck::KCoreMembers { k, count } => {
                let core = manifest
                    .k_cores
                    .iter()
                    .find(|c| c.k == *k)
                    .unwrap_or_else(|| panic!("{}: manifest has no k={k} core", example.algorithm));
                assert_eq!(core.members.len(), *count, "{}", example.algorithm);
            }
            ExampleCheck::BetweennessTop1 { id } => {
                assert_eq!(manifest.betweenness[0].top_1, *id, "{}", example.algorithm);
            }
            ExampleCheck::WardSubgraphCount { count } => {
                assert_eq!(
                    manifest.ward_subgraphs[0].members.len(),
                    *count,
                    "{}",
                    example.algorithm
                );
            }
        }
        checked += 1;
    }

    assert_eq!(
        checked,
        PLANTED_EXAMPLES.len(),
        "every example must have been compared, not skipped"
    );
}

/// The prose half and the machine-checked half must agree. `expected_rows` is what the operator
/// reads; `check` is what this test verifies — if the numbers in the copy drifted from the
/// numbers in the check, the help would be wrong in exactly the way the check cannot see.
#[test]
fn the_numbers_in_the_copy_match_the_numbers_in_the_check() {
    for example in PLANTED_EXAMPLES {
        let copy = example.expected_rows.join(" ");
        match &example.check {
            ExampleCheck::CommunityCount { count, hidden } => {
                assert!(
                    copy.contains(&count.to_string()) && copy.contains(&hidden.to_string()),
                    "{}: copy {copy:?} must state both {count} and {hidden}",
                    example.algorithm
                );
            }
            ExampleCheck::KCoreMembers { count, .. } => {
                assert!(
                    copy.contains(&count.to_string()),
                    "{}: copy {copy:?} must state {count}",
                    example.algorithm
                );
            }
            ExampleCheck::AllSimplePaths { count, .. } => {
                assert!(
                    copy.contains(&count.to_string()),
                    "{}: copy {copy:?} must state {count}",
                    example.algorithm
                );
            }
            ExampleCheck::WardSubgraphCount { count } => {
                assert!(
                    copy.contains(&count.to_string()),
                    "{}: copy {copy:?} must state {count}",
                    example.algorithm
                );
            }
            // The path examples name entities rather than counts; their ids are compared against
            // the manifest above, and the display names come from the same fixture.
            ExampleCheck::ShortestPath { .. } | ExampleCheck::BetweennessTop1 { .. } => {}
        }
    }
}
