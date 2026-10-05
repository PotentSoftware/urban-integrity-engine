//! Concurrent reader benchmark — validates PERF-03.
//!
//! Spawns 10,000 Tokio async tasks that each perform a 1-hop CSR neighbor walk
//! from a random seed node, while concurrently holding an ArcSwap guard.
//! Run with: cargo bench --bench concurrent_readers

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::sync::Arc;
use std::time::Instant;
use tokio::runtime::Runtime;
use tokio::task::JoinSet;
use urban_integrity_core::graph::snapshot::{bake_csr, SnapshotHandle};
use urban_integrity_core::graph::storage::GraphStorage;
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

/// Seed a GraphStorage with `node_count` Entity nodes and return a SnapshotHandle.
fn seed_snapshot(node_count: usize) -> Arc<SnapshotHandle> {
    let storage = GraphStorage::new();
    for i in 0..node_count {
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
    handle
}

/// Benchmark: 10k concurrent 1-hop CSR reads.
/// Parameterised over two graph sizes:
///   - 100k nodes — L3-resident (~10MB); validates v1.0 operating point (run on every PR)
///   - 1M nodes   — memory-bound (~100MB); validates scaling toward v2 (run nightly)
fn bench_concurrent_readers(c: &mut Criterion) {
    let rt = Runtime::new().expect("Tokio runtime");
    const TASK_COUNT: usize = 10_000;

    let mut group = c.benchmark_group("concurrent_readers");
    // Extend measurement time for the 1M case to get stable numbers
    group.measurement_time(std::time::Duration::from_secs(30));

    for node_count in [100_000usize, 1_000_000usize] {
        let handle = seed_snapshot(node_count);

        group.bench_with_input(
            BenchmarkId::new("10k_readers_1hop", node_count),
            &node_count,
            |b, &_n| {
                let h = handle.clone();
                b.iter_custom(|iters| {
                    let mut total = std::time::Duration::ZERO;
                    for _ in 0..iters {
                        let h2 = h.clone();
                        let start = Instant::now();
                        rt.block_on(async move {
                            let mut set = JoinSet::new();
                            for i in 0..TASK_COUNT {
                                let hh = h2.clone();
                                set.spawn(async move {
                                    let guard = hh.load();
                                    let nc = guard.graph.node_count();
                                    if nc > 0 {
                                        let seed = (i % nc) as u32;
                                        // 1-hop CSR traversal — forces memory access into shards
                                        let _neighbors = guard.graph.neighbors_slice(seed);
                                    }
                                    // guard drops here — ArcSwap ref count decremented
                                });
                            }
                            while set.join_next().await.is_some() {}
                        });
                        total += start.elapsed();
                    }
                    total
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_concurrent_readers);
criterion_main!(benches);
