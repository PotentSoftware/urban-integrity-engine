//! Rail defaults (44-CONTEXT D-07/D-10) and server ceilings (D-08) as constants. `dispatch`'s
//! clamp, the rail's field help and the algorithm help pages all read these — never a retyped
//! literal — so the help cannot state a ceiling the server does not enforce.
//!
//! This crate is the wasm-safe DTO boundary, which is why the constants live here rather than in
//! `crates/workbench`: the rail and the help pages compile to wasm and cannot reach the server
//! crate, and a second copy on the browser side is exactly how a ceiling drifts from the clamp
//! that enforces it.

pub const DEFAULT_HOP_CAP: u32 = 4;
pub const DEFAULT_RESULT_CAP: u32 = 100;
pub const DEFAULT_BUDGET_SECS: u32 = 5;
pub const DEFAULT_K_SHORTEST: u32 = 3;
/// k-core's own default k. **2, not 3** — `tool_rail::algorithm_defaults` overrides the shared
/// `k` field to 2 for `k_core` while leaving k-shortest on [`DEFAULT_K_SHORTEST`]. The two
/// algorithms share one request field but not one default, so they get one constant each.
pub const DEFAULT_K_CORE: u32 = 2;
pub const DEFAULT_TOP_N: u32 = 50;
pub const DEFAULT_MIN_COMMUNITY_SIZE: u32 = 5;
pub const DEFAULT_SEED: u64 = 42;
pub const DEFAULT_RESOLUTION: f64 = 1.0;
/// `ward_subgraph`'s own default hop cap — likewise an override of the shared `hop_cap` field,
/// narrower than [`DEFAULT_HOP_CAP`] because a ward walk fans out fast.
pub const DEFAULT_WARD_SUBGRAPH_HOPS: u32 = 2;

pub const CEILING_HOP_CAP: u32 = 8;
pub const CEILING_RESULT_CAP: u32 = 1_000;
pub const CEILING_BUDGET_SECS: u32 = 30;
pub const CEILING_K_SHORTEST: u32 = 1_000;
pub const CEILING_K_CORE: u32 = 50;
pub const CEILING_TOP_N: u32 = 500;
pub const CEILING_MIN_COMMUNITY_SIZE: u32 = 500;
/// How many source nodes pivot-sampled betweenness draws (D-09). Not a clamp — a fixed sampling
/// parameter the help must state honestly, since the score is an approximation because of it.
pub const BETWEENNESS_PIVOTS: u32 = 500;

// Every default must sit inside its own ceiling. These are compile-time assertions rather than
// runtime tests on purpose: the values are constants, so the invariant can be enforced by the
// build itself — a violating edit fails `cargo build`, not merely `cargo test`. (Clippy's
// `assertions_on_constants` rejects the runtime form for exactly this reason.)
const _: () = assert!(DEFAULT_HOP_CAP <= CEILING_HOP_CAP);
const _: () = assert!(DEFAULT_RESULT_CAP <= CEILING_RESULT_CAP);
const _: () = assert!(DEFAULT_BUDGET_SECS <= CEILING_BUDGET_SECS);
const _: () = assert!(DEFAULT_K_SHORTEST <= CEILING_K_SHORTEST);
const _: () = assert!(DEFAULT_K_CORE <= CEILING_K_CORE);
const _: () = assert!(DEFAULT_TOP_N <= CEILING_TOP_N);
const _: () = assert!(DEFAULT_MIN_COMMUNITY_SIZE <= CEILING_MIN_COMMUNITY_SIZE);
const _: () = assert!(DEFAULT_WARD_SUBGRAPH_HOPS <= CEILING_HOP_CAP);

