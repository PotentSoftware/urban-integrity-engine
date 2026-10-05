use anyhow::{bail, Result};
use clap::Args;
use petgraph::graph::{DiGraph, NodeIndex};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use crate::db::connect_db;
use urban_integrity_core::config::AppConfig;

/// Source the GraphML export reads from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum GraphSource {
    /// Projected native tables (entity, officer, location, …). Requires
    /// `urban-integrity project` to have been run first.
    Native,
    /// Reconstruct the graph directly from the `graph_events` WAL
    /// (NodeAdded + EdgeAdded events). No projection step required.
    Events,
}

#[derive(Debug, Args)]
pub struct ExportGraphmlArgs {
    /// Output file (default: graph.graphml)
    #[arg(short, long, default_value = "graph.graphml")]
    pub output: PathBuf,

    /// Where to read the graph from: the `events` (graph_events) write-ahead log
    /// or the `native` projected tables.
    ///
    /// Defaults to `events`. `native` goes through the SurrealDB SDK, whose flatbuffers
    /// codec fails to decode typed RecordId fields on large result sets — measured
    /// 2026-08-25, a 358k-node analysis errors with "Failed to deserialize flatbuffers
    /// payload". The events path reads raw HTTP `/sql` and does not have that ceiling
    /// (see `load_from_events`). `native` is kept for small graphs and for exporting
    /// exactly what `project` produced, but it is no longer the default: the default
    /// should be the path that works at real scale.
    #[arg(long, value_enum, default_value = "events")]
    pub source: GraphSource,

    /// Keep only nodes of this type (repeatable, case-insensitive).
    ///
    /// e.g. `--node-type Officer --node-type Entity`. Edges whose endpoints are
    /// filtered out are dropped with them. An unknown type is a hard error listing
    /// what is actually present — never a silent empty export.
    #[arg(long = "node-type", value_name = "TYPE")]
    pub node_type: Vec<String>,

    /// Keep only edges of this type (repeatable, case-insensitive).
    ///
    /// e.g. `--edge-type Directs`. An unknown type is a hard error listing what is
    /// actually present.
    #[arg(long = "edge-type", value_name = "TYPE")]
    pub edge_type: Vec<String>,

    /// After filtering, drop nodes left with no edges.
    ///
    /// Without this a type-filtered export is mostly unconnected dots in Gephi.
    #[arg(long)]
    pub drop_isolated: bool,

    /// Restrict the export to the weakly-connected component reachable from a set of
    /// ward seed nodes (ORC-01 / ORC-04).
    ///
    /// Three states:
    /// - **absent** → `None` — no reachability filtering, the pipeline behaves exactly as it
    ///   does today. Neither `scripts/export_graphml.sh` nor `scripts/export_parquet.sh` passes
    ///   this flag, so both are unaffected.
    /// - **present with no codes** (`--reachable-from-wards`) → falls back to every `ward_code`
    ///   / `lpa_code` configured under `[[ingest.planning_portals]]` in `config.toml`.
    /// - **present with codes** (`--reachable-from-wards X05999001,X05999002`) → those codes
    ///   override config entirely.
    ///
    /// Runs strictly BEFORE `--node-type`/`--edge-type` filtering — reachability defines the
    /// population of interest, type filters narrow within it. Reachability that resolves to
    /// zero seeds is a hard error naming every code searched and every node type present, not
    /// a silent empty export (this is the exact bug in the retired Python heredoc's
    /// `except Exception: target_codes = set()` path).
    #[arg(
        long = "reachable-from-wards",
        value_name = "CODE",
        num_args = 0..,
        value_delimiter = ','
    )]
    pub reachable_from_wards: Option<Vec<String>>,
}

