//! Arrow IPC serialisation of the population-scoped nodes/links table (plan 44-06, CNV-04).
//!
//! **Wire format** (documented here since plan 44-08's HTTP handler and the frontend's fetch
//! code both need to agree on it, per this plan's own `<action>` text): [`build_arrow_ipc`]
//! returns TWO independent Arrow IPC streams concatenated, each prefixed by its own 4-byte
//! little-endian `u32` byte length so a reader can split the buffer without parsing the Arrow
//! IPC format itself:
//!
//! ```text
//! [nodes_len: u32 LE][nodes_stream_bytes][links_len: u32 LE][links_stream_bytes]
//! ```
//!
//! Two separate streams — not one combined schema with a row-type discriminator column — because
//! Cosmograph's own point/link data model (`prepareCosmographData`) wants points and links as two
//! separate tables, not one union table with half its columns null on every row.
//!
//! **Nodes stream schema:** `id` (Utf8), `kind` (Utf8, drives D-15's per-kind colour), `name`
//! (Utf8, for the picker/side card). **Links stream schema:** `source` (Utf8), `target` (Utf8),
//! `edge_type` (Utf8). Per-edge `effective_from`/`effective_to` are deliberately NOT included in
//! this bulk payload — the side card resolves those only for the clicked node's edges, via a
//! separate server-side lookup on click (plan 44-08), never shipped for all ~331k edges up front.
//!
//! **Scope:** every row comes from the population-scoped node set
//! ([`PopulationScope::node_ids`]), never the full unfiltered [`CsrSnapshot`] (GRA-06,
//! phase 44 research Pitfall 2) — a link is only emitted when BOTH its endpoints are in scope, so
//! every link's `source`/`target` id is guaranteed to be present among the node table's `id`
//! column (CNV-04's referential-integrity requirement).

use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{ArrayRef, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::ipc::writer::StreamWriter;
use arrow::record_batch::RecordBatch;
use petgraph::visit::EdgeRef;

use urban_integrity_core::graph::snapshot::CsrSnapshot;
use urban_integrity_core::graph::storage::GraphStorage;
use urban_integrity_schema::types::node::NodeWeight;

use super::csr_view::edge_type_name;
use super::population_scope::PopulationScope;

/// Exhaustive `NodeWeight` -> stable `kind` column value. Mirrors `csr_view::edge_type_name`'s
/// exhaustiveness discipline — no `_` wildcard arm, so a 12th `NodeWeight` variant fails this
/// match at compile time instead of silently mapping to nothing.
pub fn node_kind_name(w: &NodeWeight) -> &'static str {
    match w {
        NodeWeight::Entity(_) => "Entity",
        NodeWeight::Officer(_) => "Officer",
        NodeWeight::Location(_) => "Location",
        NodeWeight::Contract(_) => "Contract",
        NodeWeight::Ward(_) => "Ward",
        NodeWeight::PoliticalActor(_) => "PoliticalActor",
        NodeWeight::Psc(_) => "Psc",
        NodeWeight::Charity(_) => "Charity",
        NodeWeight::Property(_) => "Property",
        NodeWeight::OffshoreEntity(_) => "OffshoreEntity",
        NodeWeight::Disqualification(_) => "Disqualification",
    }
}

/// Derive a display name for `w`. Every variant except `Property`/`Disqualification` carries a
/// `.name: String` field directly. `PropertyData` has no `name` field (only `title_number`/
/// `tenure`/`price_paid`) — derive `"Property {title_number}"`, or `"Property"` if
/// `title_number` is `None`. `DisqualificationData` has no `name` field either — derive
/// `"Disqualification {case_number}"`, or `"Disqualification"` if `case_number` is `None`. Never
/// a bare empty string.
pub fn node_display_name(w: &NodeWeight) -> String {
    match w {
        NodeWeight::Entity(d) => d.name.clone(),
        NodeWeight::Officer(d) => d.name.clone(),
        NodeWeight::Location(d) => d.name.clone(),
        NodeWeight::Contract(d) => d.name.clone(),
        NodeWeight::Ward(d) => d.name.clone(),
        NodeWeight::PoliticalActor(d) => d.name.clone(),
        NodeWeight::Psc(d) => d.name.clone(),
        NodeWeight::Charity(d) => d.name.clone(),
        NodeWeight::Property(d) => match &d.title_number {
            Some(title_number) => format!("Property {title_number}"),
            None => "Property".to_string(),
        },
        NodeWeight::OffshoreEntity(d) => d.name.clone(),
        NodeWeight::Disqualification(d) => match &d.case_number {
            Some(case_number) => format!("Disqualification {case_number}"),
            None => "Disqualification".to_string(),
        },
    }
}

