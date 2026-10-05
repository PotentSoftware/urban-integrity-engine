//! Boot-time entity label index (plan 44-06, NAV-06).
//!
//! [`LabelIndex::build`] walks `GraphStorage::snapshot_nodes()` ONCE (at boot), the FULL
//! replayed graph — deliberately never scoped to `PopulationScope` (the phase 44 research notes' Open
//! Question 1, resolved: full graph, not just the ward-reachable canonical population, so an
//! analyst's from/to/ward picker fields can reference any entity the analysis knows about, not
//! only ones inside the currently-loaded scope; T-44-06-01 accepts the resulting
//! information-disclosure trade-off explicitly).
//!
//! [`LabelIndex::search`] answers a typeahead query per design §8's ranking rule: a query
//! matching an exact `EntityId` shape (`ward:...`, `GB-COH:...`, `director:...`) short-circuits
//! directly to `GraphStorage::node_map`'s O(1) exact lookup (mirrored here via [`LabelIndex`]'s
//! own `by_id` map, built from the same `snapshot_nodes()` walk) before falling back to a
//! case-insensitive prefix-then-substring scan, capped at 20 results.

use std::collections::HashMap;

use urban_integrity_core::graph::storage::GraphStorage;
use urban_integrity_workbench_dto::EntityRef;

use super::arrow_export::{node_display_name, node_kind_name};

/// Design §8's ranking-rule cap: at most twenty results per query, regardless of how many
/// entries actually match.
const MAX_RESULTS: usize = 20;

#[derive(Debug, Clone)]
struct LabelIndexEntry {
    id: String,
    name: String,
    kind: String,
    /// Lower-cased copy of `name`, precomputed once at build time so `search` never re-folds
    /// case on every call.
    name_lower: String,
}

impl LabelIndexEntry {
    fn to_entity_ref(&self) -> EntityRef {
        EntityRef {
            id: self.id.clone(),
            name: self.name.clone(),
            kind: self.kind.clone(),
        }
    }
}

/// The boot-time entity search index. Built once via [`LabelIndex::build`], queried on every
/// `GET /api/entities/search?q=` request (wired in plan 44-08).
#[derive(Debug, Clone, Default)]
pub struct LabelIndex {
    entries: Vec<LabelIndexEntry>,
    /// Exact `EntityId` string -> index into `entries`, for the raw-id short-circuit.
    by_id: HashMap<String, usize>,
}

impl LabelIndex {
    /// Build the index from `storage`'s current node set. Reuses
    /// [`super::arrow_export::node_kind_name`]/[`super::arrow_export::node_display_name`] for
    /// `kind`/`name` derivation rather than duplicating the per-`NodeWeight`-variant logic.
    ///
    /// `storage.snapshot_nodes()` is sorted by `idx.index()` before `entries`/`by_id` are built —
    /// the SAME sort every other module in this phase applies before consuming a `snapshot_nodes()`
    /// walk (`crates/workbench/src/boot.rs`'s `population_scope` construction,
    /// `population_scope.rs`'s own fixtures, `csr_view.rs`'s fixtures) — so `entries[i]`'s position
    /// is numerically identical to CSR node index `i` (`bake_csr`'s own proof: every
    /// `petgraph::graph::NodeIndex` is assigned by a monotonically-increasing counter with no
    /// reuse, and `bake_csr` sorts by that index before baking). This is what makes
    /// [`LabelIndex::index_of`]'s returned `usize` directly usable as a CSR `u32` node index (plan
    /// 44-09's first real need for that) — without this sort, `by_id`'s values would only be
    /// arbitrary `DashMap` iteration-order positions, silently wrong for CSR resolution.
    pub fn build(storage: &GraphStorage) -> Self {
        let mut nodes = storage.snapshot_nodes();
        nodes.sort_unstable_by_key(|(_, idx, _, _)| idx.index());

        let mut entries = Vec::new();
        let mut by_id = HashMap::new();

        for (id, _idx, weight, _event_id) in nodes {
            let name = node_display_name(&weight);
            let kind = node_kind_name(&weight).to_string();
            let name_lower = name.to_lowercase();

            by_id.insert(id.0.clone(), entries.len());
            entries.push(LabelIndexEntry {
                id: id.0,
                name,
                kind,
                name_lower,
            });
        }

        Self { entries, by_id }
    }

