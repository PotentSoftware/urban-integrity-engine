//! Regenerates `crates/workbench-web/src/testdata/population-real-arrow.bin` — the real Arrow IPC
//! payload `graph_page.rs`'s `real_arrow_bytes_parse_into_the_full_population` parses.
//!
//! That fixture is deliberately *real encoder output*, not a hand-written frame: every
//! hand-written fixture in this workspace set `Message.bodyLength` explicitly, so none reached
//! the absent-field path arrow-rs actually takes on its schema message, and the parser returned
//! an empty `Vec` for every real stream while the suite stayed green (see that test's own
//! comment). It was originally captured by hand from a running workbench, which left no way to
//! reproduce it when the planted fixture changed.
//!
//! This test is the reproduction path. It is `#[ignore]`d because it writes into another crate's
//! source tree; run it deliberately after any change to the planted fixture's topology, node
//! ids, or display names:
//!
//! ```sh
//! cargo test -p urban-integrity-workbench --test regenerate_population_arrow -- --ignored
//! ```
//!
//! Then re-run `cargo test -p urban-integrity-workbench-web` — the assertions there pin the
//! counts and the ward seed id, so a stale fixture fails loudly rather than silently.

use std::path::PathBuf;

use urban_integrity_test_support::planted::{planted_graph, WARD, WARD_CODE};
use urban_integrity_workbench::graph::arrow_export::build_arrow_ipc;
use urban_integrity_workbench::graph::fixture::workbench_from_graph;

/// `<repo>/crates/workbench-web/src/testdata/population-real-arrow.bin`, resolved from this
/// crate's manifest dir and asserted to sit under an existing directory — a crate move breaks
/// this loudly rather than writing the fixture somewhere nobody reads it.
fn fixture_path() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("workbench-web")
        .join("src")
        .join("testdata")
        .join("population-real-arrow.bin");
    let dir = path.parent().expect("fixture path has a parent");
    assert!(
        dir.is_dir(),
        "{dir:?} does not exist — crates/workbench-web must have moved; fix this path arithmetic"
    );
    path
}

#[tokio::test]
#[ignore = "writes crates/workbench-web/src/testdata/population-real-arrow.bin; run deliberately"]
async fn regenerate_population_arrow_fixture() {
    let g = planted_graph();
    let edges: Vec<_> = g
        .edges
        .iter()
        .map(|e| (e.src.clone(), e.dst.clone(), e.weight.clone()))
        .collect();
    let (handle, state) = workbench_from_graph(&g.nodes, &edges, &[WARD_CODE.to_string()]).await;

    // `workbench_from_graph` ties population and scope together by construction, so
    // `state.population_scope` is already the ward-reachable set the handler would resolve.
    let snapshot = handle.snapshot.load();
    let scope = &state.population_scope;
    let bytes =
        build_arrow_ipc(&handle.storage, &snapshot, scope).expect("arrow export must succeed");

    // Guard rails on what is about to be written: the manifest's reachable count, and the ward
    // seed the consuming test looks for by id.
    assert_eq!(
        scope.node_ids.len(),
        g.manifest.reachable_nodes,
        "the scoped population must be the manifest's reachable set"
    );
    assert!(
        g.nodes.iter().any(|(id, _)| id.0 == WARD),
        "the planted graph must contain the ward seed {WARD}"
    );

    let path = fixture_path();
    std::fs::write(&path, &bytes).expect("fixture write must succeed");
    eprintln!("wrote {} bytes to {}", bytes.len(), path.display());
}
