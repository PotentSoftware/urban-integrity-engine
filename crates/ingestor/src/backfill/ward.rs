use crate::backfill::session::DbSession;
use crate::backfill::{extract_id_str, BackfillStats};
use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Serialize)]
struct PostcodesRequest<'a> {
    postcodes: &'a [String],
}

#[derive(Deserialize, Debug)]
struct PostcodesResponse {
    result: Vec<PostcodeResult>,
}

#[derive(Deserialize, Debug)]
struct PostcodeResult {
    query: String,
    result: Option<PostcodeData>,
}

#[derive(Deserialize, Debug)]
struct PostcodeData {
    codes: PostcodeCodes,
}

#[derive(Deserialize, Debug)]
struct PostcodeCodes {
    admin_ward: String,
}

#[derive(Deserialize, Debug)]
struct TerminatedResponse {
    result: Option<TerminatedData>,
}

#[derive(Deserialize, Debug)]
struct TerminatedData {
    longitude: Option<f64>,
    latitude: Option<f64>,
}

#[derive(Deserialize, Debug)]
struct ReverseResponse {
    result: Option<Vec<PostcodeData>>,
}

/// Which postcodes.io route produced a ward — recorded in the `Represents` edge's `details` so
/// an analyst can tell a live-postcode attribution from a centroid-derived one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WardSource {
    /// `/postcodes` bulk lookup on a current postcode.
    Live,
    /// `/terminated_postcodes` centroid, reverse-geocoded to the ward it falls in today.
    TerminatedPostcode,
}

impl WardSource {
    /// Suffix appended to the `backfill:<role>_ward` provenance string.
    fn details_suffix(self) -> &'static str {
        match self {
            WardSource::Live => "",
            WardSource::TerminatedPostcode => ":terminated_postcode",
        }
    }
}

/// Pick the ward for a party from ALL its postcodes, not an arbitrary first one: a live
/// resolution from any postcode wins; failing that, a terminated-postcode resolution from any
/// postcode; `None` only when no postcode resolves either way. The previous `pcs.iter().next()`
/// choice let one unresolvable postcode on a multi-site entity hide a resolvable sibling.
fn first_resolved<'a>(
    postcodes: &[String],
    live: &'a HashMap<String, String>,
    terminated: &'a HashMap<String, String>,
) -> Option<(&'a str, WardSource)> {
    postcodes
        .iter()
        .find_map(|p| live.get(p).map(|w| (w.as_str(), WardSource::Live)))
        .or_else(|| {
            postcodes.iter().find_map(|p| {
                terminated
                    .get(p)
                    .map(|w| (w.as_str(), WardSource::TerminatedPostcode))
            })
        })
}

pub struct PostcodesIoClient {
    client: Client,
    base_url: String,
}

impl Default for PostcodesIoClient {
    fn default() -> Self {
        Self::new()
    }
}

impl PostcodesIoClient {
    pub fn new() -> Self {
        Self::new_with_base("https://api.postcodes.io".to_string())
    }

    pub fn new_with_base(base_url: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
            base_url,
        }
    }

    /// Resolve up to 100 postcodes → ONS ward codes in one batch.
    /// Returns a map of normalised_postcode (uppercase, no spaces) → ward_code.
    /// Invalid/null results are silently skipped.
    pub async fn resolve_batch(&self, postcodes: &[String]) -> Result<HashMap<String, String>> {
        if postcodes.is_empty() {
            return Ok(HashMap::new());
        }
        let url = format!("{}/postcodes", self.base_url);
        let body = PostcodesRequest { postcodes };
        let response = self.client.post(&url).json(&body).send().await?;
        if !response.status().is_success() {
            tracing::warn!(status = %response.status(), "postcodes.io error");
            return Ok(HashMap::new());
        }
        let resp: PostcodesResponse = response.json().await?;
        Ok(resp
            .result
            .into_iter()
            .filter_map(|r| {
                r.result.map(|data| {
                    let norm = r.query.to_uppercase().replace(' ', "");
                    (norm, data.codes.admin_ward)
                })
            })
            .collect())
    }
    /// Fallback for a postcode the bulk lookup returned `null` for. postcodes.io drops a
    /// terminated postcode from `/postcodes` but keeps its centroid under
    /// `/terminated_postcodes/{pc}`; reverse-geocoding that centroid (`/postcodes?lon&lat`)
    /// yields the ward the address sits in today. Measured 2026-09-06 on the Anglesey analysis:
    /// 24 of the 31 buyer postcodes the bulk route rejected were terminated postcodes with a
    /// reverse-geocodable centroid; the other 7 (a Gibraltar code, typos) 404 here too.
    ///
    /// Returns `Ok(None)` when the postcode is unknown to `/terminated_postcodes` (404), when the
    /// record carries no centroid, or when nothing reverse-geocodes. Transport failures are `Err`.
    pub async fn resolve_terminated(&self, postcode: &str) -> Result<Option<String>> {
        let url = format!("{}/terminated_postcodes/{}", self.base_url, postcode);
        let response = self.client.get(&url).send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            tracing::warn!(status = %response.status(), postcode, "postcodes.io terminated lookup error");
            return Ok(None);
        }
        let resp: TerminatedResponse = response.json().await?;
        let (Some(lon), Some(lat)) = resp
            .result
            .map(|d| (d.longitude, d.latitude))
            .unwrap_or((None, None))
        else {
            return Ok(None);
        };
        let url = format!("{}/postcodes?lon={lon}&lat={lat}&limit=1", self.base_url);
        let response = self.client.get(&url).send().await?;
        if !response.status().is_success() {
            tracing::warn!(status = %response.status(), postcode, "postcodes.io reverse geocode error");
            return Ok(None);
        }
        let resp: ReverseResponse = response.json().await?;
        Ok(resp
            .result
            .and_then(|v| v.into_iter().next())
            .map(|d| d.codes.admin_ward))
    }
}

