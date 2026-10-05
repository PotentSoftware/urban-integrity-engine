//! PERF-03 compliance test — P99 latency CI gate.
//!
//! This test MUST pass in CI. If P99 exceeds 10ms, PERF-03 is violated.
//! Run with: cargo test -p urban-integrity-core test_perf03_p99_under_10ms

use std::time::{Duration, Instant};
use tokio::task::JoinSet;
use urban_integrity_core::graph::snapshot::{bake_csr, SnapshotHandle};
use urban_integrity_core::graph::storage::GraphStorage;
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

/// PERF-03: 10k concurrent CsrSnapshot reads must achieve P99 < 10ms.
///
/// Each task performs:
///   1. handle.load()   — atomic ArcSwap guard acquisition
///   2. graph.neighbors_slice(seed) — 1-hop CSR read
///   3. guard dropped   — ArcSwap ref-count decremented
///
/// The 100k-node graph is L3-resident (~10MB) — this validates the v1.0 operating point.
#[tokio::test(flavor = "multi_thread")]
async fn test_perf03_p99_under_10ms() {
    const NODE_COUNT: usize = 100_000;
    const TASK_COUNT: usize = 10_000;
    const P99_LIMIT: Duration = Duration::from_millis(10);

    // Build graph
    let storage = GraphStorage::new();
    for i in 0..NODE_COUNT {
        storage.insert_node(
            EntityId(format!("node-{}", i)),
            NodeWeight::Entity(EntityData {
                name: format!("Entity {}", i),
                company_number: None,
                status: EntityStatus::Active,
                incorporation_date: None,
            }),
        );
    }
    let snapshot = bake_csr(&storage, 1);
    let handle = SnapshotHandle::empty();
    handle.store(snapshot);
    // handle is Arc<SnapshotHandle> — .store() is called through Arc auto-deref

    // Spawn 10k concurrent reader tasks, each measuring its own latency
    let mut set = JoinSet::new();
    for i in 0..TASK_COUNT {
        let h = handle.clone();
        set.spawn(async move {
            let start = Instant::now();
            let guard = h.load();
            let nc = guard.graph.node_count();
            if nc > 0 {
                let seed = (i % nc) as u32;
                let _neighbors = guard.graph.neighbors_slice(seed);
            }
            drop(guard);
            start.elapsed()
        });
    }

    // Collect all task latencies
    let mut latencies: Vec<Duration> = Vec::with_capacity(TASK_COUNT);
    while let Some(result) = set.join_next().await {
        latencies.push(result.expect("reader task panicked"));
    }
    assert_eq!(latencies.len(), TASK_COUNT, "All tasks must complete");

    // Compute P99
    latencies.sort_unstable();
    let p99_idx = (latencies.len() * 99) / 100;
    let p99 = latencies[p99_idx];
    let p50 = latencies[latencies.len() / 2];
    let max = *latencies.last().unwrap();

    eprintln!(
        "PERF-03 latency stats ({} tasks, {} nodes): P50={:?}  P99={:?}  MAX={:?}",
        TASK_COUNT, NODE_COUNT, p50, p99, max
    );

    assert!(
        p99 < P99_LIMIT,
        "PERF-03 FAILED: P99 latency = {:?} exceeds {}ms limit \
         (P50={:?}, MAX={:?})",
        p99,
        P99_LIMIT.as_millis(),
        p50,
        max
    );
}