/// Build the population-scoped Arrow IPC payload described in this module's doc comment.
///
/// `spawn_blocking`-eligible: native, CPU/IO bound, no `.await` inside. Plan 44-08's HTTP
/// handler wraps this call in `tokio::task::spawn_blocking`.
pub fn build_arrow_ipc(
    storage: &GraphStorage,
    snapshot: &CsrSnapshot,
    scope: &PopulationScope,
) -> anyhow::Result<Vec<u8>> {
    // Filter storage's full node set down to the population scope, keyed by CSR `u32` index —
    // the same idx.index()-equals-CSR-index invariant `population_scope.rs`'s own doc comment
    // establishes (bake_csr sorts by that index before adding nodes to the Csr in that order).
    let mut scoped_nodes: Vec<(u32, String, NodeWeight)> = storage
        .snapshot_nodes()
        .into_iter()
        .filter_map(|(id, idx, weight, _event_id)| {
            let idx_u32 = idx.index() as u32;
            scope
                .node_ids
                .contains(&idx_u32)
                .then_some((idx_u32, id.0, weight))
        })
        .collect();
    scoped_nodes.sort_unstable_by_key(|(idx, _, _)| *idx);

    // O(1) idx -> EntityId-string lookup for resolving link endpoints below.
    let id_by_idx: HashMap<u32, &str> = scoped_nodes
        .iter()
        .map(|(idx, id, _)| (*idx, id.as_str()))
        .collect();

    let mut node_ids: Vec<String> = Vec::with_capacity(scoped_nodes.len());
    let mut node_kinds: Vec<&'static str> = Vec::with_capacity(scoped_nodes.len());
    let mut node_names: Vec<String> = Vec::with_capacity(scoped_nodes.len());
    for (_, id, weight) in &scoped_nodes {
        node_ids.push(id.clone());
        node_kinds.push(node_kind_name(weight));
        node_names.push(node_display_name(weight));
    }

    // Links: iterate each scoped node's outgoing edges (snapshot.graph.edges(node), petgraph
    // Csr's own outgoing-only iterator — mirrors csr_view::build_scoped_adjacency's own pattern
    // of considering each edge exactly once, from its source's outgoing list) and keep only
    // those whose target is ALSO in scope — this is what guarantees CNV-04's referential
    // integrity: every emitted source/target id is, by construction, also a node-table id.
    let mut link_sources: Vec<String> = Vec::new();
    let mut link_targets: Vec<String> = Vec::new();
    let mut link_edge_types: Vec<&'static str> = Vec::new();
    for &(idx, _, _) in &scoped_nodes {
        for edge_ref in snapshot.graph.edges(idx) {
            let target = edge_ref.target();
            if !scope.node_ids.contains(&target) {
                continue;
            }
            let (Some(&src_id), Some(&dst_id)) = (id_by_idx.get(&idx), id_by_idx.get(&target))
            else {
                // Both idx and target are in scope.node_ids, and id_by_idx was built from
                // exactly that set — this branch is unreachable, but never panics.
                continue;
            };
            link_sources.push(src_id.to_string());
            link_targets.push(dst_id.to_string());
            link_edge_types.push(edge_type_name(edge_ref.weight()));
        }
    }

    let nodes_schema = Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
    ]);
    let nodes_batch = RecordBatch::try_new(
        Arc::new(nodes_schema.clone()),
        vec![
            Arc::new(StringArray::from_iter_values(node_ids)) as ArrayRef,
            Arc::new(StringArray::from_iter_values(node_kinds)) as ArrayRef,
            Arc::new(StringArray::from_iter_values(node_names)) as ArrayRef,
        ],
    )?;

    let links_schema = Schema::new(vec![
        Field::new("source", DataType::Utf8, false),
        Field::new("target", DataType::Utf8, false),
        Field::new("edge_type", DataType::Utf8, false),
    ]);
    let links_batch = RecordBatch::try_new(
        Arc::new(links_schema.clone()),
        vec![
            Arc::new(StringArray::from_iter_values(link_sources)) as ArrayRef,
            Arc::new(StringArray::from_iter_values(link_targets)) as ArrayRef,
            Arc::new(StringArray::from_iter_values(link_edge_types)) as ArrayRef,
        ],
    )?;

    let nodes_bytes = write_ipc_stream(&nodes_schema, &nodes_batch)?;
    let links_bytes = write_ipc_stream(&links_schema, &links_batch)?;

    let mut out = Vec::with_capacity(8 + nodes_bytes.len() + links_bytes.len());
    out.extend_from_slice(&(nodes_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&nodes_bytes);
    out.extend_from_slice(&(links_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&links_bytes);
    Ok(out)
}

/// Write a single `RecordBatch` as a complete Arrow IPC stream (schema header + one batch + EOS
/// marker) into an in-memory buffer.
fn write_ipc_stream(schema: &Schema, batch: &RecordBatch) -> anyhow::Result<Vec<u8>> {
    let mut writer = StreamWriter::try_new(Vec::new(), schema)?;
    writer.write(batch)?;
    let bytes = writer.into_inner()?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;

    use arrow::ipc::reader::StreamReader;
    use urban_integrity_core::graph::snapshot::bake_csr;
    use urban_integrity_schema::types::edge::{DirectsData, EdgeMeta, EdgeWeight};
    use urban_integrity_schema::types::id::EntityId;
    use urban_integrity_schema::types::node::{
        CharityData, ContractData, DisqualificationData, EntityData, EntityStatus, LocationData,
        OfficerData, OffshoreEntityData, PoliticalActorData, PropertyData, PscData, WardData,
    };

    fn meta() -> EdgeMeta {
        EdgeMeta {
            created_at: chrono::Utc::now(),
            effective_from: None,
            effective_to: None,
        }
    }

    fn entity(name: &str) -> NodeWeight {
        NodeWeight::Entity(EntityData {
            name: name.to_string(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        })
    }

    /// Builds a storage + baked snapshot + `PopulationScope` covering `scoped_ids` from a list
    /// of `(entity_id, NodeWeight)` pairs plus `(src, dst)` `Directs`-typed edges. Mirrors
    /// `population_scope.rs`'s own fixture shape.
    fn fixture(
        nodes: &[(&str, NodeWeight)],
        edges: &[(&str, &str)],
        scoped_ids: &[&str],
    ) -> (GraphStorage, CsrSnapshot, PopulationScope) {
        let storage = GraphStorage::new();
        for (id, weight) in nodes {
            storage.insert_node(EntityId(id.to_string()), weight.clone());
        }
        for (src, dst) in edges {
            storage
                .insert_edge(
                    &EntityId(src.to_string()),
                    &EntityId(dst.to_string()),
                    EdgeWeight::Directs {
                        meta: meta(),
                        data: DirectsData {
                            details: "test".to_string(),
                            officer_role: None,
                        },
                    },
                )
                .expect("fixture edge insert");
        }

        let mut entries = storage.snapshot_nodes();
        entries.sort_unstable_by_key(|(_, idx, _, _)| idx.index());
        let index_of: HashMap<String, u32> = entries
            .iter()
            .enumerate()
            .map(|(i, (id, _, _, _))| (id.0.clone(), i as u32))
            .collect();

        let snapshot = bake_csr(&storage, entries.len() as u64);

        let node_ids: HashSet<u32> = scoped_ids.iter().map(|id| index_of[*id]).collect();
        let scope = PopulationScope {
            node_ids,
            ward_seed_count: 1,
        };

        (storage, snapshot, scope)
    }

    #[test]
    fn round_trip_contains_only_scoped_nodes_and_edges_with_both_endpoints_in_scope() {
        // 5 fixture nodes, only 3 scoped: a, b, c. Edge a->b (both scoped: kept), b->c (both
        // scoped: kept), c->d (d NOT scoped: dropped), d->e (neither scoped: dropped).
        let (storage, snapshot, scope) = fixture(
            &[
                ("a", entity("A")),
                ("b", entity("B")),
                ("c", entity("C")),
                ("d", entity("D")),
                ("e", entity("E")),
            ],
            &[("a", "b"), ("b", "c"), ("c", "d"), ("d", "e")],
            &["a", "b", "c"],
        );

        let bytes = build_arrow_ipc(&storage, &snapshot, &scope).expect("build_arrow_ipc");
        let (node_ids, node_kinds, node_names, links) = decode(&bytes);

        assert_eq!(node_ids.len(), 3, "exactly the 3 scoped node rows");
        let node_id_set: HashSet<&str> = node_ids.iter().map(String::as_str).collect();
        assert_eq!(
            node_id_set,
            HashSet::from(["a", "b", "c"]),
            "only the scoped node ids"
        );
        assert_eq!(node_kinds.len(), 3);
        assert_eq!(node_names.len(), 3);

        assert_eq!(
            links.len(),
            2,
            "only a-b and b-c: both c-d and d-e are dropped (an endpoint outside scope)"
        );
        for (source, target, _edge_type) in &links {
            assert!(
                node_id_set.contains(source.as_str()),
                "referential integrity: link source {source} must be a node id"
            );
            assert!(
                node_id_set.contains(target.as_str()),
                "referential integrity: link target {target} must be a node id"
            );
        }
    }

    #[test]
    fn every_links_endpoint_is_present_among_node_ids_proven_by_parsing_the_round_tripped_bytes() {
        let (storage, snapshot, scope) = fixture(
            &[("a", entity("A")), ("b", entity("B")), ("c", entity("C"))],
            &[("a", "b"), ("b", "c")],
            &["a", "b", "c"],
        );

        let bytes = build_arrow_ipc(&storage, &snapshot, &scope).expect("build_arrow_ipc");
        let (node_ids, _kinds, _names, links) = decode(&bytes);
        let node_id_set: HashSet<&str> = node_ids.iter().map(String::as_str).collect();

        assert!(!links.is_empty(), "fixture must produce at least one link");
        for (source, target, _edge_type) in &links {
            assert!(node_id_set.contains(source.as_str()));
            assert!(node_id_set.contains(target.as_str()));
        }
    }

    #[test]
    fn node_kind_maps_correctly_for_every_node_weight_variant() {
        let ward = NodeWeight::Ward(WardData {
            name: "Sample Ward".to_string(),
        });
        let officer = NodeWeight::Officer(OfficerData {
            name: "Jane Doe".to_string(),
            officer_role: None,
            appointed_on: None,
            resigned_on: None,
            company_number: None,
        });
        let location = NodeWeight::Location(LocationData {
            name: "1 High Street".to_string(),
            postcode: None,
            address_line_1: None,
            locality: None,
        });
        let contract = NodeWeight::Contract(ContractData {
            name: "Roadworks Contract".to_string(),
            ocid: None,
            value_gbp: None,
            status: None,
            date_published: None,
            ward_code: None,
            tender_ref: None,
            application_ref: None,
            application_received_date: None,
            applicant_provenance: None,
            portal_detail_handle: None,
        });
        let political_actor = NodeWeight::PoliticalActor(PoliticalActorData {
            name: "Cllr Smith".to_string(),
            ..Default::default()
        });
        let psc = NodeWeight::Psc(PscData {
            name: "PSC Person".to_string(),
            company_number: None,
            natures_of_control: None,
        });
        let charity = NodeWeight::Charity(CharityData {
            name: "Good Causes Foundation".to_string(),
            charity_number: None,
            registered_name: None,
        });
        let property_with_title = NodeWeight::Property(PropertyData {
            title_number: Some("AB12345".to_string()),
            tenure: None,
            price_paid: None,
            company_number: None,
            country_incorporated: None,
        });
        let property_without_title = NodeWeight::Property(PropertyData {
            title_number: None,
            tenure: None,
            price_paid: None,
            company_number: None,
            country_incorporated: None,
        });
        let offshore = NodeWeight::OffshoreEntity(OffshoreEntityData {
            name: "Acme Offshore Ltd".to_string(),
            jurisdiction: None,
            icij_node_id: None,
        });
        let disq_with_case = NodeWeight::Disqualification(DisqualificationData {
            case_number: Some("CASE-99".to_string()),
            disqualified_from: None,
            disqualified_until: None,
        });
        let disq_without_case = NodeWeight::Disqualification(DisqualificationData {
            case_number: None,
            disqualified_from: None,
            disqualified_until: None,
        });

        assert_eq!(node_kind_name(&entity("A")), "Entity");
        assert_eq!(node_kind_name(&officer), "Officer");
        assert_eq!(node_kind_name(&location), "Location");
        assert_eq!(node_kind_name(&contract), "Contract");
        assert_eq!(node_kind_name(&ward), "Ward");
        assert_eq!(node_kind_name(&political_actor), "PoliticalActor");
        assert_eq!(node_kind_name(&psc), "Psc");
        assert_eq!(node_kind_name(&charity), "Charity");
        assert_eq!(node_kind_name(&property_with_title), "Property");
        assert_eq!(node_kind_name(&offshore), "OffshoreEntity");
        assert_eq!(node_kind_name(&disq_with_case), "Disqualification");

        assert_eq!(node_display_name(&property_with_title), "Property AB12345");
        assert_eq!(node_display_name(&property_without_title), "Property");
        assert_eq!(
            node_display_name(&disq_with_case),
            "Disqualification CASE-99"
        );
        assert_eq!(node_display_name(&disq_without_case), "Disqualification");
        assert_eq!(node_display_name(&ward), "Sample Ward");
    }

    /// Decode the [`build_arrow_ipc`] wire format back into
    /// `(node_ids, node_kinds, node_names, [(source, target, edge_type)])`, proving the bytes
    /// round-trip through `arrow::ipc::reader::StreamReader` rather than only proving
    /// construction succeeded.
    #[allow(clippy::type_complexity)]
    fn decode(
        bytes: &[u8],
    ) -> (
        Vec<String>,
        Vec<String>,
        Vec<String>,
        Vec<(String, String, String)>,
    ) {
        assert!(
            bytes.len() >= 4,
            "buffer must at least hold the nodes length prefix"
        );
        let nodes_len = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        let nodes_start = 4;
        let nodes_end = nodes_start + nodes_len;
        let nodes_bytes = &bytes[nodes_start..nodes_end];

        let links_len_start = nodes_end;
        let links_len = u32::from_le_bytes(
            bytes[links_len_start..links_len_start + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        let links_start = links_len_start + 4;
        let links_bytes = &bytes[links_start..links_start + links_len];

        let mut node_ids = Vec::new();
        let mut node_kinds = Vec::new();
        let mut node_names = Vec::new();
        let mut reader = StreamReader::try_new(nodes_bytes, None).expect("nodes StreamReader");
        for batch in &mut reader {
            let batch = batch.expect("nodes RecordBatch");
            let id_col = batch
                .column(0)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("id column is Utf8");
            let kind_col = batch
                .column(1)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("kind column is Utf8");
            let name_col = batch
                .column(2)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("name column is Utf8");
            for i in 0..batch.num_rows() {
                node_ids.push(id_col.value(i).to_string());
                node_kinds.push(kind_col.value(i).to_string());
                node_names.push(name_col.value(i).to_string());
            }
        }

        let mut links = Vec::new();
        let mut reader = StreamReader::try_new(links_bytes, None).expect("links StreamReader");
        for batch in &mut reader {
            let batch = batch.expect("links RecordBatch");
            let source_col = batch
                .column(0)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("source column is Utf8");
            let target_col = batch
                .column(1)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("target column is Utf8");
            let edge_type_col = batch
                .column(2)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("edge_type column is Utf8");
            for i in 0..batch.num_rows() {
                links.push((
                    source_col.value(i).to_string(),
                    target_col.value(i).to_string(),
                    edge_type_col.value(i).to_string(),
                ));
            }
        }

        (node_ids, node_kinds, node_names, links)
    }
}
