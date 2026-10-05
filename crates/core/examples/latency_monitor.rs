use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use urban_integrity_core::graph::coordinator::{CoordinatorConfig, GraphCoordinator};
use urban_integrity_core::graph::types::{GraphEvent, GraphMutation};
use urban_integrity_schema::types::id::EntityId;
use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight};

#[tokio::main]
async fn main() {
    println!("Starting UrbanIntegrity Latency Monitor...");

    let (wal_tx, wal_rx) = crossbeam_channel::bounded::<GraphEvent>(10_000);
    let config = CoordinatorConfig {
        snapshot_every_n_events: 100,
        snapshot_interval_secs: 5,
    };

    let (coordinator, handle) = GraphCoordinator::new(config, wal_tx, None);
    tokio::spawn(coordinator.run());

    // Monitor task
    let monitor_handle = tokio::spawn(async move {
        let mut latencies = Vec::new();
        let start_time = Instant::now();

        while let Ok(event) = {
            let rx = wal_rx.clone();
            tokio::task::spawn_blocking(move || rx.recv())
                .await
                .unwrap()
        } {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            let latency = now.saturating_sub(event.timestamp);
            latencies.push(latency);

            if latencies.len() % 100 == 0 {
                let mut sorted = latencies.clone();
                sorted.sort();
                let p50 = sorted[sorted.len() / 2];
                let p99 = sorted[(sorted.len() as f64 * 0.99) as usize];
                println!(
                    "Count: {} | P50: {}ms | P99: {}ms | Elapsed: {:?}",
                    latencies.len(),
                    p50,
                    p99,
                    start_time.elapsed()
                );
            }

            if start_time.elapsed() > Duration::from_secs(10) {
                break;
            }
        }
        latencies
    });

    // Ingestor task
    for i in 0..1000 {
        let id = EntityId(format!("node-{}", i));
        let weight = NodeWeight::Entity(EntityData {
            name: format!("Entity {}", i),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        });
        let _ = handle
            .mutation_tx
            .send(GraphMutation::AddNode { id, weight })
            .await;
        if i % 100 == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    let latencies = monitor_handle.await.unwrap();
    println!("Final Report:");
    println!("Total Events: {}", latencies.len());
}