// A ceiling equal to its default would leave the field with exactly one legal value — a control
// the rail renders but nobody can actually turn.
const _: () = assert!(DEFAULT_HOP_CAP < CEILING_HOP_CAP);
const _: () = assert!(DEFAULT_RESULT_CAP < CEILING_RESULT_CAP);
const _: () = assert!(DEFAULT_BUDGET_SECS < CEILING_BUDGET_SECS);
const _: () = assert!(DEFAULT_K_SHORTEST < CEILING_K_SHORTEST);
const _: () = assert!(DEFAULT_K_CORE < CEILING_K_CORE);
const _: () = assert!(DEFAULT_TOP_N < CEILING_TOP_N);
const _: () = assert!(DEFAULT_MIN_COMMUNITY_SIZE < CEILING_MIN_COMMUNITY_SIZE);

// k-core and k-shortest share the `k` request field but not its default; collapsing them into
// one constant would silently change k-core's behaviour.
const _: () = assert!(DEFAULT_K_CORE != DEFAULT_K_SHORTEST);

// ─────────────────────────────────────────────────────────────────────────────
// Phase 45: neighbourhood expansion (CNV-01) and profile section paging
// (PIV-06). These join the constants above rather than starting a second
// module, per 45-CONTEXT D-13: plan 44-20 established one source of truth read
// by the clamp, the rail and the help pages alike, and "a browser-side copy is
// exactly how a ceiling drifts from the clamp enforcing it".
// ─────────────────────────────────────────────────────────────────────────────

/// How many hops an entity's neighbourhood opens with on the canvas (CNV-01).
///
/// **1, not [`DEFAULT_HOP_CAP`].** CNV-01's phrasing is "expand N hops at a time": the operator
/// arrives at the immediate neighbourhood and chooses to go further. Opening at 4 hops on a
/// high-degree entity would put a graph on screen that nobody asked for and that the node cap
/// would then have to cut — reported honestly, but still a worse first frame than the one the
/// operator can grow deliberately.
pub const DEFAULT_NEIGHBOURHOOD_HOPS: u32 = 1;

/// The furthest a single neighbourhood expansion may reach.
///
/// Narrower than [`CEILING_HOP_CAP`] on purpose: a graph algorithm runs once over a bounded
/// population, whereas neighbourhood expansion is *repeated* — an operator at this ceiling can
/// expand again from any node on screen, so the reachable set is not capped by this number, only
/// the fan-out of one step is. `ward_subgraph` made the same narrowing for the same reason
/// ([`DEFAULT_WARD_SUBGRAPH_HOPS`]).
pub const CEILING_NEIGHBOURHOOD_HOPS: u32 = 3;

/// How many nodes a neighbourhood expansion renders before it is cut.
///
/// The cut is **never silent** (CNV-01, D-12): it is reported through the same mandatory
/// `Truncation` block on `AlgorithmResult` and the same status line under the canvas that phase 44
/// built, not a second truncation convention.
pub const DEFAULT_NEIGHBOURHOOD_NODE_CAP: u32 = 250;

/// The most nodes an operator may raise a single neighbourhood expansion to.
///
/// Above [`CEILING_RESULT_CAP`] deliberately — they cap different things. A result cap bounds a
/// *ranked list* an analyst reads row by row; this bounds a *rendered graph*, where the shape of a
/// 2,000-node cluster is itself the finding. PIV-06's completeness test is written over a
/// 2,000+-neighbour entity, so this ceiling is the figure at which such an entity's neighbourhood
/// can be seen whole rather than only in truncated form.
pub const CEILING_NEIGHBOURHOOD_NODE_CAP: u32 = 2_000;

/// The most seed ids a single neighbourhood request may walk from (T-50-01, WR-04).
///
/// Enforced on BOTH sides from this one constant: the browser caps the `ids=` list it puts in the
/// URL (`finding_detail_page.rs`'s `MAX_SEED_IDS_IN_URL`, reused by `alert_detail_page.rs`), and
/// `GET /api/entity/neighbourhood` truncates whatever it is sent to this many distinct ids,
/// first-seen order, before a single hop is walked — so a hand-built request or a future caller
/// that skips the browser's cap cannot make the server walk an unbounded frontier. The seeds the
/// server actually walked travel back on `NeighbourhoodView::seeds`, the same "applied value, not
/// requested value" report `hops`/`node_cap` already make.
pub const CEILING_NEIGHBOURHOOD_SEEDS: usize = 50;