#[derive(Clone, Debug)]
struct NodeAttrs {
    entity_id: String,
    label: String,
    node_type: String,
    extra: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
struct EdgeAttrs {
    edge_type: String,
    extra: Vec<(String, String)>,
}

const NODE_TABLES: &[&str] = &[
    "entity",
    "officer",
    "location",
    "contract",
    "ward",
    "political_actor",
];

const EDGE_TABLES: &[&str] = &[
    "directs",
    "located_at",
    "awarded_to",
    "issued_by",
    "represents",
];

// Fields excluded from the "extra" attributes bag (structural / already promoted)
const NODE_SKIP: &[&str] = &["id", "entity_id", "name"];
const EDGE_SKIP: &[&str] = &["id", "src", "dst", "in", "out", "meta", "data"];

pub async fn run(args: ExportGraphmlArgs) -> Result<()> {
    let config = AppConfig::load()?;

    let graph = match args.source {
        GraphSource::Native => {
            let db = Arc::new(connect_db(&config).await?);
            load_from_native(&db).await?
        }
        GraphSource::Events => load_from_events(&config, &config.surreal.database).await?,
    };

    // Reachability MUST run strictly before --node-type/--edge-type filtering. Reachability is
    // carried by the LocatedAt (133,864 live) and Represents (127,785 live) edges, which type
    // filtering removes — running type filtering first silently collapses the reachable set with
    // no error (phase 37 research Risk 1). Do not reorder this "for readability."
    let graph = match effective_ward_codes(&args.reachable_from_wards, &config) {
        Some(codes) => {
            let seeds = resolve_ward_seeds(&graph, &codes)?;
            filter_reachable(graph, &seeds)
        }
        None => graph,
    };

    let graph = filter_graph(graph, &args.node_type, &args.edge_type, args.drop_isolated)?;

    // ── GraphML ───────────────────────────────────────────────────────────
    let xml = render_graphml(&graph)?;
    fs::write(&args.output, xml)?;
    println!("Written → {}", args.output.display());
    Ok(())
}

/// The canonical graph population count and its generation timestamp (D-19,
/// `docs/CANONICAL-POPULATION.md`), for the workbench status page.
///
/// This is a **narrow, intra-crate reuse** of the exact three calls `run()`'s default
/// (`events` source, no explicit `--node-type`/`--edge-type` filter) reachable-from-wards case
/// makes — `load_from_events`, `effective_ward_codes`, `resolve_ward_seeds` + `filter_reachable`
/// — reproducing the bare `--reachable-from-wards` flag state (`Some(vec![])`, i.e. "use
/// whatever wards are configured") so the result matches D-19's own definition of the canonical
/// population. It is **NOT** the cross-crate shared-pipeline extraction `docs/CANONICAL-
/// POPULATION.md` names as the Phase 46+ refactor target (a function `crates/workbench`, or a
/// future shared crate, could call directly without going through `crates/cli`'s internals) —
/// that extraction has not happened here. A future reader must not assume it has just because
/// this function exists.
///
/// # Which ward codes seed the reachability walk
///
/// `analysis_ward_codes` is the TARGET analysis's own ward set — its `_catalog` registry row's
/// `ward_codes`, threaded in by the `workbench` arm — and takes precedence whenever it is
/// `Some` and non-empty. Only when the bound database has no registry row (the `main` /
/// `config.surreal.database` path, or a fresh test namespace) does this fall back to
/// `config.toml`'s `[[ingest.planning_portals]]` wards via `effective_ward_codes`. Seeding
/// from config unconditionally (the original shape) meant `workbench --analysis <other>` — the
/// exact command the analysis picker discloses — walked the wrong analysis's wards, and refused
/// to boot whenever none of them existed in the target graph.
///
/// If neither source yields any ward code, the population is the full events-loaded graph — a
/// legitimate answer, not an error. `run()`'s own behaviour above is untouched by this addition.
///
/// # Zero resolvable seeds is "population unavailable", not a boot failure
///
/// `resolve_ward_seeds`'s zero-seed `bail!` is the right answer for `run()` (an export that
/// silently filtered everything away is the Python-heredoc defect it fixes). The workbench is
/// not exporting anything: it is reporting a number on a status page, and "this number could
/// not be computed, here is why" is a truthful, renderable state — refusing to serve the whole
/// UI over it is not. So this function returns [`CanonicalPopulation::Unavailable`] with the
/// reason (naming the codes tried and the database) instead of propagating that error.
///
/// # A genuinely empty `graph_events` is population 0, not an error
///
/// `load_from_events` itself hard-errors (the typed [`EmptyGraphEvents`]) on a table with zero
/// rows — correct for `run()`'s CLI UX, where an operator exporting GraphML against an empty
/// analysis almost certainly meant a different `--analysis`/database, and a loud error beats a
/// silently-empty `.graphml` file. `workbench` is a different caller: a freshly-provisioned
/// analysis with no ingestion yet is a legitimate state to boot into (the exact same "empty is
/// fine, a genuine failure is not" distinction `build_engine`'s own replay guard already draws
/// two lines away, in `crates/workbench/src/boot.rs`). Discovered via this plan's Task 3
/// DB-gated tests, which run against a freshly-created, genuinely empty `TestNs`/`NsGuard`
/// namespace per the test-namespace hygiene rule (docs/TESTING.md) — that is not a bug in the test fixture, it is
/// the realistic "brand-new analysis" case `workbench` must also handle. The emptiness match is
/// on the error's TYPE (`downcast_ref::<EmptyGraphEvents>`), never its wording. This match arm
/// is scoped to this function only; `load_from_events`/`run()` are untouched.
pub async fn canonical_population_summary(
    config: &AppConfig,
    db_name: &str,
    analysis_ward_codes: Option<&[String]>,
) -> Result<CanonicalPopulation> {
    let graph = match load_from_events(config, db_name).await {
        Ok(g) => g,
        Err(e) if e.downcast_ref::<EmptyGraphEvents>().is_some() => {
            return Ok(CanonicalPopulation::Available {
                count: 0,
                generated_at: chrono::Utc::now(),
                ward_seed_count: 0,
            });
        }
        Err(e) => return Err(e),
    };
    let codes = match analysis_ward_codes {
        Some(codes) if !codes.is_empty() => Some(codes.to_vec()),
        _ => effective_ward_codes(&Some(vec![]), config),
    };
    Ok(population_from_graph(graph, codes.as_deref(), db_name))
}

/// The canonical population as the workbench reports it — either a count, or a typed,
/// human-readable reason it could not be computed. Both carry the generation timestamp
/// (D-19/WBX-06's "never one without the other" rule) and `ward_seed_count` — the number of
/// ward seeds the reachable-from-wards walk actually started from (0 when no ward code was
/// configured at all, or when every configured code resolved to zero seeds) — added in Phase 43
/// plan 05 so `crates/workbench`'s `PopulationMeta.ward_seed_count` (D-20) travels the same path
/// `count` does rather than being re-derived independently in `crates/workbench`.
#[derive(Debug, Clone, PartialEq)]
pub enum CanonicalPopulation {
    Available {
        count: u64,
        generated_at: chrono::DateTime<chrono::Utc>,
        ward_seed_count: u64,
    },
    Unavailable {
        reason: String,
        generated_at: chrono::DateTime<chrono::Utc>,
        ward_seed_count: u64,
    },
}

/// The pure, DB-free half of [`canonical_population_summary`]: given the loaded graph and the
/// effective seed codes (`None` = no wards configured anywhere → the whole graph), produce the
/// population or the typed unavailable state. Split out so the three outcomes — seeds resolve,
/// zero seeds resolve, no codes at all — are unit-tested without a SurrealDB connection.
fn population_from_graph(
    graph: DiGraph<NodeAttrs, EdgeAttrs>,
    codes: Option<&[String]>,
    db_name: &str,
) -> CanonicalPopulation {
    let generated_at = chrono::Utc::now();
    let Some(codes) = codes else {
        return CanonicalPopulation::Available {
            count: graph.node_count() as u64,
            generated_at,
            ward_seed_count: 0,
        };
    };
    let seeds = find_ward_seeds(&graph, codes);
    if seeds.is_empty() {
        let mut requested: Vec<&str> = codes.iter().map(|s| s.as_str()).collect();
        requested.sort_unstable();
        requested.dedup();
        return CanonicalPopulation::Unavailable {
            reason: format!(
                "none of the {} ward code(s) seeding the canonical population ({}) resolve to \
                 a Ward node in database '{db_name}' — the reachable-from-wards population \
                 cannot be computed against this graph",
                requested.len(),
                requested.join(", ")
            ),
            generated_at,
            ward_seed_count: 0,
        };
    }
    let ward_seed_count = seeds.len() as u64;
    CanonicalPopulation::Available {
        count: filter_reachable(graph, &seeds).node_count() as u64,
        generated_at,
        ward_seed_count,
    }
}

// ── type filtering ──────────────────────────────────────────────────────────

/// Case-insensitively resolve each requested type against the types actually present.
///
/// Returns the matched set in the graph's own casing. **An unrecognised request is a hard
/// error, never a silent miss.** A typo like `--node-type Offcier` would otherwise export an
/// empty graph that looks exactly like "this analysis has no officers" — the zero-output
/// ambiguity this codebase has been bitten by repeatedly (the ICIJ adapter logged 814k rows
/// written against an empty table for its entire life).
fn resolve_types(
    requested: &[String],
    present: &HashSet<String>,
    what: &str,
) -> Result<Option<HashSet<String>>> {
    if requested.is_empty() {
        return Ok(None);
    }
    let lowered: HashMap<String, String> = present
        .iter()
        .map(|t| (t.to_lowercase(), t.clone()))
        .collect();

    let mut keep = HashSet::new();
    let mut unknown = Vec::new();
    for req in requested {
        match lowered.get(&req.to_lowercase()) {
            Some(actual) => {
                keep.insert(actual.clone());
            }
            None => unknown.push(req.clone()),
        }
    }
    if !unknown.is_empty() {
        let mut available: Vec<&String> = present.iter().collect();
        available.sort();
        bail!(
            "unknown {what}(s): {}\n  present in this graph: {}\n\
             Nothing was exported — a filter that matches nothing is a mistake, not an empty result.",
            unknown.join(", "),
            available
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(Some(keep))
}

// ── ward reachability (ORC-01 / ORC-04) ─────────────────────────────────────

/// Resolve the effective set of ward/LPA codes for `--reachable-from-wards`, implementing the
/// three-state precedence documented on `ExportGraphmlArgs::reachable_from_wards`.
///
/// - `flag == None` → `None` (no reachability filtering at all).
/// - `flag == Some(codes)` where `codes` is non-empty → `Some(codes)` unchanged — explicit
///   codes override config, they are never merged with it.
/// - `flag == Some(vec![])` (bare flag, no codes supplied) → falls back to every `ward_code`
///   and every present `lpa_code` across `config.ingest.planning_portals`, deduplicated and
///   sorted. Sorted purely so the zero-seed error message below is deterministic and diffable —
///   ordering has no other significance here.
fn effective_ward_codes(flag: &Option<Vec<String>>, config: &AppConfig) -> Option<Vec<String>> {
    let codes = flag.as_ref()?;
    if !codes.is_empty() {
        return Some(codes.clone());
    }
    let mut set: HashSet<String> = HashSet::new();
    for portal in &config.ingest.planning_portals {
        set.insert(portal.ward_code.clone());
        if let Some(lpa) = &portal.lpa_code {
            set.insert(lpa.clone());
        }
    }
    let mut sorted: Vec<String> = set.into_iter().collect();
    sorted.sort();
    Some(sorted)
}

/// Resolve ward seed codes to graph node indices for the reachability BFS.
///
/// A node is a seed iff it is a `Ward` node (`node_type == "Ward"`) whose `entity_id` equals
/// `ward:{code}` for one of the requested codes — exact equality on the full id, per the
/// `ward:{code}` format `crates/ingestor/src/backfill/ward.rs` writes, **not** a substring test.
/// The retired Python heredoc used `any(c in n['id'] for c in target_codes)`, a substring test
/// against arbitrary node ids; it happened to return the same 17 seeds on the live corpus
/// (ward codes are unique enough strings not to collide) but was correct by luck, not by
/// construction.
///
/// An LPA code (e.g. `X06999001`) legitimately resolves nothing on its own — there is no LPA
/// node type — and that is fine when it accompanies ward codes that DO resolve. It is only an
/// error when the entire effective set resolves to zero seeds: that is the fix for the Python's
/// `except Exception: target_codes = set()` path, which used to filter everything away and
/// still exit 0 with an empty artifact.
fn resolve_ward_seeds(
    graph: &DiGraph<NodeAttrs, EdgeAttrs>,
    codes: &[String],
) -> Result<HashSet<NodeIndex>> {
    let seeds = find_ward_seeds(graph, codes);

    if seeds.is_empty() {
        let mut requested: Vec<&String> = codes.iter().collect();
        requested.sort();
        let mut present: Vec<String> = graph
            .node_weights()
            .map(|n| n.node_type.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        present.sort();
        bail!(
            "--reachable-from-wards resolved zero seeds for code(s): {}\n  \
             present in this graph: {}\n\
             This is the fix for the retired Python heredoc's \
             `except Exception: target_codes = set()` path, which used to filter the whole \
             graph away and exit 0 with an empty artifact. An LPA code alone (no LPA node type \
             exists) legitimately resolves nothing — but it must not be the entire effective \
             set.",
            requested
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            present.join(", ")
        );
    }
    Ok(seeds)
}

/// The seed matcher itself, with no zero-seed policy attached: every `Ward` node whose
/// `entity_id` is exactly `ward:{code}` for one of `codes`. `resolve_ward_seeds` layers
/// `run()`'s hard-error policy on top; `population_from_graph` layers the workbench's
/// "unavailable, not fatal" policy on top. Both share this one matcher so they cannot drift.
fn find_ward_seeds(graph: &DiGraph<NodeAttrs, EdgeAttrs>, codes: &[String]) -> HashSet<NodeIndex> {
    let wanted: HashSet<String> = codes.iter().map(|c| format!("ward:{c}")).collect();
    graph
        .node_indices()
        .filter(|&idx| {
            let n = &graph[idx];
            n.node_type == "Ward" && wanted.contains(&n.entity_id)
        })
        .collect()
}

/// Restrict `graph` to the weakly-connected component reachable from `seeds`.
///
/// Hand-rolled BFS over `neighbors_undirected` — **deliberately undirected, not a bug.** It
/// matches `filter_graph`'s own `neighbors_undirected` use in the `drop_isolated` branch above,
/// and preserves the retired Python heredoc's `adj[u].append(v); adj[v].append(u)` behaviour.
/// With 133,864 `LocatedAt` edges live, a shared postcode bridges almost anything — that is
/// *why* 113k of 358k nodes survive today. Do NOT switch this to `petgraph::visit::Bfs`, which
/// follows outgoing edges only: it would silently shrink every export by dropping any node only
/// reachable via the inbound direction of a directed edge.
fn filter_reachable(
    graph: DiGraph<NodeAttrs, EdgeAttrs>,
    seeds: &HashSet<NodeIndex>,
) -> DiGraph<NodeAttrs, EdgeAttrs> {
    let mut visited: HashSet<NodeIndex> = seeds.clone();
    let mut queue: VecDeque<NodeIndex> = seeds.iter().copied().collect();
    while let Some(idx) = queue.pop_front() {
        for next in graph.neighbors_undirected(idx) {
            if visited.insert(next) {
                queue.push_back(next);
            }
        }
    }

    let filtered = graph.filter_map(
        |idx, n| visited.contains(&idx).then(|| n.clone()),
        |_, e| Some(e.clone()),
    );

    println!(
        "Reachable from {} ward seed(s) → {} nodes, {} edges",
        seeds.len(),
        filtered.node_count(),
        filtered.edge_count()
    );
    filtered
}

/// Apply `--node-type` / `--edge-type` / `--drop-isolated` to a loaded graph.
///
/// Order is deliberate: nodes first, then edges. `petgraph::filter_map` drops any edge whose
/// endpoints did not survive, so an edge kept by `--edge-type` still disappears if its endpoints
/// were filtered away — which is the semantics you want, and why the two flags compose.
fn filter_graph(
    graph: DiGraph<NodeAttrs, EdgeAttrs>,
    node_types: &[String],
    edge_types: &[String],
    drop_isolated: bool,
) -> Result<DiGraph<NodeAttrs, EdgeAttrs>> {
    if node_types.is_empty() && edge_types.is_empty() && !drop_isolated {
        return Ok(graph);
    }

    let present_nodes: HashSet<String> =
        graph.node_weights().map(|n| n.node_type.clone()).collect();
    let present_edges: HashSet<String> =
        graph.edge_weights().map(|e| e.edge_type.clone()).collect();

    let keep_nodes = resolve_types(node_types, &present_nodes, "node type")?;
    let keep_edges = resolve_types(edge_types, &present_edges, "edge type")?;

    let filtered = graph.filter_map(
        |_, n| match &keep_nodes {
            Some(keep) if !keep.contains(&n.node_type) => None,
            _ => Some(n.clone()),
        },
        |_, e| match &keep_edges {
            Some(keep) if !keep.contains(&e.edge_type) => None,
            _ => Some(e.clone()),
        },
    );

    let filtered = if drop_isolated {
        filtered.filter_map(
            |idx, n| (filtered.neighbors_undirected(idx).next().is_some()).then(|| n.clone()),
            |_, e| Some(e.clone()),
        )
    } else {
        filtered
    };

    println!(
        "Filtered → {} nodes, {} edges",
        filtered.node_count(),
        filtered.edge_count()
    );
    Ok(filtered)
}

// ── native-table source (projected entity/officer/… tables) ─────────────────

async fn load_from_native(
    db: &Arc<surrealdb::Surreal<surrealdb::engine::any::Any>>,
) -> Result<DiGraph<NodeAttrs, EdgeAttrs>> {
    let mut graph: DiGraph<NodeAttrs, EdgeAttrs> = DiGraph::new();
    let mut idx_map: HashMap<String, NodeIndex> = HashMap::new();

    // ── nodes ─────────────────────────────────────────────────────────────
    for table in NODE_TABLES {
        let mut res = db.query(format!("SELECT * FROM {table};")).await?;
        let rows: Vec<Value> = res.take(0)?;
        for row in rows {
            let Some(obj) = row.as_object() else { continue };
            let Some(entity_id) = obj.get("entity_id").and_then(|v| v.as_str()) else {
                continue;
            };
            let label = obj
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(entity_id)
                .to_string();
            let extra = scalar_fields(obj, NODE_SKIP);
            let idx = graph.add_node(NodeAttrs {
                entity_id: entity_id.to_string(),
                label,
                node_type: table.to_string(),
                extra,
            });
            idx_map.insert(entity_id.to_string(), idx);
        }
    }

    if idx_map.is_empty() {
        bail!("No nodes found in native tables — run `urban-integrity project` first, or pass --source events.");
    }
    println!("Loaded {} nodes", idx_map.len());

    // ── edges ─────────────────────────────────────────────────────────────
    // Query src/dst entity_ids via graph traversal (in.entity_id / out.entity_id).
    let mut edge_count = 0usize;
    let mut edge_skip = 0usize;
    for table in EDGE_TABLES {
        let mut res = db
            .query(format!(
                "SELECT in.entity_id AS src, out.entity_id AS dst, data FROM {table};"
            ))
            .await?;
        let rows: Vec<Value> = res.take(0)?;
        for row in rows {
            let Some(obj) = row.as_object() else { continue };
            let Some(src) = obj.get("src").and_then(|v| v.as_str()) else {
                edge_skip += 1;
                continue;
            };
            let Some(dst) = obj.get("dst").and_then(|v| v.as_str()) else {
                edge_skip += 1;
                continue;
            };
            let (Some(&si), Some(&di)) = (idx_map.get(src), idx_map.get(dst)) else {
                edge_skip += 1;
                continue;
            };
            // Flatten data sub-object into extra attrs
            let mut extra = scalar_fields(obj, EDGE_SKIP);
            if let Some(Value::Object(data)) = obj.get("data") {
                for (k, v) in data {
                    if let Some(s) = scalar_str(v) {
                        extra.push((format!("data_{k}"), s));
                    }
                }
            }
            graph.add_edge(
                si,
                di,
                EdgeAttrs {
                    edge_type: table.to_string(),
                    extra,
                },
            );
            edge_count += 1;
        }
    }
    println!(
        "Loaded {} edges ({} skipped — unresolved endpoints)",
        edge_count, edge_skip
    );

    Ok(graph)
}

// ── graph_events source (NodeAdded + EdgeAdded WAL) ─────────────────────────

/// Reconstruct the graph directly from the `graph_events` write-ahead log.
///
/// The typed "`graph_events` holds no `NodeAdded` rows at all" signal `load_from_events`
/// returns (as the root cause of its `anyhow::Error`) on an empty table.
///
/// A typed error rather than a prose `bail!` so callers that treat emptiness as a legitimate
/// state — `canonical_population_summary`, for a freshly-provisioned analysis — match on
/// `err.downcast_ref::<EmptyGraphEvents>()` instead of on the wording of this message. The old
/// `e.to_string().contains("No NodeAdded events found ...")` check meant rewording this
/// message would silently turn every empty-analysis workbench boot into a hard failure, and
/// any unrelated error whose chain happened to quote it would be swallowed as "population 0".
/// The message itself is unchanged for `run()`'s CLI UX, where an empty export target almost
/// certainly means a wrong `--analysis`/database and a loud error beats a silently-empty file.
#[derive(Debug)]
pub(crate) struct EmptyGraphEvents;

impl std::fmt::Display for EmptyGraphEvents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("No NodeAdded events found in graph_events — is the engine populated?")
    }
}

impl std::error::Error for EmptyGraphEvents {}

/// Mirrors the projector (`crates/ingestor/src/projector.rs`): NodeAdded events are
/// deduplicated keeping the highest-timestamp row per entity_id (correction events
/// win), then EdgeAdded events are resolved against the resulting node set.
///
/// Reads via the raw HTTP `/sql` endpoint rather than the SDK: the SurrealDB 3.0.x
/// server / 3.1.x SDK flatbuffers codec can fail to decode typed RecordId fields on
/// large result sets. Over plain HTTP JSON every id (including coerced `director:`
/// keys) arrives as a string, so reconstruction is reliable.
///
/// `db_name` is the database to read — the caller's RESOLVED analysis database (what
/// `crate::db::resolve_analysis_db` returned), not `config.surreal.database`, which under
/// `--analysis <id>` names the default database rather than the selected one.
async fn load_from_events(
    config: &AppConfig,
    db_name: &str,
) -> Result<DiGraph<NodeAttrs, EdgeAttrs>> {
    let http_base = config
        .surreal
        .ws_url
        .replace("wss://", "https://")
        .replace("ws://", "http://");
    let ns = config.surreal.namespace.clone();
    let db = db_name.to_string();
    let auth = match (
        config.surreal.username.as_deref(),
        config.surreal.password.as_deref(),
    ) {
        (Some(u), Some(p)) => Some((u.to_owned(), p.to_owned())),
        _ => None,
    };
    let client = reqwest::Client::new();

    let mut graph: DiGraph<NodeAttrs, EdgeAttrs> = DiGraph::new();
    let mut idx_map: HashMap<String, NodeIndex> = HashMap::new();

    // ── nodes: load + dedup by entity_id (highest timestamp wins) ───────────
    //
    // This is the node-population counterpart of the `edge_latest` dedup below, AND of
    // `crates/core/src/persistence/scan.rs::latest_entity_weights`, which implements the
    // identical "greatest timestamp wins" rule for entity-node population counts. The two
    // implementations (this file's and `scan.rs`'s) are intentionally NOT unified — ORC-06
    // (Phase 38) requires the oracle's counting path to stay independent of
    // `persistence::scan`, so a defect in one stays visible when checked against the other.
    // Do not refactor this into a shared helper.
    let node_rows =
        raw_select_all_of_kind(&client, &http_base, &ns, &db, &auth, "NodeAdded").await?;

    let mut node_latest: HashMap<String, (u64, Value)> = HashMap::new();
    for row in node_rows {
        let Some(entity_id) = extract_id(&row["payload"]["entity_id"]) else {
            continue;
        };
        let ts = row["timestamp"].as_u64().unwrap_or(0);
        match node_latest.get(&entity_id) {
            Some((prev_ts, _)) if *prev_ts >= ts => {}
            _ => {
                node_latest.insert(entity_id, (ts, row));
            }
        }
    }

    // Sorted-key iteration, not raw `HashMap` iteration: the 2026-08-27 and 2026-08-28 exports
    // were the same graph (113,250 nodes / 331,129 links) yet their Parquet column orders
    // differed, because pandas derives columns from first-appearance row order. `render_graphml`
    // is already deterministic given insertion order; this is the only non-deterministic input.
    let mut node_ids: Vec<&String> = node_latest.keys().collect();
    node_ids.sort();
    for entity_id in node_ids {
        let (_ts, row) = &node_latest[entity_id];
        let Some(wj) = row["payload"]["weight_json"].as_str() else {
            continue;
        };
        let Ok(weight) = serde_json::from_str::<Value>(wj) else {
            continue;
        };
        let node_type = weight
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let label = weight
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(entity_id)
            .to_string();
        let extra = flatten_weight(&weight, &["type", "name"]);
        let idx = graph.add_node(NodeAttrs {
            entity_id: entity_id.clone(),
            label,
            node_type,
            extra,
        });
        idx_map.insert(entity_id.clone(), idx);
    }

    if idx_map.is_empty() {
        return Err(EmptyGraphEvents.into());
    }
    println!("Loaded {} nodes from graph_events", idx_map.len());

    // ── edges: load + dedup by (source, target, edge_type) (highest timestamp wins) ─────
    let edge_rows =
        raw_select_all_of_kind(&client, &http_base, &ns, &db, &auth, "EdgeAdded").await?;

    let (edge_latest, edge_skip) = dedup_edge_rows(edge_rows, &idx_map);

    // Sorted-key iteration, not raw `HashMap` iteration — same rationale as `node_ids` above.
    let mut edge_keys: Vec<&EdgeTriple> = edge_latest.keys().collect();
    edge_keys.sort();
    let mut edge_count = 0usize;
    for key in edge_keys {
        let (src, dst, edge_type) = key;
        let (_ts, row) = &edge_latest[key];
        // Endpoints were already confirmed present in `idx_map` inside `dedup_edge_rows`.
        let si = idx_map[src];
        let di = idx_map[dst];
        let weight: Value = row["payload"]["weight_json"]
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        // Flatten one level: top-level scalars + nested objects (meta.*, data.*).
        // Preserves the temporal edge metadata (created_at / effective_from / _to).
        let extra = flatten_weight(&weight, &["type"]);
        graph.add_edge(
            si,
            di,
            EdgeAttrs {
                edge_type: edge_type.clone(),
                extra,
            },
        );
        edge_count += 1;
    }
    println!(
        "Loaded {} edges from graph_events ({} skipped — unresolved endpoints)",
        edge_count, edge_skip
    );

    Ok(graph)
}

/// Dedup key for an edge: `(source entity_id, target entity_id, edge_type)`.
type EdgeTriple = (String, String, String);
/// One surviving `(timestamp, raw row)` pair per [`EdgeTriple`].
type EdgeDedupMap = HashMap<EdgeTriple, (u64, Value)>;

/// Collapse raw `EdgeAdded` rows to one entry per `(source, target, edge_type)` triple, keeping
/// the row with the greatest `graph_events.timestamp`. Returns the dedup map plus a count of rows
/// skipped for unresolved/unparseable endpoints (mirrors the pre-dedup `edge_skip` semantics
/// exactly — a row with a missing `entity_id` or an endpoint absent from `idx_map` is still
/// counted as skipped, checked before the dedup key is built).
///
/// This is the edge-dedup counterpart of the node dedup above in `load_from_events`, AND of
/// `crates/core/src/persistence/scan.rs::latest_entity_weights`, which implements the identical
/// "greatest timestamp wins" rule for entity-node population counts. The two implementations
/// (this file's and `scan.rs`'s) are intentionally NOT unified — ORC-06 (Phase 38) requires the
/// oracle's counting path to stay independent of `persistence::scan`, so a defect in one stays
/// visible when checked against the other. Do not refactor this into a shared helper.
///
/// # Why timestamp, and not scan order
///
/// `graph_events` record ids are random, so `ORDER BY id` (the paged scan's order) is NOT
/// chronological. "Last row in scan order" is an arbitrary pick among a triple's events, not its
/// most recent write — this exact mistake shipped once (quick task `260831-ffx`, 2026-08-31) and
/// collapsed a probe's published count from 37,856 to 15,096 while count-only unit tests stayed
/// green both before and after.
fn dedup_edge_rows(
    rows: Vec<Value>,
    idx_map: &HashMap<String, NodeIndex>,
) -> (EdgeDedupMap, usize) {
    let mut edge_latest: EdgeDedupMap = HashMap::new();
    let mut edge_skip = 0usize;
    for row in rows {
        let (Some(src), Some(dst)) = (
            extract_id(&row["payload"]["src_id"]),
            extract_id(&row["payload"]["dst_id"]),
        ) else {
            edge_skip += 1;
            continue;
        };
        if idx_map.get(&src).is_none() || idx_map.get(&dst).is_none() {
            edge_skip += 1;
            continue;
        }
        let weight: Value = row["payload"]["weight_json"]
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        let edge_type = weight
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let ts = row["timestamp"].as_u64().unwrap_or(0);
        let key = (src, dst, edge_type);
        match edge_latest.get(&key) {
            Some((prev_ts, _)) if *prev_ts >= ts => {}
            _ => {
                edge_latest.insert(key, (ts, row));
            }
        }
    }
    (edge_latest, edge_skip)
}

/// Every `graph_events` row of `kind`, paged to exhaustion.
///
/// The two exports used to run a single `LIMIT 100000` / `LIMIT 200000`. Those ceilings were
/// sized to a past graph and the graph outgrew them: measured 2026-07-29 against
/// `an_anglesey_aebaa785`, the table holds 358,829 NodeAdded and 388,162 EdgeAdded rows. A
/// GraphML export truncated at a ceiling is not obviously wrong — it opens, it renders, and the
/// missing half looks like an absence of data rather than an absence of rows.
///
/// `kind` alone in the WHERE clause is safe; the SurrealDB 3.x planner bug needs an unindexed
/// CONTAINS alongside it.
async fn raw_select_all_of_kind(
    client: &reqwest::Client,
    base_url: &str,
    ns: &str,
    db: &str,
    auth: &Option<(String, String)>,
    kind: &str,
) -> Result<Vec<Value>> {
    const PAGE: usize = 20_000;
    let mut out = Vec::new();
    let mut start = 0usize;
    loop {
        // `id` is in the projection because SurrealDB 3.x rejects an ORDER BY idiom that is not
        // selected; callers ignore it.
        let sql = format!(
            "SELECT id, payload, timestamp FROM graph_events \
             WHERE kind = '{kind}' ORDER BY id LIMIT {PAGE} START {start}"
        );
        let rows = raw_select(client, base_url, ns, db, auth, &sql).await?;
        let returned = rows.len();
        if returned == 0 {
            break;
        }
        out.extend(rows);
        start += returned;
    }
    Ok(out)
}

/// Run a single SELECT against the raw HTTP `/sql` endpoint and return the result rows.
async fn raw_select(
    client: &reqwest::Client,
    base_url: &str,
    ns: &str,
    db: &str,
    auth: &Option<(String, String)>,
    sql: &str,
) -> Result<Vec<Value>> {
    let url = format!("{}/sql", base_url.trim_end_matches('/'));
    let mut req = client
        .post(&url)
        .header("surreal-ns", ns)
        .header("surreal-db", db)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(sql.to_owned());
    if let Some((user, pass)) = auth {
        req = req.basic_auth(user, Some(pass));
    }
    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("graph_events SELECT HTTP {status}: {body}");
    }
    // Response shape: [{status: "OK", result: [ … ]}]
    let mut outer: Vec<Value> = resp.json().await?;
    let first = outer
        .pop()
        .ok_or_else(|| anyhow::anyhow!("empty graph_events SELECT response"))?;
    if first["status"].as_str() != Some("OK") {
        let detail = first["result"].as_str().unwrap_or("unknown error");
        bail!("graph_events SELECT error: {detail}");
    }
    match first["result"].clone() {
        Value::Array(a) => Ok(a),
        _ => bail!("unexpected graph_events SELECT result shape"),
    }
}

/// Extract an entity id from a SurrealDB field that may be a plain string or a
/// RecordId object (`{"tb": …, "id": …}`). Mirrors `Projector::extract_id`.
fn extract_id(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_owned());
    }
    if let (Some(tb), Some(id_val)) = (v["tb"].as_str(), Some(&v["id"])) {
        let id_str = if let Some(s) = id_val.as_str() {
            s.to_owned()
        } else if let Some(s) = id_val["String"].as_str() {
            s.to_owned()
        } else {
            id_val.to_string().trim_matches('"').to_owned()
        };
        return Some(format!("{tb}:{id_str}"));
    }
    None
}

