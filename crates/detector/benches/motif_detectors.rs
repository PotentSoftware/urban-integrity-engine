use criterion::{criterion_group, criterion_main, Criterion};
use std::collections::HashMap;
use urban_integrity_core::graph::snapshot::bake_csr;
use urban_integrity_core::graph::storage::GraphStorage;
use urban_integrity_detector::motif::cluster::ClusterDetector;
use urban_integrity_detector::motif::nepotism::NepotismDetector;
use urban_integrity_detector::motif::phoenix::{PhoenixConfig, PhoenixDetector};
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

fn setup_bench_graph(node_count: usize) -> GraphStorage {
    let storage = GraphStorage::new();
    for i in 0..node_count {
        let id = EntityId(format!("node-{}", i));
        let weight = NodeWeight::Entity(EntityData {
            name: format!("Entity {}", i),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        });
        storage.insert_node(id, weight);
    }
    storage
}

fn bench_detectors(c: &mut Criterion) {
    let storage = setup_bench_graph(10_000);
    let snapshot = bake_csr(&storage, 10_000);
    let mut id_map = HashMap::new();
    for (id, idx, _, _) in storage.snapshot_nodes() {
        id_map.insert(idx.index() as u32, id.0.clone());
    }

    let phoenix = PhoenixDetector::new(PhoenixConfig {
        detection_window_days: 30,
        min_value_threshold: 1000.0,
    });
    let cluster = ClusterDetector::new(Default::default());
    let nepotism = NepotismDetector::new(Default::default());

    c.bench_function("phoenix_detector", |b| {
        b.iter(|| phoenix.check_tier_b(&snapshot, &[], &id_map))
    });

    c.bench_function("cluster_detector", |b| {
        b.iter(|| cluster.check(&snapshot, &id_map))
    });

    c.bench_function("nepotism_detector", |b| {
        b.iter(|| nepotism.check(&snapshot, &id_map))
    });
}

criterion_group!(benches, bench_detectors);
criterion_main!(benches);
