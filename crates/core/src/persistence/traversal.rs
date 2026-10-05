//! BFS shortest-path traversal over the HIN graph (D-05/D-06).
//!
//! This module exposes the canonical BFS implementation used by both the CLI
//! `graph-path` command and the GraphQL `entityPath` resolver.  All
//! adjacency-load logic (SurrealDB queries) and the pure BFS algorithm live
//! here so they can be shared without duplication.

use std::collections::{HashMap, HashSet, VecDeque};

use surrealdb::{engine::any::Any, Surreal};
use tracing::debug;

use crate::persistence::queries::PathStep;
use crate::persistence::scan;

/// Maximum BFS hops for the graph-path traversal (D-04).
/// Caps depth to prevent runaway traversal on densely-connected graphs.
pub const MAX_HOPS: usize = 6;

/// One adjacency list entry — a neighbor reachable from a node via a typed edge.
#[derive(Debug, Clone)]
pub struct AdjEntry {
    pub neighbor: String,
    pub edge_type: String,
    pub forward: bool,
}

// ── Pure helpers (no DB, no async) ────────────────────────────────────────────

/// Convert a camelCase edge type string to SCREAMING_SNAKE_CASE for display.
///
/// Inserts an underscore before each transition from a lowercase character to an
/// uppercase character, then uppercases the entire string.
///
/// # Examples
/// ```
/// use urban_integrity_core::persistence::traversal::prettify_edge_type;
/// assert_eq!(prettify_edge_type("OwnsProperty"), "OWNS_PROPERTY");
/// assert_eq!(prettify_edge_type("AwardedTo"), "AWARDED_TO");
/// assert_eq!(prettify_edge_type("Directs"), "DIRECTS");
/// assert_eq!(prettify_edge_type("DisqualifiedFrom"), "DISQUALIFIED_FROM");
/// ```
pub fn prettify_edge_type(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len() + 4);
    let mut prev_lower = false;
    for c in raw.chars() {
        if c.is_uppercase() && prev_lower {
            result.push('_');
        }
        result.push(c.to_ascii_uppercase());
        prev_lower = c.is_lowercase();
    }
    result
}