    /// Exact-id lookup returning the entry's CSR node index (`u32`), or `None` if `id` is unknown
    /// — plan 44-09's dispatch layer uses this to resolve `from_id`/`to_id`/`ward_id` to a CSR
    /// index before checking population-scope membership. Safe to use as a `ScopedAdjacency`/
    /// `CsrSnapshot` node index because [`Self::build`] sorts by CSR index before storing
    /// `entries`/`by_id` (see that method's doc comment).
    pub fn index_of(&self, id: &str) -> Option<u32> {
        self.by_id.get(id).map(|&i| i as u32)
    }

    /// The [`EntityRef`] at CSR node index `idx`, or `None` if `idx` is out of range — plan
    /// 44-09's dispatch layer uses this to convert an algorithm's raw CSR-index output back into
    /// the wire `EntityRef` shape (path hops, ranked entities, community top entities, the picked
    /// ward).
    pub fn entity_ref_at(&self, idx: u32) -> Option<EntityRef> {
        self.entries
            .get(idx as usize)
            .map(LabelIndexEntry::to_entity_ref)
    }

    /// Resolve `query` to at most 20 [`EntityRef`]s.
    ///
    /// 1. Empty `query` returns an empty `Vec` — never the whole index.
    /// 2. Exact-id short-circuit: if `query` matches an `EntityId` string exactly
    ///    (case-sensitive — ids are not user-typed casually the way names are), return that
    ///    single entry immediately as a one-element `Vec`, regardless of whether any entry's
    ///    name would otherwise prefix/substring-match `query`.
    /// 3. Otherwise, case-fold `query` and rank: entries whose lower-cased name STARTS WITH the
    ///    folded query first, then entries whose lower-cased name CONTAINS it (excluding the
    ///    prefix-match set, so no entry appears twice), each group in stable original order,
    ///    capped at 20 total.
    pub fn search(&self, query: &str) -> Vec<EntityRef> {
        if query.is_empty() {
            return Vec::new();
        }

        if let Some(&idx) = self.by_id.get(query) {
            return vec![self.entries[idx].to_entity_ref()];
        }

        // Bare-code lookup. Node ids are scheme-prefixed (`ward:X05999001`, `GB-COH:PL000099`),
        // but the identifiers an analyst actually has in hand — from the evidence report, from
        // `config.toml`, from Companies House — are the BARE code: `X05999001`, `PL000099`.
        // Matching only the full id and then falling back to NAME matching left those queries
        // returning nothing at all, so the entity picker answered "No entity matches" for a node
        // that plainly exists, and the endpoint could not be picked (operator QA 2026-09-12).
        // Case-insensitive because ward codes are conventionally written uppercase and company
        // numbers are not consistently either way.
        let query_folded = query.to_lowercase();
        let bare_matches: Vec<EntityRef> = self
            .entries
            .iter()
            .filter(|entry| {
                entry
                    .id
                    .split_once(':')
                    .is_some_and(|(_scheme, code)| code.to_lowercase() == query_folded)
            })
            .map(LabelIndexEntry::to_entity_ref)
            .collect();
        if !bare_matches.is_empty() {
            return bare_matches;
        }

        let folded = query_folded;
        let mut prefix_matches: Vec<&LabelIndexEntry> = Vec::new();
        let mut substring_matches: Vec<&LabelIndexEntry> = Vec::new();
        for entry in &self.entries {
            if entry.name_lower.starts_with(&folded) {
                prefix_matches.push(entry);
            } else if entry.name_lower.contains(&folded) {
                substring_matches.push(entry);
            }
        }

        prefix_matches
            .into_iter()
            .chain(substring_matches)
            .take(MAX_RESULTS)
            .map(LabelIndexEntry::to_entity_ref)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use urban_integrity_schema::types::id::EntityId;
    use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight, WardData};