/// Flatten a parsed `weight_json` object into `(key, value)` scalar attribute pairs.
///
/// Top-level scalars become `key`. Nested objects are flattened one level into
/// `parent_child` keys (scalars only), capturing `status.type`, `meta.created_at`,
/// `data.details`, etc. Keys in `skip_top` are omitted.
fn flatten_weight(weight: &Value, skip_top: &[&str]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(obj) = weight.as_object() else {
        return out;
    };
    for (k, v) in obj {
        if skip_top.contains(&k.as_str()) {
            continue;
        }
        match v {
            Value::Object(sub) => {
                for (sk, sv) in sub {
                    if let Some(s) = scalar_str(sv) {
                        out.push((format!("{k}_{sk}"), s));
                    }
                }
            }
            _ => {
                if let Some(s) = scalar_str(v) {
                    out.push((k.clone(), s));
                }
            }
        }
    }
    out
}

// ── helpers ───────────────────────────────────────────────────────────────

fn scalar_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn scalar_fields(obj: &serde_json::Map<String, Value>, skip: &[&str]) -> Vec<(String, String)> {
    obj.iter()
        .filter(|(k, _)| !skip.contains(&k.as_str()))
        .filter_map(|(k, v)| scalar_str(v).map(|s| (k.clone(), s)))
        .collect()
}