/// How many nodes one parent must have pulled into an expansion before its cluster is offered for
/// collapse (CNV-02's high-degree cluster glyph).
///
/// **Offered, never applied automatically.** A collapse that happened on its own would be a
/// silently shorter graph with a different mechanism — the very failure the reported node cap
/// exists to prevent — so this is the threshold at which a *control* appears, and the operator
/// presses it.
///
/// 25 rather than a rounder number, for two reasons that bound it from either side. Below about
/// twenty, collapsing hides more than it reveals: an operator can still read a dozen labelled
/// points, and replacing them with one glyph costs a click to get back what was already legible.
/// Above it, a single high-degree parent — a ward, a shared registered address, a frequently
/// awarded authority — takes the whole frame, and the *rest* of the neighbourhood becomes the
/// thing that cannot be seen. It must also stay strictly below
/// [`DEFAULT_NEIGHBOURHOOD_NODE_CAP`], or the control could never appear at the default cap at
/// all; the build asserts that rather than leaving it to a reader.
pub const CLUSTER_COLLAPSE_THRESHOLD: u32 = 25;

/// How many rows one profile section returns per page (PIV-06, D-07).
///
/// The profile path is **paged, not capped**: `queries.rs` carries hard `LIMIT` literals — 20,000
/// on AwardedTo edges, 100,000 on five more — two of them with comments admitting they were
/// already raised once when live data outgrew them. A page size is not one of those. It bounds a
/// *window*, and `Page::total` plus `Page::next_cursor` say what lies beyond it, which is the
/// whole difference between paging and capping.
pub const DEFAULT_PROFILE_PAGE_SIZE: u32 = 50;

/// The largest page a client may ask a profile section for. A ceiling on the page size only, never
/// on the section: an analyst wanting every row of a 2,400-row section pages through it and the
/// `total` tells them how far that is.
pub const CEILING_PROFILE_PAGE_SIZE: u32 = 500;

// Same build-time enforcement as the block above — a violating edit fails `cargo build`, not
// merely `cargo test`.
const _: () = assert!(DEFAULT_NEIGHBOURHOOD_HOPS <= CEILING_NEIGHBOURHOOD_HOPS);
const _: () = assert!(DEFAULT_NEIGHBOURHOOD_NODE_CAP <= CEILING_NEIGHBOURHOOD_NODE_CAP);
const _: () = assert!(DEFAULT_PROFILE_PAGE_SIZE <= CEILING_PROFILE_PAGE_SIZE);

// A ceiling equal to its default leaves the field with exactly one legal value — a control the UI
// renders but nobody can actually turn.
const _: () = assert!(DEFAULT_NEIGHBOURHOOD_HOPS < CEILING_NEIGHBOURHOOD_HOPS);
const _: () = assert!(DEFAULT_NEIGHBOURHOOD_NODE_CAP < CEILING_NEIGHBOURHOOD_NODE_CAP);
const _: () = assert!(DEFAULT_PROFILE_PAGE_SIZE < CEILING_PROFILE_PAGE_SIZE);

// One neighbourhood expansion must stay strictly inside the graph-walk ceiling the server already
// clamps to: expansion is repeated, so its per-step reach is deliberately the narrower of the two.
// Equality here would silently turn one expansion into a full-depth walk.
const _: () = assert!(CEILING_NEIGHBOURHOOD_HOPS < CEILING_HOP_CAP);