    fn entity(name: &str) -> NodeWeight {
        NodeWeight::Entity(EntityData {
            name: name.to_string(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        })
    }

    fn ward(name: &str) -> NodeWeight {
        NodeWeight::Ward(WardData {
            name: name.to_string(),
        })
    }

    fn storage_with(nodes: &[(&str, NodeWeight)]) -> GraphStorage {
        let storage = GraphStorage::new();
        for (id, weight) in nodes {
            storage.insert_node(EntityId(id.to_string()), weight.clone());
        }
        storage
    }

    /// Operator QA 2026-09-12: typing the bare ward code `X05999001` into the entity picker
    /// answered "No entity matches" for a node that plainly exists, because real node ids are
    /// scheme-prefixed (`ward:X05999001`) and search matched only the FULL id or the NAME. The
    /// bare code is the identifier an analyst actually holds — it is what the evidence report and
    /// `config.toml` both carry. PROVE THIS CAN FAIL: drop the bare-code branch from `search` and
    /// both assertions below return empty.
    #[test]
    fn search_matches_a_bare_code_against_a_scheme_prefixed_id() {
        let storage = storage_with(&[
            ("ward:X05999001", ward("Planted Ward")),
            ("GB-COH:PL000099", entity("Planted Council")),
        ]);
        let index = LabelIndex::build(&storage);

        let ward_hit = index.search("X05999001");
        assert_eq!(
            ward_hit.len(),
            1,
            "bare ward code must resolve, got {ward_hit:?}"
        );
        assert_eq!(ward_hit[0].id, "ward:X05999001");

        let company_hit = index.search("PL000099");
        assert_eq!(
            company_hit.len(),
            1,
            "bare company number must resolve, got {company_hit:?}"
        );
        assert_eq!(company_hit[0].id, "GB-COH:PL000099");

        // Case-insensitive, and the full id still wins the exact-match short circuit.
        assert_eq!(index.search("x05999001").len(), 1);
        assert_eq!(index.search("ward:X05999001")[0].id, "ward:X05999001");
    }

    #[test]
    fn search_with_a_raw_ward_id_shape_short_circuits_via_exact_id_even_with_no_fuzzy_match() {
        // "X05001499" is a bare ward-code-shaped id; no fixture entry's NAME contains that
        // substring, so a fuzzy-only search would return empty — the short-circuit must fire on
        // the id itself, not on any name match.
        let storage = storage_with(&[
            ("X05001499", ward("Sample Ward")),
            ("GB-COH:other", entity("Unrelated Company")),
        ]);
        let index = LabelIndex::build(&storage);

        let results = index.search("X05001499");

        assert_eq!(results.len(), 1, "exact-id short-circuit returns one entry");
        assert_eq!(results[0].id, "X05001499");
        assert_eq!(
            results[0].name, "Sample Ward",
            "the short-circuited entry still carries its real display name"
        );
    }

    #[test]
    fn search_ranks_prefix_matches_before_substring_matches_and_caps_at_twenty() {
        let mut nodes: Vec<(String, NodeWeight)> = vec![
            ("p1".to_string(), entity("Jane Smith")), // prefix match
            ("p2".to_string(), entity("Jane Doe")),   // prefix match
            ("s1".to_string(), entity("Mary Jane")),  // substring match only
        ];
        // Push the prefix-match count past the 20-result cap so the cap itself is exercised
        // against a real overflow, not just an under-full fixture.
        for i in 0..25 {
            nodes.push((format!("extra-{i}"), entity(&format!("Jane Extra {i}"))));
        }

        let node_refs: Vec<(&str, NodeWeight)> = nodes
            .iter()
            .map(|(id, w)| (id.as_str(), w.clone()))
            .collect();
        let storage = storage_with(&node_refs);
        let index = LabelIndex::build(&storage);

        let results = index.search("jane");

        assert_eq!(results.len(), 20, "results are capped at 20 total");
        // Every prefix match ("Jane ...") outranks the lone substring match ("Mary Jane") --
        // with 27 prefix matches and only 1 substring match, all 20 returned results must be
        // prefix matches; the substring match never surfaces within the cap.
        assert!(
            results.iter().all(|r| r.name.starts_with("Jane")),
            "prefix matches must be ranked ahead of substring matches: {:?}",
            results.iter().map(|r| &r.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn search_prefix_then_substring_ordering_when_under_the_cap() {
        let storage = storage_with(&[
            ("s1", entity("Mary Jane")),  // substring match only
            ("p1", entity("Jane Smith")), // prefix match
        ]);
        let index = LabelIndex::build(&storage);

        let results = index.search("jane");

        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].name, "Jane Smith",
            "the prefix match must rank ahead of the substring-only match"
        );
        assert_eq!(results[1].name, "Mary Jane");
    }

    #[test]
    fn search_with_no_match_returns_empty_vec_not_an_error() {
        let storage = storage_with(&[("e1", entity("Alpha Ltd"))]);
        let index = LabelIndex::build(&storage);

        let results = index.search("nomatch");

        assert!(results.is_empty());
    }

    #[test]
    fn search_with_empty_query_returns_empty_vec_never_the_whole_index() {
        let storage = storage_with(&[("e1", entity("Alpha Ltd")), ("e2", entity("Beta Ltd"))]);
        let index = LabelIndex::build(&storage);

        let results = index.search("");

        assert!(results.is_empty());
    }

    #[test]
    fn a_ward_search_result_always_carries_the_wards_name_never_the_bare_code() {
        let storage = storage_with(&[("ward:X05001499", ward("Sample Ward"))]);
        let index = LabelIndex::build(&storage);

        let results = index.search("Sample Ward");

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].name, "Sample Ward",
            "EntityRef.name must be the ward's name"
        );
        assert_eq!(
            results[0].id, "ward:X05001499",
            "the ward code lives only in EntityRef.id"
        );
        assert_eq!(results[0].kind, "Ward");
    }