fn xml_esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn render_graphml(graph: &DiGraph<NodeAttrs, EdgeAttrs>) -> Result<String> {
    let mut out = String::new();

    writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#)?;
    writeln!(
        out,
        r#"<graphml xmlns="http://graphml.graphdrawing.org/xmlns"
  xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
  xsi:schemaLocation="http://graphml.graphdrawing.org/xmlns
    http://graphml.graphdrawing.org/xmlns/1.0/graphml.xsd">"#
    )?;

    // Fixed keys present on every node / edge
    writeln!(
        out,
        r#"  <key id="label"     for="node" attr.name="label"     attr.type="string"/>"#
    )?;
    writeln!(
        out,
        r#"  <key id="node_type" for="node" attr.name="node_type" attr.type="string"/>"#
    )?;
    writeln!(
        out,
        r#"  <key id="edge_type" for="edge" attr.name="edge_type" attr.type="string"/>"#
    )?;

    // Collect all dynamic attribute keys so we can declare them once
    let mut node_keys: std::collections::BTreeSet<String> = Default::default();
    let mut edge_keys: std::collections::BTreeSet<String> = Default::default();
    for w in graph.node_weights() {
        for (k, _) in &w.extra {
            node_keys.insert(k.clone());
        }
    }
    for w in graph.edge_weights() {
        for (k, _) in &w.extra {
            edge_keys.insert(k.clone());
        }
    }
    for k in &node_keys {
        writeln!(
            out,
            r#"  <key id="n_{k}" for="node" attr.name="{k}" attr.type="string"/>"#
        )?;
    }
    for k in &edge_keys {
        writeln!(
            out,
            r#"  <key id="e_{k}" for="edge" attr.name="{k}" attr.type="string"/>"#
        )?;
    }

    writeln!(out, r#"  <graph id="G" edgedefault="directed">"#)?;

    for idx in graph.node_indices() {
        let w = &graph[idx];
        writeln!(out, r#"    <node id="{}">"#, xml_esc(&w.entity_id))?;
        writeln!(
            out,
            r#"      <data key="label">{}</data>"#,
            xml_esc(&w.label)
        )?;
        writeln!(
            out,
            r#"      <data key="node_type">{}</data>"#,
            xml_esc(&w.node_type)
        )?;
        for (k, v) in &w.extra {
            writeln!(out, r#"      <data key="n_{k}">{}</data>"#, xml_esc(v))?;
        }
        writeln!(out, r#"    </node>"#)?;
    }

    for (i, eidx) in graph.edge_indices().enumerate() {
        let (si, di) = graph.edge_endpoints(eidx).unwrap();
        let w = &graph[eidx];
        writeln!(
            out,
            r#"    <edge id="e{i}" source="{}" target="{}">"#,
            xml_esc(&graph[si].entity_id),
            xml_esc(&graph[di].entity_id),
        )?;
        writeln!(
            out,
            r#"      <data key="edge_type">{}</data>"#,
            xml_esc(&w.edge_type)
        )?;
        for (k, v) in &w.extra {
            writeln!(out, r#"      <data key="e_{k}">{}</data>"#, xml_esc(v))?;
        }
        writeln!(out, r#"    </edge>"#)?;
    }

    writeln!(out, r#"  </graph>"#)?;
    writeln!(out, r#"</graphml>"#)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny graph: Officer -Directs-> Entity -LocatedAt-> Location,
    /// plus an unconnected Ward.
    fn fixture() -> DiGraph<NodeAttrs, EdgeAttrs> {
        let mut g: DiGraph<NodeAttrs, EdgeAttrs> = DiGraph::new();
        let mk = |id: &str, ty: &str| NodeAttrs {
            entity_id: id.to_string(),
            label: id.to_string(),
            node_type: ty.to_string(),
            extra: vec![],
        };
        let officer = g.add_node(mk("director:jane-smith", "Officer"));
        let entity = g.add_node(mk("GB-COH:12345678", "Entity"));
        let location = g.add_node(mk("loc:1", "Location"));
        g.add_node(mk("ward:W05000001", "Ward")); // deliberately unconnected

        let edge = |ty: &str| EdgeAttrs {
            edge_type: ty.to_string(),
            extra: vec![],
        };
        g.add_edge(officer, entity, edge("Directs"));
        g.add_edge(entity, location, edge("LocatedAt"));
        g
    }

    /// The default is `events`, not `native`. `native` hits the SurrealDB SDK flatbuffers
    /// ceiling on any real-sized analysis (358k nodes errors outright), so defaulting to it
    /// meant the obvious invocation was the broken one. Pinned here because a default is a
    /// one-word edit away from silently regressing.
    /// The empty-table signal is matched by TYPE, so it survives both a rewording of its message
    /// and an `anyhow` context wrapper added by an intermediate caller — the two ways the old
    /// `to_string().contains(...)` check would have silently broken.
    #[test]
    fn empty_graph_events_is_downcastable_through_a_context_chain() {
        use anyhow::Context as _;
        let bare: anyhow::Error = EmptyGraphEvents.into();
        assert!(bare.downcast_ref::<EmptyGraphEvents>().is_some());
        assert!(
            bare.to_string()
                .contains("No NodeAdded events found in graph_events"),
            "run()'s CLI wording must be preserved: {bare}"
        );

        let wrapped: anyhow::Error = Err::<(), anyhow::Error>(EmptyGraphEvents.into())
            .context("loading the canonical population")
            .unwrap_err();
        assert!(
            wrapped.downcast_ref::<EmptyGraphEvents>().is_some(),
            "a context wrapper must not hide the typed signal: {wrapped:#}"
        );

        let other = anyhow::anyhow!("No NodeAdded events found in graph_events (lookalike prose)");
        assert!(
            other.downcast_ref::<EmptyGraphEvents>().is_none(),
            "prose that merely quotes the message must NOT be treated as emptiness"
        );
    }

    #[test]
    fn default_source_is_events_not_native() {
        use clap::Parser;
        #[derive(Parser)]
        struct Wrap {
            #[command(flatten)]
            args: ExportGraphmlArgs,
        }
        let w = Wrap::parse_from(["export-graphml"]);
        assert_eq!(w.args.source, GraphSource::Events);
    }

    /// Pins the three-state `--reachable-from-wards` contract at the clap layer, not just
    /// at the `effective_ward_codes` function level: absent → `None`, bare flag → `Some([])`,
    /// explicit codes → `Some(codes)`.
    #[test]
    fn reachable_from_wards_flag_has_three_distinct_states() {
        use clap::Parser;
        #[derive(Parser)]
        struct Wrap {
            #[command(flatten)]
            args: ExportGraphmlArgs,
        }
        let absent = Wrap::parse_from(["export-graphml"]);
        assert_eq!(absent.args.reachable_from_wards, None);

        let bare = Wrap::parse_from(["export-graphml", "--reachable-from-wards"]);
        assert_eq!(bare.args.reachable_from_wards, Some(vec![]));

        let explicit = Wrap::parse_from([
            "export-graphml",
            "--reachable-from-wards",
            "W05001492,W05001493",
        ]);
        assert_eq!(
            explicit.args.reachable_from_wards,
            Some(vec!["W05001492".to_string(), "W05001493".to_string()])
        );
    }

    #[test]
    fn no_filters_returns_the_graph_untouched() {
        let g = filter_graph(fixture(), &[], &[], false).unwrap();
        assert_eq!(g.node_count(), 4);
        assert_eq!(g.edge_count(), 2);
    }

    #[test]
    fn node_type_filter_keeps_only_named_types() {
        let g = filter_graph(fixture(), &["Officer".into(), "Entity".into()], &[], false).unwrap();
        assert_eq!(g.node_count(), 2);
        // The LocatedAt edge's Location endpoint is gone, so the edge goes with it —
        // petgraph::filter_map drops edges whose endpoints did not survive.
        assert_eq!(g.edge_count(), 1);
        assert_eq!(g.edge_weights().next().unwrap().edge_type, "Directs");
    }

    #[test]
    fn type_matching_is_case_insensitive() {
        let g = filter_graph(fixture(), &["oFFiCeR".into()], &[], false).unwrap();
        assert_eq!(g.node_count(), 1);
        assert_eq!(g.node_weights().next().unwrap().node_type, "Officer");
    }

    #[test]
    fn edge_type_filter_keeps_only_named_types() {
        let g = filter_graph(fixture(), &[], &["Directs".into()], false).unwrap();
        assert_eq!(g.edge_count(), 1);
        // Nodes are untouched by an edge-only filter — Location and Ward survive as isolates.
        assert_eq!(g.node_count(), 4);
    }

    #[test]
    fn drop_isolated_removes_nodes_left_without_edges() {
        // Ward is unconnected from the start; filtering to Directs also isolates Location.
        let g = filter_graph(fixture(), &[], &["Directs".into()], true).unwrap();
        assert_eq!(g.edge_count(), 1);
        assert_eq!(
            g.node_count(),
            2,
            "only the Directs endpoints should remain"
        );
        let mut kinds: Vec<&str> = g.node_weights().map(|n| n.node_type.as_str()).collect();
        kinds.sort();
        assert_eq!(kinds, vec!["Entity", "Officer"]);
    }

    /// The load-bearing one. A mistyped filter must NOT quietly produce an empty graph that
    /// reads as "this analysis has no officers" — zero output and no-matching-data are
    /// indistinguishable, which is the defect class this codebase keeps getting bitten by.
    #[test]
    fn unknown_node_type_is_a_hard_error_naming_what_is_present() {
        let err = filter_graph(fixture(), &["Offcier".into()], &[], false)
            .expect_err("a typo must fail loudly, not export an empty graph");
        let msg = err.to_string();
        assert!(msg.contains("Offcier"), "must name the bad input: {msg}");
        assert!(msg.contains("Officer"), "must list what IS present: {msg}");
        assert!(
            msg.contains("Entity") && msg.contains("Ward"),
            "full list: {msg}"
        );
    }

    #[test]
    fn unknown_edge_type_is_a_hard_error_naming_what_is_present() {
        let err = filter_graph(fixture(), &[], &["Direct".into()], false)
            .expect_err("a near-miss edge type must fail loudly");
        let msg = err.to_string();
        assert!(msg.contains("Direct"), "must name the bad input: {msg}");
        assert!(
            msg.contains("Directs") && msg.contains("LocatedAt"),
            "must list present: {msg}"
        );
    }

    #[test]
    fn a_valid_filter_that_legitimately_matches_nothing_is_not_an_error() {
        // Ward exists but has no edges; filtering to it with drop_isolated is a real,
        // correctly-empty answer — distinguishable from the typo case above precisely
        // because the type name resolved.
        let g = filter_graph(fixture(), &["Ward".into()], &[], true).unwrap();
        assert_eq!(g.node_count(), 0);
    }

    // ── ward reachability (ORC-01 / ORC-04) ─────────────────────────────────

    use urban_integrity_core::config::PlanningPortalConfig;

    /// Minimal but fully-parseable `AppConfig`, mirroring `preflight.rs`'s test helper —
    /// only `ingest.planning_portals` varies per test.
    fn test_config(portals: &[(Option<&str>, &str)]) -> AppConfig {
        let json = serde_json::json!({
            "surreal": { "ws_url": "ws://localhost:8000", "namespace": "urban_integrity", "database": "main" },
            "graph": {
                "checkpoint_events_n": 1000, "checkpoint_interval_secs": 60,
                "checkpoint_path": "data/checkpoints", "wal_channel_capacity": 10000,
                "wal_batch_size": 100, "wal_batch_window_ms": 100, "shard_count": 64,
                "snapshot_every_n_events": 1000, "snapshot_interval_secs": 30
            },
            "ingest": { "cf_poll_interval_secs": 900, "scrapling_url": "http://localhost", "scrapling_port": 8765 },
            "detector": { "phoenix_window_days": 180, "phoenix_min_value": 10000.0, "imd_csv_path": null }
        });
        let mut config: AppConfig = serde_json::from_value(json).expect("config parses");
        config.ingest.planning_portals = portals
            .iter()
            .map(|(lpa, ward)| PlanningPortalConfig {
                lpa_code: lpa.map(|s| s.to_string()),
                ward_code: (*ward).to_string(),
                portal_type: "arcus".to_string(),
                url: "https://example.invalid".to_string(),
            })
            .collect();
        config
    }

    #[test]
    fn effective_ward_codes_absent_flag_returns_none() {
        let config = test_config(&[(None, "W05001492")]);
        assert_eq!(effective_ward_codes(&None, &config), None);
    }

    #[test]
    fn effective_ward_codes_bare_flag_falls_back_to_config_deduplicated_and_sorted() {
        let config = test_config(&[
            (Some("W06000001"), "W05001493"),
            (Some("W06000001"), "W05001492"),
        ]);
        let codes = effective_ward_codes(&Some(vec![]), &config).expect("Some");
        assert_eq!(
            codes,
            vec![
                "W05001492".to_string(),
                "W05001493".to_string(),
                "W06000001".to_string(),
            ]
        );
    }

    #[test]
    fn effective_ward_codes_explicit_codes_override_config_entirely() {
        let config = test_config(&[(Some("W06000001"), "W05001492")]);
        let codes =
            effective_ward_codes(&Some(vec!["W05000001".to_string()]), &config).expect("Some");
        assert_eq!(codes, vec!["W05000001".to_string()]);
    }

    /// Ward node id `ward:W05001492` plus a deliberately colliding non-Ward node whose
    /// `entity_id` CONTAINS that same code as a substring — proves the matcher is typed,
    /// not the retired Python's `any(c in n['id'] for c in target_codes)` substring test.
    fn reachability_seed_fixture() -> DiGraph<NodeAttrs, EdgeAttrs> {
        let mut g: DiGraph<NodeAttrs, EdgeAttrs> = DiGraph::new();
        let mk = |id: &str, ty: &str| NodeAttrs {
            entity_id: id.to_string(),
            label: id.to_string(),
            node_type: ty.to_string(),
            extra: vec![],
        };
        g.add_node(mk("ward:W05001492", "Ward"));
        g.add_node(mk("planning:W05001492-99", "Planning")); // substring-colliding, not a Ward
        g
    }

    #[test]
    fn resolve_ward_seeds_matcher_is_typed_not_substring() {
        let g = reachability_seed_fixture();
        let seeds = resolve_ward_seeds(&g, &["W05001492".to_string()]).unwrap();
        assert_eq!(seeds.len(), 1, "only the Ward node must match");
        let idx = *seeds.iter().next().unwrap();
        assert_eq!(g[idx].node_type, "Ward");
        assert_eq!(g[idx].entity_id, "ward:W05001492");
    }

    #[test]
    fn resolve_ward_seeds_zero_match_is_a_hard_error_naming_codes_and_present_types() {
        let g = reachability_seed_fixture();
        let err = resolve_ward_seeds(&g, &["W09999999".to_string()])
            .expect_err("a code matching no Ward node must fail loudly");
        let msg = err.to_string();
        assert!(msg.contains("W09999999"), "must name the bad code: {msg}");
        assert!(msg.contains("Ward"), "must list present node types: {msg}");
        assert!(
            msg.contains("Planning"),
            "must list every present node type, not just Ward: {msg}"
        );
    }

    #[test]
    fn resolve_ward_seeds_lpa_code_alone_resolving_nothing_is_still_a_hard_error() {
        // The LPA code legitimately matches no node type on its own — but supplying ONLY
        // the LPA code must still fail per ORC-04, since the effective seed set is empty.
        let g = reachability_seed_fixture();
        let err = resolve_ward_seeds(&g, &["W06000001".to_string()])
            .expect_err("LPA-only codes resolving zero seeds must fail loudly");
        assert!(err.to_string().contains("W06000001"));
    }

    /// Ward(seed) -Directs-> Mid -Directs-> Far (forward, two hops), X -Directs-> Ward
    /// (only reachable via the INBOUND direction of a directed edge), and an entirely
    /// disconnected Isolated node.
    fn reachability_bfs_fixture() -> (
        DiGraph<NodeAttrs, EdgeAttrs>,
        NodeIndex, // ward (seed)
        NodeIndex, // far (two forward hops away)
        NodeIndex, // x (only reachable via inbound edge)
        NodeIndex, // isolated (separate component)
    ) {
        let mut g: DiGraph<NodeAttrs, EdgeAttrs> = DiGraph::new();
        let mk = |id: &str, ty: &str| NodeAttrs {
            entity_id: id.to_string(),
            label: id.to_string(),
            node_type: ty.to_string(),
            extra: vec![],
        };
        let ward = g.add_node(mk("ward:W05001492", "Ward"));
        let mid = g.add_node(mk("loc:1", "Location"));
        let far = g.add_node(mk("GB-COH:1", "Entity"));
        let x = g.add_node(mk("director:x", "Officer"));
        let isolated = g.add_node(mk("GB-COH:2", "Entity"));

        let edge = |ty: &str| EdgeAttrs {
            edge_type: ty.to_string(),
            extra: vec![],
        };
        g.add_edge(ward, mid, edge("LocatedAt"));
        g.add_edge(mid, far, edge("Represents"));
        // X -> Ward: X is only reachable from `ward` via the INBOUND direction of this
        // edge. A directed `petgraph::Bfs` starting at `ward` would never visit X.
        g.add_edge(x, ward, edge("Directs"));
        // `isolated` has no edge to anything — a separate weakly-connected component.

        (g, ward, far, x, isolated)
    }

    // ── population_from_graph (WBX-06 / D-19, the workbench's "unavailable, not fatal" policy) ──

    /// Seeds resolve: the population is the reachable-from-wards node count, exactly what
    /// `run()`'s `--reachable-from-wards` export would contain (ward + mid + far + x, not the
    /// isolated node).
    #[test]
    fn population_from_graph_counts_the_reachable_component_when_seeds_resolve() {
        let (g, _ward, _far, _x, _isolated) = reachability_bfs_fixture();
        let codes = vec!["W05001492".to_string()];
        match population_from_graph(g, Some(&codes), "an_test") {
            CanonicalPopulation::Available { count, .. } => assert_eq!(count, 4),
            other => panic!("expected Available, got {other:?}"),
        }
    }

    /// Zero seeds resolve: the workbench gets a typed Unavailable naming the codes it tried and
    /// the database — NOT `run()`'s hard error, and NOT a fabricated `0`.
    ///
    /// PROVE THIS CAN FAIL: returning `Available { count: 0 }` here (the naive "empty is fine"
    /// shortcut) fails the match; propagating `resolve_ward_seeds`'s `bail!` would not compile
    /// against this signature.
    #[test]
    fn population_from_graph_is_unavailable_with_a_reason_when_no_seed_resolves() {
        let (g, _ward, _far, _x, _isolated) = reachability_bfs_fixture();
        let codes = vec!["W05009999".to_string(), "W06000001".to_string()];
        match population_from_graph(g, Some(&codes), "an_other") {
            CanonicalPopulation::Unavailable { reason, .. } => {
                assert!(
                    reason.contains("W05009999"),
                    "must name the codes tried: {reason}"
                );
                assert!(
                    reason.contains("W06000001"),
                    "must name the codes tried: {reason}"
                );
                assert!(
                    reason.contains("an_other"),
                    "must name the database: {reason}"
                );
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    /// No ward codes configured anywhere: the population is the whole loaded graph.
    #[test]
    fn population_from_graph_with_no_codes_is_the_whole_graph() {
        let (g, _ward, _far, _x, _isolated) = reachability_bfs_fixture();
        let total = g.node_count() as u64;
        match population_from_graph(g, None, "an_test") {
            CanonicalPopulation::Available { count, .. } => assert_eq!(count, total),
            other => panic!("expected Available, got {other:?}"),
        }
    }

    #[test]
    fn filter_reachable_keeps_two_hop_forward_reachable_node() {
        let (g, ward, far, _x, _isolated) = reachability_bfs_fixture();
        let seeds: HashSet<NodeIndex> = [ward].into_iter().collect();
        let filtered = filter_reachable(g, &seeds);
        assert!(
            filtered.node_weights().any(|n| n.entity_id == "GB-COH:1"),
            "far must survive a two-hop forward path from the seed"
        );
        // sanity: the far node captured before filtering is only meaningful via entity_id,
        // since filter_map renumbers indices — assert via id, not the stale `far` index.
        let _ = far;
    }

    #[test]
    fn filter_reachable_undirected_bfs_keeps_node_reachable_only_via_inbound_edge() {
        // This is the case a directed `petgraph::Bfs` would drop: X's only edge points
        // INTO the seed (X -> Ward), so a directed traversal from `ward` never visits X.
        let (g, ward, _far, _x, _isolated) = reachability_bfs_fixture();
        let seeds: HashSet<NodeIndex> = [ward].into_iter().collect();
        let filtered = filter_reachable(g, &seeds);
        assert!(
            filtered.node_weights().any(|n| n.entity_id == "director:x"),
            "X must survive — reachable only via the inbound direction of a directed edge"
        );
    }

    #[test]
    fn filter_reachable_drops_a_node_with_no_path_to_any_seed() {
        let (g, ward, _far, _x, _isolated) = reachability_bfs_fixture();
        let seeds: HashSet<NodeIndex> = [ward].into_iter().collect();
        let filtered = filter_reachable(g, &seeds);
        assert!(
            !filtered.node_weights().any(|n| n.entity_id == "GB-COH:2"),
            "a node in a separate connected component must be dropped"
        );
    }

    /// The load-bearing order-dependence test (phase 37 research Risk 1 / Criterion 7).
    /// Fixture: Ward(seed) -LocatedAt-> Location -Represents-> Entity.
    ///
    /// Right order (reachability, then type filter) keeps Entity: the whole reachable
    /// component — Ward, Location, Entity — survives `filter_reachable`, and the
    /// subsequent `--node-type Entity` filter narrows within that population.
    ///
    /// Wrong order (type filter, then reachability) does NOT keep Entity: filtering to
    /// `["Entity", "Ward"]` first removes Location — the bridging edge's endpoint — so
    /// when reachability finally runs from the (still-present) Ward seed, there is no
    /// path left to Entity. Ward is kept in the wrong-order type filter specifically so
    /// the seed itself survives to prove the BRIDGE, not the seed, is what breaks.
    #[test]
    fn reachability_runs_before_type_filter_not_after_pipeline_order() {
        let bridge_fixture = || {
            let mut g: DiGraph<NodeAttrs, EdgeAttrs> = DiGraph::new();
            let mk = |id: &str, ty: &str| NodeAttrs {
                entity_id: id.to_string(),
                label: id.to_string(),
                node_type: ty.to_string(),
                extra: vec![],
            };
            let ward = g.add_node(mk("ward:W05001492", "Ward"));
            let location = g.add_node(mk("loc:1", "Location"));
            let entity = g.add_node(mk("GB-COH:1", "Entity"));
            let edge = |ty: &str| EdgeAttrs {
                edge_type: ty.to_string(),
                extra: vec![],
            };
            g.add_edge(ward, location, edge("LocatedAt"));
            g.add_edge(location, entity, edge("Represents"));
            g
        };
        let codes = vec!["W05001492".to_string()];

        // Right order: filter_reachable, THEN filter_graph(["Entity"]).
        let g0 = bridge_fixture();
        let seeds0 = resolve_ward_seeds(&g0, &codes).unwrap();
        let g1 = filter_reachable(g0, &seeds0);
        let g2 = filter_graph(g1, &["Entity".into()], &[], false).unwrap();
        assert!(
            g2.node_weights().any(|n| n.entity_id == "GB-COH:1"),
            "right order (reachability before type filter) must keep Entity"
        );

        // Wrong order: filter_graph(["Entity", "Ward"]), THEN filter_reachable.
        let g0 = bridge_fixture();
        let g1 = filter_graph(g0, &["Entity".into(), "Ward".into()], &[], false).unwrap();
        let seeds1 = resolve_ward_seeds(&g1, &codes).unwrap();
        let g2 = filter_reachable(g1, &seeds1);
        assert!(
            !g2.node_weights().any(|n| n.entity_id == "GB-COH:1"),
            "wrong order (type filter before reachability) must NOT keep Entity — \
             the Location bridge was removed first"
        );
    }

    // ── edge dedup (37-02, ORC-02) ───────────────────────────────────────────

    /// Build a raw `EdgeAdded` row shaped exactly as `dedup_edge_rows` expects: plain-string
    /// `src_id`/`dst_id` (accepted by `extract_id`), a `weight_json` string carrying `type` and
    /// a distinguishing `marker`, and a top-level `timestamp`.
    fn edge_row(src: &str, dst: &str, edge_type: &str, ts: u64, marker: &str) -> Value {
        let weight = serde_json::json!({ "type": edge_type, "marker": marker });
        serde_json::json!({
            "payload": {
                "src_id": src,
                "dst_id": dst,
                "weight_json": serde_json::to_string(&weight).unwrap(),
            },
            "timestamp": ts,
        })
    }

    /// `idx_map` fixture with two resolvable endpoints (`dedup_edge_rows` only needs
    /// presence-in-map, not real graph indices behind them).
    fn edge_idx_map() -> HashMap<String, NodeIndex> {
        let mut m = HashMap::new();
        m.insert("GB-COH:1".to_string(), NodeIndex::new(0));
        m.insert("GB-COH:2".to_string(), NodeIndex::new(1));
        m
    }

    /// The exact shape of the regression that shipped once already (`260831-ffx`): the greatest
    /// timestamp (900) sits in the MIDDLE of scan order, not last. A "last row in scan order
    /// wins" implementation keeps the `ts100` row; the correct implementation keeps `ts900`.
    #[test]
    fn duplicate_edge_tie_break_prefers_greatest_timestamp_when_it_is_not_last_in_scan_order() {
        let rows = vec![
            edge_row("GB-COH:1", "GB-COH:2", "Directs", 500, "ts500"),
            edge_row("GB-COH:1", "GB-COH:2", "Directs", 900, "ts900"),
            edge_row("GB-COH:1", "GB-COH:2", "Directs", 100, "ts100"),
        ];
        let (latest, skip) = dedup_edge_rows(rows, &edge_idx_map());
        assert_eq!(skip, 0);
        assert_eq!(
            latest.len(),
            1,
            "one surviving row per (src, dst, edge_type) triple"
        );
        let key = (
            "GB-COH:1".to_string(),
            "GB-COH:2".to_string(),
            "Directs".to_string(),
        );
        let (ts, row) = &latest[&key];
        assert_eq!(*ts, 900);
        assert_eq!(
            row["payload"]["weight_json"]
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .and_then(|w| w["marker"].as_str().map(|s| s.to_string())),
            Some("ts900".to_string()),
            "the surviving row must be the one with the greatest timestamp, not the last in scan order"
        );
    }

    /// Two rows share `(src, dst)` but differ in `edge_type` — both survive; the dedup key is
    /// the full triple, not just the endpoints.
    #[test]
    fn duplicate_edge_dedup_key_is_the_full_triple_not_just_endpoints() {
        let rows = vec![
            edge_row("GB-COH:1", "GB-COH:2", "Directs", 100, "a"),
            edge_row("GB-COH:1", "GB-COH:2", "LocatedAt", 100, "b"),
        ];
        let (latest, skip) = dedup_edge_rows(rows, &edge_idx_map());
        assert_eq!(skip, 0);
        assert_eq!(
            latest.len(),
            2,
            "differing edge_type must not be collapsed — both triples survive"
        );
    }

    /// Three rows share one triple entirely — exactly one survives, total count drops.
    #[test]
    fn three_rows_sharing_one_triple_collapse_to_one() {
        let rows = vec![
            edge_row("GB-COH:1", "GB-COH:2", "Directs", 1, "a"),
            edge_row("GB-COH:1", "GB-COH:2", "Directs", 2, "b"),
            edge_row("GB-COH:1", "GB-COH:2", "Directs", 3, "c"),
        ];
        let (latest, skip) = dedup_edge_rows(rows, &edge_idx_map());
        assert_eq!(skip, 0);
        assert_eq!(latest.len(), 1);
    }

    /// Rows whose endpoints do not resolve in `idx_map` are counted as skipped, and are not
    /// inserted into the dedup map — the pre-dedup `edge_skip` semantics are unchanged.
    #[test]
    fn unresolved_endpoint_is_skipped_not_inserted_into_dedup_map() {
        let rows = vec![edge_row("GB-COH:1", "GB-COH:999", "Directs", 1, "a")];
        let (latest, skip) = dedup_edge_rows(rows, &edge_idx_map());
        assert_eq!(skip, 1);
        assert!(latest.is_empty());
    }

    // ── deterministic insertion order (37-02, ORC-03) ───────────────────────

    /// Sorting `node_latest`-shaped keys is order-independent: the same three keys inserted
    /// into two `HashMap`s in two different explicit orders yield the identical sorted
    /// `Vec<String>` both times. This pins the sort as the fix, without relying on a live
    /// `HashMap`'s actual (randomised, per-process) iteration order — so the test is never
    /// flaky regardless of hash-seed.
    #[test]
    fn node_insertion_order_is_sorted_regardless_of_hashmap_build_order() {
        let build = |ids: &[&str]| -> Vec<String> {
            let mut m: HashMap<String, (u64, Value)> = HashMap::new();
            for id in ids {
                m.insert(id.to_string(), (0, Value::Null));
            }
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            keys.into_iter().cloned().collect()
        };
        let order_a = build(&["c", "a", "b"]);
        let order_b = build(&["b", "c", "a"]);
        let expected = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(order_a, expected);
        assert_eq!(order_b, expected);
    }

    /// Same property for the `(source, target, edge_type)` triple keys `edge_latest` uses:
    /// sorted regardless of build order, tuple `Ord` giving lexicographic
    /// source-then-target-then-edge_type order.
    #[test]
    fn edge_insertion_order_is_sorted_regardless_of_hashmap_build_order() {
        let build = |triples: &[(&str, &str, &str)]| -> Vec<EdgeTriple> {
            let mut m: EdgeDedupMap = HashMap::new();
            for (s, d, t) in triples {
                m.insert(
                    (s.to_string(), d.to_string(), t.to_string()),
                    (0, Value::Null),
                );
            }
            let mut keys: Vec<&EdgeTriple> = m.keys().collect();
            keys.sort();
            keys.into_iter().cloned().collect()
        };
        let triples_a = [
            ("z", "y", "Directs"),
            ("a", "b", "LocatedAt"),
            ("a", "b", "Directs"),
        ];
        let triples_b = [
            ("a", "b", "Directs"),
            ("z", "y", "Directs"),
            ("a", "b", "LocatedAt"),
        ];
        let order_a = build(&triples_a);
        let order_b = build(&triples_b);
        let expected = vec![
            ("a".to_string(), "b".to_string(), "Directs".to_string()),
            ("a".to_string(), "b".to_string(), "LocatedAt".to_string()),
            ("z".to_string(), "y".to_string(), "Directs".to_string()),
        ];
        assert_eq!(order_a, expected);
        assert_eq!(order_b, expected);
    }

    // ── canonical_population_summary (D-19, WBX-06, plan 42-07 Task 1) ──────

    /// Seed a `graph_events` fixture directly through the DB-gated `TestNs` connection: one
    /// `Ward` node, one `Entity` node reachable via a `LocatedAt` edge, and one entirely
    /// disconnected `Entity` — proving the reachability filter (not just node loading) runs
    /// inside `canonical_population_summary`, per its D-19 doc comment.
    async fn seed_canonical_population_fixture(
        db: &surrealdb::Surreal<surrealdb::engine::any::Any>,
    ) {
        use urban_integrity_schema::types::edge::{EdgeMeta, EdgeWeight, LocatedAtData};
        use urban_integrity_schema::types::node::{EntityData, EntityStatus, NodeWeight, WardData};

        let ward_weight = serde_json::to_string(&NodeWeight::Ward(WardData {
            name: "Test Ward".to_string(),
        }))
        .expect("WardData serialises");
        let entity_weight = serde_json::to_string(&NodeWeight::Entity(EntityData {
            name: "Reachable Co".to_string(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        }))
        .expect("EntityData serialises");
        let isolated_weight = serde_json::to_string(&NodeWeight::Entity(EntityData {
            name: "Isolated Co".to_string(),
            company_number: None,
            status: EntityStatus::Active,
            incorporation_date: None,
        }))
        .expect("EntityData serialises");

        for (event_id, entity_id, weight_json) in [
            ("canon-pop-ward", "ward:W05001492", ward_weight.as_str()),
            (
                "canon-pop-reachable",
                "GB-COH:canon-pop-reachable",
                entity_weight.as_str(),
            ),
            (
                "canon-pop-isolated",
                "GB-COH:canon-pop-isolated",
                isolated_weight.as_str(),
            ),
        ] {
            db.query("CREATE graph_events CONTENT $r")
                .bind((
                    "r",
                    serde_json::json!({
                        "event_id": event_id,
                        "timestamp": 1_700_000_000_000u64,
                        "kind": "NodeAdded",
                        "payload": {
                            "entity_id": entity_id,
                            "weight_json": weight_json,
                        },
                    }),
                ))
                .await
                .expect("failed to seed NodeAdded row");
        }

        let edge = EdgeWeight::LocatedAt {
            meta: EdgeMeta {
                created_at: chrono::Utc::now(),
                effective_from: None,
                effective_to: None,
            },
            data: LocatedAtData {
                details: "test".to_string(),
            },
        };
        let edge_weight_json = serde_json::to_string(&edge).expect("LocatedAt edge serialises");
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": "canon-pop-edge",
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "EdgeAdded",
                    "payload": {
                        "src_id": "ward:W05001492",
                        "dst_id": "GB-COH:canon-pop-reachable",
                        "weight_json": edge_weight_json,
                    },
                }),
            ))
            .await
            .expect("failed to seed EdgeAdded row");
    }

    /// `canonical_population_summary` must return exactly the ward-reachable node count (Ward +
    /// the one LocatedAt-connected Entity, NOT the disconnected Entity), computed through the
    /// same `load_from_events` → ward seeds → `filter_reachable` pipeline `export-graphml`
    /// itself uses — and a generation timestamp within the last few seconds of the call.
    ///
    /// Three calls against one seeded fixture, in precedence order: (1) no registry wards, so
    /// config wards seed the walk (the `main` path); (2) registry wards that match, with config
    /// wards that do NOT — the registry row must win; (3) registry wards that match nothing —
    /// a typed `Unavailable` naming the codes and database, never an error and never `0`.
    #[tokio::test]
    async fn canonical_population_summary_matches_seeded_ward_reachable_fixture() {
        if !urban_integrity_test_support::db_tests_enabled() {
            eprintln!("[skip] set URBAN_DB_TESTS=1 with SurrealDB running to execute");
            return;
        }

        let db =
            urban_integrity_test_support::TestNs::connect_with_prefix("test_canon_pop", "main")
                .await;
        seed_canonical_population_fixture(&db).await;

        let mut config = test_config(&[(None, "W05001492")]);
        config.surreal.namespace = db.ns().to_string();
        config.surreal.database = "main".to_string();
        config.surreal.username = Some("root".to_string());
        config.surreal.password = Some("root".to_string());

        // (1) `main` path: no registry row, config wards seed the walk.
        let before = chrono::Utc::now();
        let population = canonical_population_summary(&config, "main", None)
            .await
            .expect("canonical_population_summary should succeed against the seeded fixture");
        let after = chrono::Utc::now();
        let CanonicalPopulation::Available {
            count,
            generated_at,
            ..
        } = population
        else {
            panic!("config wards match the seeded Ward: expected Available, got {population:?}");
        };
        assert_eq!(
            count, 2,
            "expected exactly the Ward seed plus its one LocatedAt-reachable Entity, not the \
             disconnected Entity"
        );
        assert!(
            generated_at >= before && generated_at <= after,
            "generation timestamp must be within the call's own wall-clock window: {generated_at}"
        );

        // (2) Registry wards take precedence over config wards: config now names a ward that
        // does NOT exist in the graph, the analysis row names the one that does.
        let mismatched_config = {
            let mut c = test_config(&[(None, "W05000001")]);
            c.surreal = config.surreal.clone();
            c
        };
        let registry_codes = vec!["W05001492".to_string()];
        let population =
            canonical_population_summary(&mismatched_config, "main", Some(&registry_codes))
                .await
                .expect("registry-seeded population should succeed");
        assert!(
            matches!(population, CanonicalPopulation::Available { count: 2, .. }),
            "the analysis's own registry wards must seed the walk, not config.toml's: {population:?}"
        );

        // (3) Registry wards that resolve nothing: typed Unavailable, not an error, not 0.
        let unresolvable = vec!["W05009999".to_string()];
        let population = canonical_population_summary(&config, "main", Some(&unresolvable))
            .await
            .expect("zero resolvable seeds must not be an error for the workbench");
        let CanonicalPopulation::Unavailable { reason, .. } = population else {
            panic!("expected Unavailable for unresolvable registry wards, got {population:?}");
        };
        assert!(
            reason.contains("W05009999") && reason.contains("main"),
            "reason must name the codes tried and the database: {reason}"
        );
    }
}