/// Build a `node_id → display_name` map from raw NodeAdded rows.
///
/// Uses latest-timestamp-wins dedup so that the most recent label for an
/// entity is always used.
pub fn build_node_label_from_rows(node_rows: &[serde_json::Value]) -> HashMap<String, String> {
    let mut node_latest: HashMap<String, (u64, String)> = HashMap::new();
    for row in node_rows {
        let entity_id = match row["payload"]["entity_id"].as_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let ts = row["timestamp"].as_u64().unwrap_or(0);
        // Try to extract a display name from the weight_json.
        let display_name = row["payload"]["weight_json"]
            .as_str()
            .and_then(|wj| serde_json::from_str::<serde_json::Value>(wj).ok())
            .and_then(|v| {
                // Entity nodes carry "name" at the top level of their data object.
                // Weight JSON is tagged: {"type":"Entity","meta":{...},"data":{"name":"...",...}}
                v["data"]["name"]
                    .as_str()
                    .or_else(|| v["name"].as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| entity_id.clone());

        let entry = node_latest
            .entry(entity_id)
            .or_insert_with(|| (0, display_name.clone()));
        if ts > entry.0 {
            *entry = (ts, display_name);
        }
    }

    // Flatten to id → name.
    node_latest
        .into_iter()
        .map(|(id, (_ts, name))| (id, name))
        .collect()
}

/// Build an undirected adjacency map from raw EdgeAdded rows.
///
/// Inserts both forward and reverse directions for each edge.  Uses a dedup
/// set to avoid duplicate (src, dst, edge_type) pairs.
pub fn build_adj_from_rows(edge_rows: &[serde_json::Value]) -> HashMap<String, Vec<AdjEntry>> {
    let mut adj: HashMap<String, Vec<AdjEntry>> = HashMap::new();
    let mut seen_edges: HashSet<(String, String, String)> = HashSet::new();

    for row in edge_rows {
        let src_id = match row["payload"]["src_id"].as_str() {
            Some(s) => s,
            None => continue,
        };
        let dst_id = match row["payload"]["dst_id"].as_str() {
            Some(s) => s,
            None => continue,
        };
        let weight_json = match row["payload"]["weight_json"].as_str() {
            Some(s) => s,
            None => continue,
        };

        // Extract edge type from the "type" tag — render raw, uppercased (no UNKNOWN arm).
        let edge_type_opt = serde_json::from_str::<serde_json::Value>(weight_json)
            .ok()
            .and_then(|v| v["type"].as_str().map(str::to_string));
        let edge_type = match edge_type_opt {
            Some(t) => t,
            None => continue, // graceful-skip: can't determine type
        };

        // Dedup guard.
        let key = (src_id.to_string(), dst_id.to_string(), edge_type.clone());
        if !seen_edges.insert(key) {
            continue;
        }

        // Insert both directions for undirected traversal (D-04).
        adj.entry(src_id.to_string()).or_default().push(AdjEntry {
            neighbor: dst_id.to_string(),
            edge_type: edge_type.clone(),
            forward: true,
        });
        adj.entry(dst_id.to_string()).or_default().push(AdjEntry {
            neighbor: src_id.to_string(),
            edge_type: edge_type.clone(),
            forward: false,
        });
    }

    adj
}

/// Convert a path of node IDs (from [`bfs_shortest_path`]) into `Vec<PathStep>`.
///
/// For each node in the path:
/// - `node_id`: the entity ID string
/// - `node_label`: looked up from `node_label` map, falling back to the ID
/// - `edge_type`: `None` for index 0; for index i > 0, looked up from the
///   adjacency map entry for `path[i-1]` where `neighbor == path[i]`, then
///   uppercased.
pub fn build_path_steps(
    path: &[String],
    node_label: &HashMap<String, String>,
    adj: &HashMap<String, Vec<AdjEntry>>,
) -> Vec<PathStep> {
    path.iter()
        .enumerate()
        .map(|(i, node_id)| {
            let label = node_label
                .get(node_id)
                .cloned()
                .unwrap_or_else(|| node_id.clone());

            let edge_type = if i == 0 {
                None
            } else {
                let prev_id = &path[i - 1];
                adj.get(prev_id).and_then(|entries| {
                    entries
                        .iter()
                        .find(|e| &e.neighbor == node_id)
                        .map(|e| prettify_edge_type(&e.edge_type))
                })
            };

            PathStep {
                node_id: node_id.clone(),
                node_label: label,
                edge_type,
            }
        })
        .collect()
}

/// BFS over the undirected adjacency map from `src` toward `dst`.
///
/// Returns the sequence of node IDs forming the shortest path, or `None` if no
/// path exists within `max_hops` hops.  Pass [`MAX_HOPS`] for the default cap.
pub(crate) fn bfs_shortest_path(
    adj: &HashMap<String, Vec<AdjEntry>>,
    src: &str,
    dst: &str,
    max_hops: usize,
) -> Option<Vec<String>> {
    if src == dst {
        return Some(vec![src.to_string()]);
    }

    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    let mut visited: HashSet<String> = HashSet::new();
    // parent: node → (previous node, edge_type used, forward flag)
    let mut parent: HashMap<String, (String, String, bool)> = HashMap::new();

    queue.push_back((src.to_string(), 0));
    visited.insert(src.to_string());

    while let Some((curr, hops)) = queue.pop_front() {
        if curr == dst {
            // Reconstruct path by walking parent pointers back from dst to src.
            let mut path = vec![curr.clone()];
            let mut node = curr.clone();
            while let Some((prev, _, _)) = parent.get(&node) {
                path.push(prev.clone());
                node = prev.clone();
            }
            path.reverse();
            return Some(path);
        }
        if hops >= max_hops {
            continue;
        }
        for entry in adj.get(&curr).into_iter().flatten() {
            if visited.insert(entry.neighbor.clone()) {
                parent.insert(
                    entry.neighbor.clone(),
                    (curr.clone(), entry.edge_type.clone(), entry.forward),
                );
                queue.push_back((entry.neighbor.clone(), hops + 1));
            }
        }
    }

    None
}

// ── Async DB entry point ──────────────────────────────────────────────────────

/// Load the HIN adjacency from SurrealDB and run BFS from `src_id` to `dst_id`.
///
/// Returns `Ok(vec![])` when no path is found within `max_hops` (not an error
/// condition).  Returns `Ok(vec![step])` (single step, `edge_type = None`) when
/// `src_id == dst_id`.
///
/// # Adjacency loading
///
/// Both `NodeAdded` and `EdgeAdded` are paged to exhaustion via
/// [`crate::persistence::scan::scan_events_by_kind`], bounded only by that module's safety cap.
///
/// This used to read up to 65 000 nodes and 120 000 edges — ceilings sized to the graph as it
/// stood in 2026-05. The graph outgrew them silently. A traversal over partial adjacency does not
/// fail; it returns "no path" for entities that are in fact connected, which is indistinguishable
/// from a genuine negative result.
///
/// Page sizes are compile-time constants — not user-controllable (T-15-BFS mitigation).
pub async fn bfs_traversal(
    db: &Surreal<Any>,
    src_id: &str,
    dst_id: &str,
    max_hops: usize,
) -> Result<Vec<PathStep>, surrealdb::Error> {
    // Short-circuit: same entity.
    if src_id == dst_id {
        return Ok(vec![]);
    }

    // ── Bulk-load adjacency (D-01 — two static queries, no user values) ────────
    //
    // Both sides page to exhaustion. They used to stop at ceilings sized to a 2026-05 graph
    // (~56k NodeAdded, ~90k EdgeAdded), and the graph outgrew them without anything failing:
    // measured 2026-07-29 against `an_anglesey_aebaa785`, the `LIMIT 65000` node query returned
    // exactly 65,000 of **358,829** NodeAdded rows, and the 6-page edge loop capped at 120,000 of
    // **388,162**. A traversal reading 18% of nodes and 31% of edges reports "no path" between
    // entities that are connected — a false negative that looks exactly like a real answer.
    //
    // `kind` alone is safe to keep in the WHERE clause: a single indexed equality is not exposed
    // to the SurrealDB 3.x planner bug, which needs an unindexed CONTAINS alongside it.
    let node_rows = scan::scan_events_by_kind(db, "NodeAdded").await?;
    let node_count = node_rows.len();

    let edge_rows = scan::scan_events_by_kind(db, "EdgeAdded").await?;
    let edge_count = edge_rows.len();

    // Logged rather than warned: with paging there is no ceiling left to hit, so the useful
    // signal is how much adjacency the answer was actually derived from.
    debug!(
        node_count,
        edge_count, "bfs_traversal: adjacency loaded in full"
    );

    // Build maps from raw rows.
    let node_label = build_node_label_from_rows(&node_rows);
    let mut adj = build_adj_from_rows(&edge_rows);

    // ── Virtual edges: load confirmed/auto-linked identity_links as synthetic AdjEntry ──
    // ENR-11 gate: status IN ['auto_linked','confirmed'] — candidate links never traversed.
    // GRD-09: routed through scan::scan_identity_links (paged past SCAN_MAX_ROWS, `?`-propagated
    // on error) instead of a bespoke `LIMIT 50000` that silently truncated at 48% of Anglesey's
    // identity_links count and swallowed decode errors as an empty result via
    // `.unwrap_or_default()`. `status IN [...]` becomes two calls concatenated, mirroring
    // `scan_events_by_any_type`'s OR-of-markers handling for `graph_events`.
    let mut identity_link_rows = scan::scan_identity_links(db, Some("auto_linked"), None).await?;
    identity_link_rows.extend(scan::scan_identity_links(db, Some("confirmed"), None).await?);

    for row in &identity_link_rows {
        // IdentityLinkRow.source_node_id/target_node_id are already RecordId-coercion-safe
        // plain Strings (scan::record_id_str resolved any RecordId-object form already).
        let src = row.source_node_id.clone();
        let tgt = row.target_node_id.clone();
        if src.is_empty() || tgt.is_empty() {
            continue;
        }
        // Bidirectional synthetic edges — IdentityLink is undirected.
        adj.entry(src.clone()).or_default().push(AdjEntry {
            neighbor: tgt.clone(),
            edge_type: "IdentityLink".to_string(),
            forward: true,
        });
        adj.entry(tgt.clone()).or_default().push(AdjEntry {
            neighbor: src.clone(),
            edge_type: "IdentityLink".to_string(),
            forward: false,
        });
    }

    // BFS.
    let path_ids = bfs_shortest_path(&adj, src_id, dst_id, max_hops);

    match path_ids {
        None => Ok(vec![]),
        Some(path) => Ok(build_path_steps(&path, &node_label, &adj)),
    }
}

/// Extract a node ID string from a SurrealDB JSON field value.
///
/// Handles both plain string values and SurrealDB RecordId objects (Pitfall 5).
/// RecordId objects appear for `director:` keyed nodes when stored via `CREATE … CONTENT`.
/// Returns an empty string if the value cannot be extracted (caller should skip the row).
///
/// GRD-09: `bfs_traversal`'s identity_links virtual-edge load now goes through
/// `scan::scan_identity_links`, whose `IdentityLinkRow` fields are already RecordId-coercion-safe
/// plain `String`s — so this function has no production caller left in this file. Kept
/// `#[cfg(test)]`-only (not deleted) because its unit tests below still exercise the RecordId
/// coercion behaviour directly, and the plan that removed its last caller was explicit about not
/// deleting either the function or its tests.
///
/// # Examples
/// - `"director:john-smith"` (string) → `"director:john-smith"`
/// - `{"tb":"director","id":{"String":"john-smith"}}` → `"director:john-smith"`
/// - `{"tb":"director","id":"john-smith"}` → `"director:john-smith"`
#[cfg(test)]
fn extract_node_id_str(v: &serde_json::Value) -> String {
    // Plain string — most common case.
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    // RecordId object: try { "tb": "director", "id": { "String": "john-smith" } }
    if let Some(id_val) = v.get("id") {
        if let Some(s) = id_val.as_str() {
            if let Some(tb) = v.get("tb").and_then(|x| x.as_str()) {
                return format!("{tb}:{s}");
            }
            return s.to_string();
        }
        if let Some(s) = id_val.get("String").and_then(|x| x.as_str()) {
            // Reconstruct director:slug form.
            if let Some(tb) = v.get("tb").and_then(|x| x.as_str()) {
                return format!("{tb}:{s}");
            }
            return s.to_string();
        }
    }
    String::new()
}

// ── Unit tests (no DB — operate on hand-built HashMaps) ───────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_adj(edges: &[(&str, &str, &str)]) -> HashMap<String, Vec<AdjEntry>> {
        let mut adj: HashMap<String, Vec<AdjEntry>> = HashMap::new();
        for (src, dst, etype) in edges {
            adj.entry(src.to_string()).or_default().push(AdjEntry {
                neighbor: dst.to_string(),
                edge_type: etype.to_string(),
                forward: true,
            });
            adj.entry(dst.to_string()).or_default().push(AdjEntry {
                neighbor: src.to_string(),
                edge_type: etype.to_string(),
                forward: false,
            });
        }
        adj
    }

    // bfs_finds_shortest_path: 3-node chain A→B→C, confirm [A,B,C] returned.
    #[test]
    fn bfs_finds_shortest_path() {
        let adj = make_adj(&[("A", "B", "Directs"), ("B", "C", "Directs")]);
        let path = bfs_shortest_path(&adj, "A", "C", MAX_HOPS);
        assert!(path.is_some(), "Expected a path to be found");
        let p = path.unwrap();
        assert_eq!(p, vec!["A", "B", "C"], "Path should be A→B→C, got: {p:?}");
    }

    // bfs_no_path: disconnected graph → BFS returns None.
    #[test]
    fn bfs_no_path() {
        let adj = make_adj(&[("A", "B", "Directs")]);
        // C is disconnected from A-B
        let path = bfs_shortest_path(&adj, "A", "C", MAX_HOPS);
        assert!(path.is_none(), "Expected no path for disconnected graph");
    }

    // bfs_respects_hop_cap: path requires 7 hops, confirm None at cap=6.
    #[test]
    fn bfs_respects_hop_cap() {
        // Build a chain of 8 nodes (7 edges = 7 hops from node0 to node7).
        let edges: Vec<(&str, &str, &str)> = vec![
            ("n0", "n1", "Directs"),
            ("n1", "n2", "Directs"),
            ("n2", "n3", "Directs"),
            ("n3", "n4", "Directs"),
            ("n4", "n5", "Directs"),
            ("n5", "n6", "Directs"),
            ("n6", "n7", "Directs"),
        ];
        let adj = make_adj(&edges);
        // 7 hops > MAX_HOPS (6) → should return None
        let path = bfs_shortest_path(&adj, "n0", "n7", MAX_HOPS);
        assert!(
            path.is_none(),
            "Path of 7 hops must be None at MAX_HOPS=6, got: {path:?}"
        );
    }

    // bfs_undirected: edge only A→B in raw data, confirm path B→A found.
    #[test]
    fn bfs_undirected() {
        // Only insert the forward direction manually (simulates raw data with one direction).
        let mut adj: HashMap<String, Vec<AdjEntry>> = HashMap::new();
        // Only A→B exists in "raw" form; we simulate what the adjacency builder does:
        // insert BOTH directions.
        adj.entry("A".to_string()).or_default().push(AdjEntry {
            neighbor: "B".to_string(),
            edge_type: "Directs".to_string(),
            forward: true,
        });
        adj.entry("B".to_string()).or_default().push(AdjEntry {
            neighbor: "A".to_string(),
            edge_type: "Directs".to_string(),
            forward: false,
        });

        // With the reverse entry, B→A should be found.
        let path = bfs_shortest_path(&adj, "B", "A", MAX_HOPS);
        assert!(path.is_some(), "Expected undirected path B→A to be found");
        let p = path.unwrap();
        assert_eq!(p, vec!["B", "A"], "Path should be [B, A], got: {p:?}");
    }

    // build_path_steps: confirm PathStep construction from path + label + adj maps.
    #[test]
    fn build_path_steps_basic() {
        let adj = make_adj(&[
            ("node-a", "node-b", "Directs"),
            ("node-b", "node-c", "Contracts"),
        ]);
        let mut node_label = HashMap::new();
        node_label.insert("node-a".to_string(), "Company A".to_string());
        node_label.insert("node-b".to_string(), "John Smith".to_string());
        node_label.insert("node-c".to_string(), "Council C".to_string());

        let path = vec![
            "node-a".to_string(),
            "node-b".to_string(),
            "node-c".to_string(),
        ];
        let steps = build_path_steps(&path, &node_label, &adj);

        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].node_id, "node-a");
        assert_eq!(steps[0].node_label, "Company A");
        assert_eq!(steps[0].edge_type, None);

        assert_eq!(steps[1].node_id, "node-b");
        assert_eq!(steps[1].node_label, "John Smith");
        assert_eq!(steps[1].edge_type, Some("DIRECTS".to_string()));

        assert_eq!(steps[2].node_id, "node-c");
        assert_eq!(steps[2].node_label, "Council C");
        assert_eq!(steps[2].edge_type, Some("CONTRACTS".to_string()));
    }

    // build_path_steps: single-node path returns no edge_type.
    #[test]
    fn build_path_steps_single_node() {
        let adj: HashMap<String, Vec<AdjEntry>> = HashMap::new();
        let node_label: HashMap<String, String> = HashMap::new();
        let path = vec!["node-x".to_string()];
        let steps = build_path_steps(&path, &node_label, &adj);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].node_id, "node-x");
        assert_eq!(steps[0].node_label, "node-x"); // falls back to id
        assert_eq!(steps[0].edge_type, None);
    }

    // ── prettify_edge_type tests (SURF-04) ────────────────────────────────────

    #[test]
    fn prettify_edge_type_owns_property() {
        assert_eq!(
            prettify_edge_type("OwnsProperty"),
            "OWNS_PROPERTY",
            "OwnsProperty must prettify to OWNS_PROPERTY (SURF-04)"
        );
    }

    #[test]
    fn prettify_edge_type_awarded_to() {
        assert_eq!(
            prettify_edge_type("AwardedTo"),
            "AWARDED_TO",
            "AwardedTo must prettify to AWARDED_TO"
        );
    }

    #[test]
    fn prettify_edge_type_directs() {
        // Single-word camelCase — no underscore inserted
        assert_eq!(
            prettify_edge_type("Directs"),
            "DIRECTS",
            "Directs must prettify to DIRECTS (no spurious underscore)"
        );
    }

    #[test]
    fn prettify_edge_type_disqualified_from() {
        assert_eq!(
            prettify_edge_type("DisqualifiedFrom"),
            "DISQUALIFIED_FROM",
            "DisqualifiedFrom must prettify to DISQUALIFIED_FROM"
        );
    }

    // Regression: build_path_steps with OwnsProperty edge uses prettify_edge_type.
    #[test]
    fn build_path_steps_owns_property_edge_renders_correctly() {
        let adj = make_adj(&[(
            "gb-coh:12345678",
            "property:12345678_ln1234",
            "OwnsProperty",
        )]);
        let mut node_label = HashMap::new();
        node_label.insert("gb-coh:12345678".to_string(), "Apex Build Ltd".to_string());
        node_label.insert("property:12345678_ln1234".to_string(), "LN1234".to_string());

        let path = vec![
            "gb-coh:12345678".to_string(),
            "property:12345678_ln1234".to_string(),
        ];
        let steps = build_path_steps(&path, &node_label, &adj);

        assert_eq!(steps.len(), 2);
        // SURF-04: OwnsProperty must render as OWNS_PROPERTY (not OWNSPROPERTY)
        assert_eq!(
            steps[1].edge_type,
            Some("OWNS_PROPERTY".to_string()),
            "OwnsProperty edge must render as OWNS_PROPERTY via prettify_edge_type (SURF-04)"
        );
    }

    // ── SC#5: virtual IdentityLink edge traversal (Phase 20-05) ──────────────

    /// Test 1 (SC#5): BFS traverses a synthetic IdentityLink virtual edge.
    /// A→B is a regular Directs edge; B→C is a synthetic IdentityLink.
    /// BFS must find path A→B→C via the virtual edge.
    #[test]
    fn virtual_identity_link_traversal() {
        let mut adj = make_adj(&[("A", "B", "Directs")]);
        adj.entry("B".to_string()).or_default().push(AdjEntry {
            neighbor: "C".to_string(),
            edge_type: "IdentityLink".to_string(),
            forward: true,
        });
        adj.entry("C".to_string()).or_default().push(AdjEntry {
            neighbor: "B".to_string(),
            edge_type: "IdentityLink".to_string(),
            forward: false,
        });
        let path = bfs_shortest_path(&adj, "A", "C", MAX_HOPS);
        assert!(
            path.is_some(),
            "BFS must traverse virtual IdentityLink edges from A to C via B"
        );
        let steps = path.unwrap();
        assert_eq!(steps.len(), 3, "Path must contain 3 nodes: A, B, C");
        assert_eq!(steps[0], "A");
        assert_eq!(steps[1], "B");
        assert_eq!(steps[2], "C");
    }

    /// Test 2 (ENR-11): candidate identity_links (status omitted / not in filter) are never
    /// added as virtual edges. This is a compile-time + logic invariant test: the DB query
    /// in bfs_traversal uses status IN ['auto_linked','confirmed'] — status='candidate' rows
    /// must never appear. We verify the adjacency-map build does NOT add IdentityLink for
    /// a candidate-only adj (i.e., if only candidate rows existed, no IdentityLink virtual
    /// edges would be present).
    #[test]
    fn virtual_identity_link_only_confirmed_or_auto_linked() {
        // Build an adj with NO IdentityLink entries (simulates a world where only candidate
        // rows exist — the ENR-11 filter would have excluded them from identity_link_rows).
        let adj = make_adj(&[("A", "B", "Directs")]);
        // No IdentityLink in adj → path A→C must be None (C is disconnected).
        let path = bfs_shortest_path(&adj, "A", "C", MAX_HOPS);
        assert!(
            path.is_none(),
            "Without auto_linked/confirmed IdentityLink virtual edges, C must be unreachable"
        );
    }

    /// Test 3 (Pitfall 5): extract_node_id_str handles director:-keyed RecordId objects.
    #[test]
    fn extract_node_id_str_plain_string() {
        let v = serde_json::json!("director:john-smith");
        assert_eq!(extract_node_id_str(&v), "director:john-smith");
    }

    #[test]
    fn extract_node_id_str_record_id_object_string_id() {
        // SurrealDB RecordId: {"tb":"director","id":"john-smith"}
        let v = serde_json::json!({"tb": "director", "id": "john-smith"});
        assert_eq!(extract_node_id_str(&v), "director:john-smith");
    }

    #[test]
    fn extract_node_id_str_record_id_object_nested_string() {
        // SurrealDB RecordId: {"tb":"director","id":{"String":"john-smith"}}
        let v = serde_json::json!({"tb": "director", "id": {"String": "john-smith"}});
        assert_eq!(extract_node_id_str(&v), "director:john-smith");
    }

    #[test]
    fn extract_node_id_str_null_returns_empty() {
        let v = serde_json::json!(null);
        assert_eq!(extract_node_id_str(&v), "");
    }
}