pub struct WardBackfiller {
    session: DbSession,
    postcodes_client: PostcodesIoClient,
    pub dry_run: bool,
}

impl WardBackfiller {
    pub fn new(session: DbSession, dry_run: bool) -> Self {
        Self {
            session,
            postcodes_client: PostcodesIoClient::new(),
            dry_run,
        }
    }

    pub async fn run(&self) -> Result<BackfillStats> {
        use crate::backfill::{insert_edge_event, insert_node_event, update_node_event_by_record};
        use chrono::Utc;
        use urban_integrity_schema::types::{
            edge::{EdgeMeta, EdgeWeight, RepresentsData},
            node::{ContractData, NodeWeight, WardData},
        };

        let mut stats = BackfillStats::default();

        // ── 1. Load all Contract NodeAdded events ─────────────────────────────
        // Paginated + kind-filtered Rust-side. The former single-shot form combined the planner
        // bug with a hard `LIMIT`, so it under-read silently; see `scan_events_by_type`.
        let contract_rows = crate::backfill::scan_events_by_type(
            &self.session,
            "NodeAdded",
            "\"type\":\"Contract\"",
        )
        .await?;

        // Filter: CF contracts (entity_id "CF:"), tender_ref present, ward_code absent
        let cf_contracts: Vec<(String, String, ContractData)> = contract_rows
            .into_iter()
            .filter_map(|row| {
                // Record id, not event_id — the write addresses the row by primary key.
                let record_id = extract_id_str(&row["id"])?;
                let entity_id = extract_id_str(&row["payload"]["entity_id"])?;
                if !entity_id.starts_with("CF:") {
                    return None;
                }
                let wj = row["payload"]["weight_json"].as_str()?;
                let weight: NodeWeight = serde_json::from_str(wj).ok()?;
                match weight {
                    NodeWeight::Contract(c) if c.tender_ref.is_some() && c.ward_code.is_none() => {
                        Some((record_id, entity_id, c))
                    }
                    _ => None,
                }
            })
            .collect();

        stats.scanned = cf_contracts.len();
        tracing::info!(
            count = stats.scanned,
            "Ward backfill: CF contracts to enrich"
        );

        if cf_contracts.is_empty() {
            return Ok(stats);
        }

        // ── 2. Build entity→postcode map from LocatedAt edges ─────────────────
        let located_rows = crate::backfill::scan_events_by_type(
            &self.session,
            "EdgeAdded",
            "\"type\":\"LocatedAt\"",
        )
        .await?;

        let location_node_rows = crate::backfill::scan_events_by_type(
            &self.session,
            "NodeAdded",
            "\"type\":\"Location\"",
        )
        .await?;

        let mut loc_postcodes: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for row in &location_node_rows {
            let lid = match extract_id_str(&row["payload"]["entity_id"]) {
                Some(s) => s,
                None => continue,
            };
            let wj = match row["payload"]["weight_json"].as_str() {
                Some(s) => s,
                None => continue,
            };
            if let Ok(NodeWeight::Location(ld)) = serde_json::from_str::<NodeWeight>(wj) {
                if let Some(pc) = ld.postcode {
                    loc_postcodes.insert(lid, pc.to_uppercase().replace(' ', ""));
                }
            }
        }

        // entity_id → set of postcodes
        let mut entity_postcodes: std::collections::HashMap<
            String,
            std::collections::HashSet<String>,
        > = std::collections::HashMap::new();
        for row in &located_rows {
            let src = match extract_id_str(&row["payload"]["src_id"]) {
                Some(s) => s,
                None => continue,
            };
            let dst = match extract_id_str(&row["payload"]["dst_id"]) {
                Some(s) => s,
                None => continue,
            };
            if let Some(pc) = loc_postcodes.get(&dst) {
                entity_postcodes.entry(src).or_default().insert(pc.clone());
            }
        }

        // ── 3. Build contract→entity map from AwardedTo + IssuedBy edges ──────
        let awarded_rows = crate::backfill::scan_events_by_type(
            &self.session,
            "EdgeAdded",
            "\"type\":\"AwardedTo\"",
        )
        .await?;

        let issued_rows = crate::backfill::scan_events_by_type(
            &self.session,
            "EdgeAdded",
            "\"type\":\"IssuedBy\"",
        )
        .await?;

        // contract_id → Vec<(entity_id, role)>
        let mut contract_parties: std::collections::HashMap<String, Vec<(String, &str)>> =
            std::collections::HashMap::new();
        for row in &awarded_rows {
            let src = match extract_id_str(&row["payload"]["src_id"]) {
                Some(s) => s,
                None => continue,
            };
            let dst = match extract_id_str(&row["payload"]["dst_id"]) {
                Some(s) => s,
                None => continue,
            };
            if dst.starts_with("CF:") {
                contract_parties
                    .entry(dst)
                    .or_default()
                    .push((src, "supplier"));
            }
        }
        for row in &issued_rows {
            let src = match extract_id_str(&row["payload"]["src_id"]) {
                Some(s) => s,
                None => continue,
            };
            let dst = match extract_id_str(&row["payload"]["dst_id"]) {
                Some(s) => s,
                None => continue,
            };
            if dst.starts_with("CF:") {
                contract_parties
                    .entry(dst)
                    .or_default()
                    .push((src, "buyer"));
            }
        }

        // ── 4. Collect all postcodes needing resolution ───────────────────────
        struct ContractWork {
            record_id: String,
            contract_id: String,
            contract_data: ContractData,
            buyer_postcodes: Vec<String>,
            supplier_postcodes: Vec<String>,
        }
        let mut work: Vec<ContractWork> = Vec::new();
        let mut all_postcodes: std::collections::HashSet<String> = std::collections::HashSet::new();

        for (record_id, contract_id, contract_data) in &cf_contracts {
            let parties = contract_parties
                .get(contract_id)
                .cloned()
                .unwrap_or_default();
            // Every postcode of every party, sorted for a deterministic pick — `first_resolved`
            // (step 6) chooses among them once the lookups are in.
            let mut buyer_pcs: Vec<String> = Vec::new();
            let mut supplier_pcs: Vec<String> = Vec::new();
            for (entity_id, role) in &parties {
                if let Some(pcs) = entity_postcodes.get(entity_id) {
                    let target = match *role {
                        "buyer" => &mut buyer_pcs,
                        "supplier" => &mut supplier_pcs,
                        _ => continue,
                    };
                    target.extend(pcs.iter().cloned());
                    all_postcodes.extend(pcs.iter().cloned());
                }
            }
            buyer_pcs.sort();
            supplier_pcs.sort();
            work.push(ContractWork {
                record_id: record_id.clone(),
                contract_id: contract_id.clone(),
                contract_data: contract_data.clone(),
                buyer_postcodes: buyer_pcs,
                supplier_postcodes: supplier_pcs,
            });
        }

        // ── 5. Batch-resolve postcodes → ward codes ───────────────────────────
        let postcodes_vec: Vec<String> = all_postcodes.into_iter().collect();
        let mut ward_map: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for chunk in postcodes_vec.chunks(100) {
            match self.postcodes_client.resolve_batch(chunk).await {
                Ok(m) => ward_map.extend(m),
                Err(e) => {
                    tracing::warn!(?e, "postcodes.io batch error — skipping chunk");
                }
            }
        }

        // ── 5b. Terminated-postcode fallback for what the bulk route rejected ─
        // One GET pair per postcode, so bounded by the (small) residue, not the corpus.
        let mut terminated_map: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut unresolved: Vec<&String> = postcodes_vec
            .iter()
            .filter(|p| !ward_map.contains_key(*p))
            .collect();
        unresolved.sort();
        for pc in unresolved {
            match self.postcodes_client.resolve_terminated(pc).await {
                Ok(Some(ward)) => {
                    terminated_map.insert(pc.clone(), ward);
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(?e, postcode = %pc, "postcodes.io terminated lookup failed — skipping");
                }
            }
        }
        tracing::info!(
            live = ward_map.len(),
            terminated = terminated_map.len(),
            unresolved = postcodes_vec.len() - ward_map.len() - terminated_map.len(),
            "Ward backfill: postcode resolution"
        );

        // ── 6. Update contracts and create Ward/Represents events ─────────────
        let mut inserted_wards: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut inserted_represents: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        for cw in &work {
            let buyer = first_resolved(&cw.buyer_postcodes, &ward_map, &terminated_map);
            let supplier = first_resolved(&cw.supplier_postcodes, &ward_map, &terminated_map);
            let buyer_ward = buyer.map(|(w, _)| w);
            let supplier_ward = supplier.map(|(w, _)| w);

            if buyer_ward.is_none() && supplier_ward.is_none() {
                tracing::debug!(contract = %cw.contract_id, "no ward resolved — skipping");
                stats.skipped += 1;
                continue;
            }

            if !self.dry_run {
                let primary_ward = buyer_ward.or(supplier_ward).unwrap().to_string();

                // ── write Ward node + Represents edges FIRST (commit marker comes last) ──
                if let Some((bw, source)) = buyer {
                    let ward_id = format!("ward:{bw}");
                    if inserted_wards.insert(ward_id.clone()) {
                        insert_node_event(
                            &self.session,
                            &ward_id,
                            &NodeWeight::Ward(WardData {
                                name: bw.to_string(),
                            }),
                        )
                        .await?;
                    }
                    let edge_key = (ward_id.clone(), cw.contract_id.clone());
                    if inserted_represents.insert(edge_key) {
                        insert_edge_event(
                            &self.session,
                            &ward_id,
                            &cw.contract_id,
                            &EdgeWeight::Represents {
                                meta: EdgeMeta {
                                    created_at: Utc::now(),
                                    effective_from: None,
                                    effective_to: None,
                                },
                                data: RepresentsData {
                                    details: format!(
                                        "backfill:buyer_ward{}",
                                        source.details_suffix()
                                    ),
                                },
                            },
                        )
                        .await?;
                    }
                }

                if let Some((sw, source)) = supplier {
                    if Some(sw) != buyer_ward {
                        let ward_id = format!("ward:{sw}");
                        if inserted_wards.insert(ward_id.clone()) {
                            insert_node_event(
                                &self.session,
                                &ward_id,
                                &NodeWeight::Ward(WardData {
                                    name: sw.to_string(),
                                }),
                            )
                            .await?;
                        }
                        let edge_key = (ward_id.clone(), cw.contract_id.clone());
                        if inserted_represents.insert(edge_key) {
                            insert_edge_event(
                                &self.session,
                                &ward_id,
                                &cw.contract_id,
                                &EdgeWeight::Represents {
                                    meta: EdgeMeta {
                                        created_at: Utc::now(),
                                        effective_from: None,
                                        effective_to: None,
                                    },
                                    data: RepresentsData {
                                        details: format!(
                                            "backfill:supplier_ward{}",
                                            source.details_suffix()
                                        ),
                                    },
                                },
                            )
                            .await?;
                        }
                    }
                }

                // ── stamp ward_code on contract LAST (commit marker) ──────────────────
                let updated = NodeWeight::Contract(ContractData {
                    ward_code: Some(primary_ward.clone()),
                    ..cw.contract_data.clone()
                });
                update_node_event_by_record(&self.session, &cw.record_id, &updated).await?;
            }
            stats.matched += 1;
        }

        tracing::info!(
            scanned = stats.scanned,
            matched = stats.matched,
            skipped = stats.skipped,
            errors = stats.errors,
            "Ward backfill complete"
        );
        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_resolve_batch_returns_ward_codes() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 16384];
                let _ = stream.read(&mut buf);
                let body = r#"{"status":200,"result":[{"query":"SW1A1AA","result":{"codes":{"admin_ward":"E05001234"}}}]}"#;
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                );
            }
        });
        let client = PostcodesIoClient::new_with_base(format!("http://127.0.0.1:{}", port));
        let postcodes = vec!["SW1A1AA".to_string()];
        let result = client.resolve_batch(&postcodes).await.unwrap();
        assert_eq!(result.get("SW1A1AA").map(String::as_str), Some("E05001234"));
    }

    #[tokio::test]
    async fn test_resolve_batch_handles_null_result() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 16384];
                let _ = stream.read(&mut buf);
                let body = r#"{"status":200,"result":[{"query":"INVALID","result":null}]}"#;
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                );
            }
        });
        let client = PostcodesIoClient::new_with_base(format!("http://127.0.0.1:{}", port));
        let result = client
            .resolve_batch(&["INVALID".to_string()])
            .await
            .unwrap();
        assert!(result.is_empty(), "null result should be skipped");
    }

    #[tokio::test]
    async fn test_resolve_batch_empty_input() {
        let client = PostcodesIoClient::new();
        let result = client.resolve_batch(&[]).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_resolve_batch_normalises_postcode_key() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 16384];
                let _ = stream.read(&mut buf);
                // postcodes.io echoes back the query as-is
                let body = r#"{"status":200,"result":[{"query":"sw1a 1aa","result":{"codes":{"admin_ward":"E05001234"}}}]}"#;
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(), body
                    )
                    .as_bytes(),
                );
            }
        });
        let client = PostcodesIoClient::new_with_base(format!("http://127.0.0.1:{}", port));
        let result = client
            .resolve_batch(&["sw1a 1aa".to_string()])
            .await
            .unwrap();
        // Key is normalised: uppercase, no spaces
        assert!(
            result.contains_key("SW1A1AA"),
            "key should be normalised to SW1A1AA"
        );
    }

    /// Serve `responses` on successive connections (one HTTP exchange each, `Connection: close`),
    /// so a multi-request client method can be exercised against a fixed script.
    fn serve_sequence(responses: Vec<(u16, &'static str)>) -> u16 {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for (status, body) in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buf = [0u8; 16384];
                let _ = stream.read(&mut buf);
                let reason = if status == 200 { "OK" } else { "Not Found" };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                );
            }
        });
        port
    }

    #[tokio::test]
    async fn test_resolve_terminated_reverse_geocodes_centroid() {
        let port = serve_sequence(vec![
            (
                200,
                r#"{"status":200,"result":{"postcode":"B4 6DG","year_terminated":2019,"month_terminated":6,"longitude":-1.897,"latitude":52.483}}"#,
            ),
            (
                200,
                r#"{"status":200,"result":[{"postcode":"B4 6DH","codes":{"admin_ward":"E05011151"}}]}"#,
            ),
        ]);
        let client = PostcodesIoClient::new_with_base(format!("http://127.0.0.1:{port}"));
        let ward = client.resolve_terminated("B46DG").await.unwrap();
        assert_eq!(ward.as_deref(), Some("E05011151"));
    }

    #[tokio::test]
    async fn test_resolve_terminated_unknown_postcode_is_none() {
        let port = serve_sequence(vec![(
            404,
            r#"{"status":404,"error":"Terminated postcode not found"}"#,
        )]);
        let client = PostcodesIoClient::new_with_base(format!("http://127.0.0.1:{port}"));
        let ward = client.resolve_terminated("GX111AA").await.unwrap();
        assert!(
            ward.is_none(),
            "a postcode unknown to both endpoints must resolve to None"
        );
    }

    #[tokio::test]
    async fn test_resolve_terminated_empty_reverse_result_is_none() {
        let port = serve_sequence(vec![
            (
                200,
                r#"{"status":200,"result":{"postcode":"ZZ1 1ZZ","longitude":-1.0,"latitude":52.0}}"#,
            ),
            (200, r#"{"status":200,"result":null}"#),
        ]);
        let client = PostcodesIoClient::new_with_base(format!("http://127.0.0.1:{port}"));
        let ward = client.resolve_terminated("ZZ11ZZ").await.unwrap();
        assert!(ward.is_none());
    }

    #[test]
    fn first_resolved_prefers_live_lookup_and_skips_unresolvable_postcodes() {
        let live: HashMap<String, String> = [("LS14AP".to_string(), "E05001411".to_string())]
            .into_iter()
            .collect();
        let terminated: HashMap<String, String> = [("B46DG".to_string(), "E05011151".to_string())]
            .into_iter()
            .collect();
        // First postcode unresolvable anywhere, second only via the terminated fallback, third
        // live: the live one wins over the terminated one regardless of order.
        let pcs = [
            "GX111AA".to_string(),
            "B46DG".to_string(),
            "LS14AP".to_string(),
        ];
        assert_eq!(
            first_resolved(&pcs, &live, &terminated),
            Some(("E05001411", WardSource::Live))
        );
        let pcs = ["GX111AA".to_string(), "B46DG".to_string()];
        assert_eq!(
            first_resolved(&pcs, &live, &terminated),
            Some(("E05011151", WardSource::TerminatedPostcode))
        );
        let pcs = ["GX111AA".to_string()];
        assert_eq!(first_resolved(&pcs, &live, &terminated), None);
        assert_eq!(first_resolved(&[], &live, &terminated), None);
    }
}
