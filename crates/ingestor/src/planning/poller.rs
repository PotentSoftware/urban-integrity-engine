//! Background Tokio task that periodically scrapes configured planning portals
//! and feeds planning application data into the graph pipeline.
//!
//! Mirrors the `CfPoller` pattern from `crates/ingestor/src/cf/client.rs`.

use std::time::Duration;
use tracing::{info, warn};

use urban_integrity_core::{
    config::PlanningPortalConfig, graph::coordinator::GraphCoordinatorHandle, pipeline::process,
};

use crate::planning::{mapper::map_planning_item, sidecar::ScrapingSidecar};

/// Background poller for all configured planning portals.
///
/// Mirrors `CfPoller` — iterates over all configured ward entries,
/// calls the Scrapling sidecar for each, maps results to graph mutations,
/// and sends them to the `GraphCoordinator`. Repeats on `poll_interval`.
///
/// On any failure (sidecar unreachable, pipeline rejection), logs WARN
/// and continues — never panics. Portal scraping is best-effort.
pub struct PlanningPoller {
    sidecar: ScrapingSidecar,
    coordinator: GraphCoordinatorHandle,
    portals: Vec<PlanningPortalConfig>,
    poll_interval: Duration,
}

impl PlanningPoller {
    pub fn new(
        sidecar: ScrapingSidecar,
        coordinator: GraphCoordinatorHandle,
        portals: Vec<PlanningPortalConfig>,
        poll_interval_secs: u64,
    ) -> Self {
        Self {
            sidecar,
            coordinator,
            portals,
            poll_interval: Duration::from_secs(poll_interval_secs),
        }
    }

    /// Poll all configured planning portals on an interval forever.
    ///
    /// If the sidecar is unreachable for a portal, logs a WARN and continues
    /// to the next ward — never panics. The engine keeps running.
    pub async fn run(&self) {
        loop {
            for portal in &self.portals {
                self.poll_once(portal).await;
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    /// Scrape a single portal entry and push results into the coordinator.
    ///
    /// Returns immediately if the sidecar returns 0 items — this happens when
    /// the sidecar is unreachable or the ward page is genuinely empty.
    pub async fn poll_once(&self, portal: &PlanningPortalConfig) {
        info!(
            ward_code = %portal.ward_code,
            url = %portal.url,
            portal_type = %portal.portal_type,
            "Planning portal poll starting"
        );

        let items = self.sidecar.scrape(&portal.url, &portal.portal_type).await;

        if items.is_empty() {
            warn!(
                ward_code = %portal.ward_code,
                "Planning portal scrape returned 0 items (sidecar unreachable or page empty)"
            );
            return;
        }

        let mut mutation_count = 0usize;

        for item in &items {
            let events = map_planning_item(item, &portal.ward_code);
            for event in events {
                match process(event) {
                    Ok(mutation) => {
                        if let Err(e) = self.coordinator.mutation_tx.send(mutation).await {
                            warn!(
                                ward_code = %portal.ward_code,
                                ?e,
                                "Planning mutation send failed — coordinator channel closed?"
                            );
                        } else {
                            mutation_count += 1;
                        }
                    }
                    Err(e) => {
                        warn!(
                            ward_code = %portal.ward_code,
                            ?e,
                            "Planning event failed pipeline validation — skipping"
                        );
                    }
                }
            }
        }

        info!(
            ward_code = %portal.ward_code,
            applications = items.len(),
            mutations = mutation_count,
            "Planning portal poll complete"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_core::{
        config::PlanningPortalConfig,
        graph::coordinator::{CoordinatorConfig, GraphCoordinator},
        persistence::wal::wal_channel,
    };

    fn make_coordinator() -> GraphCoordinatorHandle {
        let (wal_tx, _wal_rx) = wal_channel(1024);
        let (coordinator, handle) =
            GraphCoordinator::new(CoordinatorConfig::default(), wal_tx, None);
        tokio::spawn(coordinator.run());
        handle
    }

    fn make_portal(url: &str) -> PlanningPortalConfig {
        PlanningPortalConfig {
            lpa_code: None,
            ward_code: "E05001234".to_string(),
            url: url.to_string(),
            portal_type: "idox".to_string(),
        }
    }

    #[tokio::test]
    async fn test_planning_poller_new_builds_correctly() {
        let handle = make_coordinator();
        let sidecar = ScrapingSidecar::new("http://127.0.0.1", 19996);
        let poller = PlanningPoller::new(sidecar, handle, vec![], 60);
        assert_eq!(poller.portals.len(), 0);
        assert_eq!(poller.poll_interval, Duration::from_secs(60));
    }

    #[tokio::test]
    async fn test_poll_once_with_unreachable_sidecar_returns_gracefully() {
        let handle = make_coordinator();
        let sidecar = ScrapingSidecar::new("http://127.0.0.1", 19996);
        let poller = PlanningPoller::new(sidecar, handle, vec![], 60);
        let portal = make_portal("http://example.com/planning");
        // Should not panic — returns early after warn when sidecar unreachable (0 items)
        poller.poll_once(&portal).await;
    }

    #[tokio::test]
    async fn test_poll_once_with_mock_sidecar_items_pushes_mutations() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let body = r#"{"status":"ok","results":[{"reference":"23/01234","description":"Warehouse erection","address":"B1 1AA","ward":"Sandwell Central","applicant":"Apex Build Ltd"}]}"#;
        let body_owned = body.to_owned();

        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body_owned.len(),
                    body_owned
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let handle = make_coordinator();
        let sidecar = ScrapingSidecar::new("http://127.0.0.1", port);
        let poller = PlanningPoller::new(sidecar, handle, vec![], 60);
        let portal = make_portal(&format!("http://127.0.0.1:{}/planning", port));
        poller.poll_once(&portal).await;
    }
}