    /// [`LabelIndex::index_of`]'s returned `u32` must be numerically identical to the CSR node
    /// index `bake_csr` assigns the same entity — not just an arbitrary internal `entries`
    /// position. Built with edges present (so node insertion order is non-trivial), proving the
    /// build-time sort by `idx.index()` (this module's own fix) is load-bearing: without it,
    /// `by_id`'s values would be raw `DashMap` iteration-order positions, silently wrong as CSR
    /// indices.
    #[test]
    fn index_of_returns_the_same_index_bake_csr_assigns() {
        use urban_integrity_core::graph::snapshot::bake_csr;
        use urban_integrity_schema::types::edge::{DirectsData, EdgeMeta, EdgeWeight};

        let storage = storage_with(&[
            ("a", entity("Alpha Ltd")),
            ("b", entity("Beta Ltd")),
            ("c", entity("Gamma Ltd")),
        ]);
        storage
            .insert_edge(
                &EntityId("a".to_string()),
                &EntityId("b".to_string()),
                EdgeWeight::Directs {
                    meta: EdgeMeta {
                        created_at: chrono::Utc::now(),
                        effective_from: None,
                        effective_to: None,
                    },
                    data: DirectsData {
                        details: "d".into(),
                        officer_role: None,
                    },
                },
            )
            .expect("fixture edge insert");

        let index = LabelIndex::build(&storage);
        let snapshot = bake_csr(&storage, 4);

        for (id, expected_idx, _weight, _event_id) in storage.snapshot_nodes() {
            let resolved = index
                .index_of(&id.0)
                .unwrap_or_else(|| panic!("index_of must resolve a known id: {}", id.0));
            assert_eq!(
                resolved,
                expected_idx.index() as u32,
                "index_of({}) must equal bake_csr's own CSR index, not an arbitrary entries position",
                id.0
            );
            assert!(
                (resolved as usize) < snapshot.graph.node_count(),
                "resolved index must be within the baked CSR's node count"
            );
        }
    }

    /// [`LabelIndex::entity_ref_at`] is the exact inverse of [`LabelIndex::index_of`]: round-trips
    /// id -> index -> `EntityRef` back to the same id, and returns `None` out of range.
    #[test]
    fn entity_ref_at_round_trips_with_index_of() {
        let storage = storage_with(&[("a", entity("Alpha Ltd")), ("b", entity("Beta Ltd"))]);
        let index = LabelIndex::build(&storage);

        let idx = index.index_of("a").expect("a must resolve");
        let entity_ref = index
            .entity_ref_at(idx)
            .expect("entity_ref_at must resolve a valid index");
        assert_eq!(entity_ref.id, "a");
        assert_eq!(entity_ref.name, "Alpha Ltd");

        assert!(
            index.entity_ref_at(999).is_none(),
            "an out-of-range index must return None, never panic"
        );
    }
}