// A cluster of one is a node, and a cluster of two is a pair — neither is a thing to collapse. And
// a threshold at or above the default node cap is a control that can never appear at the default,
// which is worse than no control at all: the operator would have to raise the cap to reach the
// feature that makes a large expansion readable.
const _: () = assert!(CLUSTER_COLLAPSE_THRESHOLD >= 3);
const _: () = assert!(CLUSTER_COLLAPSE_THRESHOLD < DEFAULT_NEIGHBOURHOOD_NODE_CAP);

// A zero-hop neighbourhood is the node on its own, which is not a neighbourhood; a zero-row page
// returns nothing however many times "Show more" is pressed. Both are non-values the type system
// cannot rule out, so the build does.
const _: () = assert!(DEFAULT_NEIGHBOURHOOD_HOPS >= 1);
const _: () = assert!(DEFAULT_PROFILE_PAGE_SIZE >= 1);

/// How many nodes one filtered canvas result carries (CNV-03).
///
/// A filter is not a walk: it runs over a population that is already in the browser, so this is a
/// **window on what passed**, not a bound on what was searched. The window exists because the
/// unfiltered population on the live analysis is 358,484 nodes, and a filter that passes most of
/// them would otherwise put every id back on the wire to say so.
///
/// Reported through phase 44's mandatory `Truncation` block every time it bites, for the reason
/// `CANVAS_FILTER`'s own consumers cannot check for themselves: a filtered canvas cut at the cap
/// looks exactly like a selective filter.
pub const CANVAS_FILTER_NODE_CAP: u32 = 2_000;

/// The largest visible subgraph that may be saved into an investigation in one action (CNV-03).
///
/// A save above this is **refused, never truncated**. Truncating it would be the defect this whole
/// requirement is written against — "what was saved is exactly what was visible" — reappearing one
/// level down, and the analyst would have no way to see it: a saved investigation of 500 entities
/// looks identical whether 500 was the whole set or the first 500 of four thousand.
///
/// Strictly below `CANVAS_FILTER_NODE_CAP` so a result the canvas has already windowed can never
/// be saved whole by accident.
pub const SAVE_SUBGRAPH_NODE_CAP: u32 = 500;

const _: () = assert!(SAVE_SUBGRAPH_NODE_CAP >= 1);
const _: () = assert!(SAVE_SUBGRAPH_NODE_CAP < CANVAS_FILTER_NODE_CAP);
// A filtered window narrower than one neighbourhood expansion would make the filter the thing that
// shortened the graph, silently, every time both were in use.
const _: () = assert!(CANVAS_FILTER_NODE_CAP >= CEILING_NEIGHBOURHOOD_NODE_CAP);

// ─────────────────────────────────────────────────────────────────────────────
// Phase 50: per-motif-kind default hops (D-13, roadmap open question 1) and the
// degree-cap percentile (D-14, roadmap open question 2). Joins the blocks above
// rather than starting a third module, for the same reason 45-CONTEXT D-13 gave
// the neighbourhood-expansion block: one source of truth the detail view's rail
// and the boot-time hub-set resolver both read.
// ─────────────────────────────────────────────────────────────────────────────

/// The hops a star-motif finding's detail view opens at (D-13).
///
/// **1.** A star finding's seeds (the pinned centre plus its named leaves, D-11) are already a
/// complete structural claim — the motif renderer draws the whole thing before any traversal
/// runs. One hop surfaces what those specific named entities connect to *elsewhere*, via the
/// tool rail's non-hub edge types, without re-deriving the star's own shape: `LocatedAt` is
/// excluded from [`crate::caps`]'s neighbourhood expansion by design (see
/// [`DEFAULT_NEIGHBOURHOOD_HOPS`]'s own edge-type story), not by omission, so a second hop would
/// not even walk back to the centre the way the motif already drew it.
pub const DEFAULT_HOPS_STAR: u32 = 1;

