//! The worked examples the algorithm help pages render (spec §5.2 block 3). Wasm-safe literals;
//! `crates/workbench/tests/planted_examples_match_manifest.rs` proves each one equals the planted
//! manifest, so the help cannot drift from the fixture it describes.
//!
//! Every example is written against the planted fixture, which an operator can reproduce for
//! themselves with `scripts/toy-analysis.sh` — that is the whole point of stating exact rows
//! rather than "you will see some communities": the help makes a claim the reader can falsify in
//! their own browser in under a minute.
//!
//! The prose in `setup`/`expected_rows` is human copy and cannot be machine-checked. The `check`
//! field carries the same claim in a form the drift test CAN compare against the manifest, so a
//! fixture change fails the build rather than quietly making the help lie. Keep the two halves
//! saying the same thing.

pub struct PlantedExample {
    pub algorithm: &'static str,
    /// Rail parameters the analyst sets, in analyst words ("From: Planted Alpha Ltd").
    pub setup: &'static [&'static str],
    /// The exact rows to expect, in the order the results list shows them.
    pub expected_rows: &'static [&'static str],
    /// Machine-checkable facts the drift test compares to the manifest.
    pub check: ExampleCheck,
}

pub enum ExampleCheck {
    ShortestPath {
        from: &'static str,
        to: &'static str,
        node_ids: &'static [&'static str],
    },
    AllSimplePaths {
        from: &'static str,
        to: &'static str,
        hop_cap: u32,
        count: usize,
        with_located_at: bool,
    },
    CommunityCount {
        count: usize,
        hidden: usize,
    },
    KCoreMembers {
        k: u32,
        count: usize,
    },
    BetweennessTop1 {
        id: &'static str,
    },
    WardSubgraphCount {
        count: usize,
    },
}

pub const PLANTED_EXAMPLES: &[PlantedExample] = &[
    PlantedExample {
        algorithm: "shortest_paths",
        setup: &[
            "From: Planted Alpha Ltd",
            "To: Planted Beta Ltd",
            "k: 1",
            "Edge types: defaults",
        ],
        expected_rows: &[
            "Planted Alpha Ltd → Planted Officer One → Planted Beta Ltd — one officer directs both companies",
        ],
        check: ExampleCheck::ShortestPath {
            from: "GB-COH:PL000001",
            to: "GB-COH:PL000002",
            node_ids: &["GB-COH:PL000001", "director:planted-o1", "GB-COH:PL000002"],
        },
    },
    PlantedExample {
        algorithm: "all_simple_paths",
        setup: &[
            "From: Planted Alpha Ltd",
            "To: Planted Beta Ltd",
            "Hop cap: 4",
            "Edge types: defaults plus LocatedAt",
        ],
        expected_rows: &[
            "2 paths: one through Planted Officer One, one through 1 Planted Street",
            "Without LocatedAt the same run returns 1 path — the shared address is the second route",
        ],
        check: ExampleCheck::AllSimplePaths {
            from: "GB-COH:PL000001",
            to: "GB-COH:PL000002",
            hop_cap: 4,
            count: 2,
            with_located_at: true,
        },
    },
    PlantedExample {
        algorithm: "community",
        setup: &[
            "Seed: 42",
            "Minimum community size: 5",
            "Edge types: defaults",
        ],
        expected_rows: &[
            "3 communities — the two dense blocks, plus the phoenix chain and its beneficial owner",
            "15 nodes hidden below minimum size",
        ],
        check: ExampleCheck::CommunityCount {
            count: 3,
            hidden: 15,
        },
    },
    PlantedExample {
        algorithm: "k_core",
        setup: &["k: 5", "Edge types: defaults"],
        expected_rows: &[
            "20 nodes, 50 edges: the ten officers and ten companies of the two blocks",
            "Everything of degree below 5 peels away, including the bridge officer",
        ],
        check: ExampleCheck::KCoreMembers { k: 5, count: 20 },
    },
    PlantedExample {
        algorithm: "betweenness",
        setup: &["Top N: 50", "Seed: 42", "Edge types: defaults"],
        expected_rows: &[
            "Top row: Planted Bridge Officer — every route between the two blocks passes through it",
        ],
        check: ExampleCheck::BetweennessTop1 {
            id: "director:planted-ob",
        },
    },
    PlantedExample {
        algorithm: "ward_subgraph",
        setup: &["Ward: Planted Ward", "Hops: 2 (the default)"],
        expected_rows: &[
            "5 nodes: the ward and its four contracts",
            "The walk follows Represents only, whatever the edge-type checklist says",
        ],
        check: ExampleCheck::WardSubgraphCount { count: 5 },
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// One example per algorithm the rail offers, and no duplicates — a help page with no worked
    /// example is the gap this module exists to close.
    #[test]
    fn there_is_exactly_one_example_per_algorithm() {
        let mut names: Vec<&str> = PLANTED_EXAMPLES.iter().map(|e| e.algorithm).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec![
                "all_simple_paths",
                "betweenness",
                "community",
                "k_core",
                "shortest_paths",
                "ward_subgraph",
            ]
        );
    }

    /// Every example must actually say something in both halves. An empty `expected_rows` would
    /// render a worked example that works nothing through.
    #[test]
    fn every_example_states_setup_and_expected_rows() {
        for example in PLANTED_EXAMPLES {
            assert!(
                !example.setup.is_empty(),
                "{} has no setup lines",
                example.algorithm
            );
            assert!(
                !example.expected_rows.is_empty(),
                "{} has no expected rows",
                example.algorithm
            );
            for line in example.setup.iter().chain(example.expected_rows.iter()) {
                assert!(
                    !line.trim().is_empty(),
                    "{} has a blank line in its copy",
                    example.algorithm
                );
            }
        }
    }
}