/// The hops a typed-edge-motif finding's detail view opens at (D-13).
///
/// **1.** A typed-edge finding names an officer/target pair per row (D-11) — the edges
/// themselves are the claim. One hop shows what else those same named entities touch via the
/// rail's non-hub edge types (`Represents`, `IssuedBy` excluded, same reasoning as
/// [`DEFAULT_HOPS_STAR`]), never re-deriving the finding's own edges by walking further.
pub const DEFAULT_HOPS_TYPED_EDGE: u32 = 1;

const _: () = assert!(DEFAULT_HOPS_STAR <= CEILING_NEIGHBOURHOOD_HOPS);
const _: () = assert!(DEFAULT_HOPS_TYPED_EDGE <= CEILING_NEIGHBOURHOOD_HOPS);

/// The percentile of the whole population's degree distribution a hub node is defined against
/// (D-14, roadmap open question 2).
///
/// **This is the percentile only.** The numeric degree cutoff it resolves to is corpus-dependent
/// and MUST be computed at boot time from whichever population is loaded — never hardcoded here,
/// since a fixed number mis-serves either the ~362k-node live corpus or the 82-node planted
/// fixture (measured proof in the phase 50 research notes; docs/TESTING.md's own 3x CLI-timing error is the
/// standing example of a stale hardcoded figure going unnoticed for thirteen days). See
/// the phase 50 assumptions notes for the measured distributions this value was chosen against, and
/// `crates/workbench/src/graph/csr_view.rs::degree_percentile_cutoff` for where the cutoff is
/// actually computed, under the same edge-type filter the traversal uses.
pub const DEGREE_CAP_PERCENTILE: f64 = 99.0;

const _: () = assert!(DEGREE_CAP_PERCENTILE > 0.0 && DEGREE_CAP_PERCENTILE < 100.0);

// ─────────────────────────────────────────────────────────────────────────────
// Phase 46: alert / motif triage inbox node budgets and list caps (D-01, D-03).
// Joins the blocks above rather than starting a third module, for the same
// reason the Phase 50 block gives: one source of truth the row thumbnail, the
// detail route and the server-side truncation all read — never a retyped
// literal at any call site.
// ─────────────────────────────────────────────────────────────────────────────

/// The node budget a row's motif-path thumbnail draws to (D-03).
///
/// Measured against 1,807 live alerts (phase 46 research Q2): `AddressCluster`'s induced subgraph
/// has a p90 of 6 nodes, so 8 draws the great majority of alerts whole while still bounding row
/// height against the outliers.
pub const ALERT_PATH_NODE_BUDGET_ROW: usize = 8;

/// The node budget the `/alert/:id` detail route's motif path draws to (D-03).
///
/// Matches the shipped `DETAIL_MOTIF_BUDGET` (`finding_detail_page.rs:74`) this phase's detail
/// route mirrors, per phase 46 assumption A-02 — one detail-page node budget across the app, not
/// a second number invented for this route.
pub const ALERT_PATH_NODE_BUDGET_DETAIL: usize = 20;

/// How many participant chips a row renders before collapsing the rest into a `+{n} more` label
/// (UI-SPEC row-card contract: "renders up to 6 chips and then a Label-size `+{n} more`").
pub const ALERT_ROW_PARTICIPANT_CHIPS: usize = 6;

/// The detail route's **Participants list** cap — independent of the SVG drawing budget above
/// (UI-SPEC: "coverage is not lost: the Participants list below names every participant").
///
/// Measured against the live corpus's max (`AddressCluster` max = 173 participants,
/// phase 46 research Q2) plus headroom.
pub const ALERT_PARTICIPANTS_LIST_CAP: usize = 200;

const _: () = assert!(ALERT_ROW_PARTICIPANT_CHIPS < ALERT_PATH_NODE_BUDGET_ROW);
const _: () = assert!(ALERT_PATH_NODE_BUDGET_ROW < ALERT_PATH_NODE_BUDGET_DETAIL);
const _: () = assert!(ALERT_PATH_NODE_BUDGET_DETAIL < ALERT_PARTICIPANTS_LIST_CAP);
