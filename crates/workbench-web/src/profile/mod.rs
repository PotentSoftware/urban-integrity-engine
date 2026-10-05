//! The entity profile screen (`/entity/:id`, PIV-02) and **the one profile renderer in the
//! codebase** (D-11) — the same component the canvas inspection panel mounts at `Compact` density
//! in plan 45-13.
//!
//! # The 404 policy, settled here
//!
//! Two upstream plans measured the same defect and left the decision to this one.
//!
//! * 45-11 Task 1: `RecordKind` gained `Contract` and `Location`, and contracts, planning
//!   applications and addresses now carry the graph id of the node they are about — **five** row
//!   classes carry a `pivot`, against 45-08's two.
//! * But `crates/core::persistence::queries::query_entity_by_id` resolves `NodeWeight::Entity` and
//!   `NodeWeight::OffshoreEntity` **only**, so `GET /api/entity/profile?id=CF:…` (or `planning:`,
//!   `location:`, `LOC:`) 404s. Measured on the live analysis 2026-09-15: zero rows in the
//!   projected `entity` table carry any of those four prefixes.
//!
//! **This plan renders only what opens.** [`pivot::opens_a_profile`] is the whole policy: a pivot whose
//! `kind` is `"Entity"` or `"OffshoreEntity"` renders as an anchor; every other kind renders as
//! plain text, exactly as the phase 45 UI spec already requires ("targets whose kind has no profile
//! route yet render as plain text, not a dead link").
//!
//! Widening the read path was considered and rejected as the *worse* of the two honest answers,
//! for a reason that is about this screen rather than about effort. A `Location` node has no
//! officers, no contracts, no status and no Companies House number; assembling one into
//! [`EntityProfileDto`] would produce a page on which fourteen of sixteen sections render
//! "No officers recorded for this entity." about an *address*. That is not an empty section — it
//! is a wrong answer rendered as a sentence, which is precisely the failure the three-way
//! empty-state rule exists to prevent. A contract and a location want their own subject types and
//! their own page; that is a new read path and a new page (deviation Rule 4), not something to
//! drag in behind a renderer.
//!
//! The consequence is stated rather than hidden: the pivot surface the read path *can supply* is
//! five row classes wide, and the pivot surface this screen *ships as anchors* is narrower, because
//! it is filtered per row by the target's node class rather than per row type. The filter is
//! per-row deliberately — an OSINT link pointing at a `Charity` node and one pointing at an
//! `Entity` node arrive in the same section, and only the second one opens.
//! `db_a_pivot_id_names_a_node_the_profile_route_cannot_open_yet`
//! (`crates/core/tests/profile_pivot_ids.rs`) is left **passing**: the gap it pins is real and this
//! plan did not close it.
//!
//! # The as-at date persists across a pivot (the phase 45 UI spec Open Question 2, settled)
//!
//! Persist, and persist *structurally*: the date lives in the URL (`/entity/{id}?as_at=…`), and
//! [`pivot::pivot_href`] carries it onto every anchor this screen renders. An analyst investigating a date
//! window does not re-set it at every hop, the banner keeps it visible so the state cannot be
//! silently carried, and the back button restores it with the page because it was never anywhere
//! else. Plan 45-12 builds the control; the control's whole job is to set that one query
//! parameter, and it inherits persistence rather than implementing it.
//!
//! # The section index is a chip row at every width (the phase 45 UI spec Open Question 1, settled)
//!
//! The spec's sticky 240px rail at >=1280px is **not built**, and the horizontal chip row it would
//! collapse to below that width is shipped at every width instead. The checker's own finding is the
//! reason: `grep -rn "@media"` across `crates/workbench-web` returns nothing, so 1280 would be the
//! first responsive rule in the workbench (the phase 44 UI spec declared 900px for the graph page and it
//! was never built either). A responsive rule in this crate is also the one kind of behaviour its
//! test model cannot reach — every test here is a host-side pure-function or source-reading test,
//! with no DOM and no headless browser (phase 42 research Pitfall 10) — so it would ship untested
//! and silently rot. The chip row costs one scan line and tells the analyst the same thing.
//!
//! # Testing approach
//!
//! Plain-function-first, the convention `tool_rail.rs` / `graph_results_list.rs` / `disambiguation.rs`
//! document: every string this screen renders is built by a plain function over owned DTO values,
//! host-testable with no DOM. The components are thin and their reachability is proved by
//! source-reading tests in `tests/abbr_reachable.rs` — Phase 43 shipped a fully unit-tested `Abbr`
//! with zero call sites, which is the failure that style of test exists to catch.
#![allow(dead_code)]

pub mod as_at;
pub mod pivot;
pub mod provenance_chip;
pub mod sections;

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_params_map, use_query_map};
use urban_integrity_workbench_dto::{
    EntityProfileDto, EntityRef, NULL_RANGE_RULE, UNDATED_ROWS_ARE,
};

use crate::breadcrumb::Breadcrumb;
use crate::copy_button::CopyButton;
use crate::disambiguation::{status_label, EntitySearch};
use crate::graph_deep_link::graph_href_for_refs;
use crate::investigation::pin::PinControl;
use crate::investigation::pin_origin_for;
use crate::profile::as_at::AsAtControl;
use crate::profile::provenance_chip::compact_provenance_summary;
use crate::profile::sections::{ProfileSections, SectionIndex};

/// The phase 45 UI spec Copy Deck, "Loading — profile". Mirrors the Analysis page's
/// "Loading the last probe run…" and the Graph page's "Loading the graph…".
pub const PROFILE_LOADING: &str = "Loading the profile…";

/// The phase 45 UI spec Copy Deck, "Error — profile load failure". The endpoint is named in the copy
/// because an analyst reporting this is reporting a specific route.
pub const PROFILE_LOAD_FAILURE: &str =
    "The workbench returned data this page cannot read (/api/entity/profile).";

/// The retry affordance beside [`PROFILE_LOAD_FAILURE`].
pub const PROFILE_RETRY: &str = "Retry";

/// The copy for a request the workbench **refused** — a 400, not a 500.
///
/// Authored by plan 45-16; the phase 45 UI spec's copy deck has no row for it, because until that plan
/// the state did not exist on this side of the wire. Recorded in the plan 45-16 summary.
///
/// # Why this is not [`PROFILE_LOAD_FAILURE`], and not the server's sentence either
///
/// The two sentences answer different questions and only one of them can be answered from each
/// side of the wire. *What kind of thing happened, and what do I do now* is a fact about this
/// page: there is an address bar, the analyst typed into it, and reloading an unchanged bad URL
/// will reproduce the refusal exactly — which is why this state carries no [`PROFILE_RETRY`].
/// The server cannot know any of that. *Which value was wrong* is a fact only the server holds,
/// and it is rendered verbatim underneath (see [`rejection_detail`]) rather than paraphrased
/// here, so the two never drift.
///
/// What is not defensible — and was the defect plan 45-12 §6.4 found and 45-16 fixed — is
/// rendering [`PROFILE_LOAD_FAILURE`] for this case: a sentence about the workbench, for a
/// mistake the analyst made and can correct, on the one screen whose whole subject is telling
/// three emptinesses and two failure modes apart.
pub const PROFILE_REQUEST_REJECTED: &str =
    "The workbench rejected this request (/api/entity/profile). Nothing is wrong with this page — \
     correct the address and load it again.";

/// The header block's deep link into the canvas, reusing 44-21's `/graph?ids=` form.
pub const OPEN_ON_GRAPH: &str = "Open on graph";

/// The `/entity` search page's heading and help line. `/entity` exists because
/// the phase 45 UI spec's own breadcrumb contract for this page is `Home / Entity / {name}` — the middle
/// segment names a surface, and PIV-01's list needs a page to open on that is not already a
/// profile.
pub const ENTITY_SEARCH_HEADING: &str = "Find an entity";
/// See [`ENTITY_SEARCH_HEADING`].
pub const ENTITY_SEARCH_PLACEHOLDER: &str = "Name, company number or graph id";

/// The phase 45 UI spec Copy Deck, "Error — entity not found". The workbench's own 404 body carries this
/// same sentence (`crates/workbench/src/entity/mod.rs`), so the page and the API agree by
/// construction rather than by review.
pub fn entity_not_found(id: &str) -> String {
    format!("No entity with id {id} exists in this analysis.")
}

/// The persistent as-at banner (the phase 45 UI spec Copy Deck, "As-at banner (active)").
///
/// The bracketed word is [`UNDATED_ROWS_ARE`], which `workbench-dto`'s `temporal` module fixes
/// from the same place the read path applies the rule — never retyped here, so the sentence on
/// screen cannot drift out of agreement with the filter's behaviour.
pub fn as_at_banner(as_at: &str) -> String {
    format!(
        "Showing this entity as at {as_at}. Facts with no recorded date range are \
         {UNDATED_ROWS_ARE} — see below."
    )
}

// The pivot-link vocabulary lives in `profile/pivot.rs` — plan 45-11 Task 2 moved it there because
// it gained callers outside the profile screen: the Analysis page's evidence refs and the Graph
// page's results pivot through the same functions, via `crate::entity_ref_view::EntityRefView`.
// Three copies of "which kind opens" would be three chances for one click to 404 on one surface
// and open on another. Imported, never re-exported: this is a binary crate, so a `pub use` nobody
// reads through is dead code rather than an API.
use pivot::{encode_segment, profile_href, response_is_still_wanted, ProfileRoute};

/// **What the banner's "see below" points at**: plan 45-06's null-range rule, in the words the read
/// path applies it from, rendered above the sections when a date is set.
///
/// The banner is a summary — it says undated facts are [`UNDATED_ROWS_ARE`] and defers the reason.
/// Until this plan nothing rendered the reason, so the promise dangled: `NULL_RANGE_RULE` reached
/// `crates/workbench-web` in no source at all, and an analyst reading "see below" found a shorter
/// list and no explanation of it. That is the failure PIV-04 names, arrived at through a missing
/// sentence rather than a wrong filter.
///
/// Returned rather than inlined so a test can assert the screen states the constant **verbatim**.
/// 45-06's instruction was explicit: render it, do not paraphrase, do not retype the word.
pub fn as_at_rule_statement(as_at: Option<&str>) -> Option<&'static str> {
    as_at.filter(|d| !d.is_empty()).map(|_| NULL_RANGE_RULE)
}

/// The header block's `{kind} · {status}` line. The status words are
/// [`crate::disambiguation::status_label`] — `crates/cli`'s `format_entity_status` strings
/// character for character — because D-01 says the CLI and the workbench must not disagree about
/// one company, and 45-09 left that function here for this page to call rather than respell.
pub fn header_kind_and_status(profile: &EntityProfileDto) -> String {
    format!("{} · {}", profile.kind, status_label(&profile.status.value))
}

/// The `/graph?ids=…` deep link for the subject, or `None` when the subject's class is not one the
/// canvas deep link can name (`graph_deep_link::node_id_for_ref` maps `Entity` and `CompanyNumber`
/// and refuses to invent a node id for anything else).
pub fn open_on_graph_href(profile: &EntityProfileDto) -> Option<String> {
    graph_href_for_refs(&[("Entity".to_string(), profile.id.clone())])
}

/// Which of the two densities the renderer is mounted at (D-11).
///
/// **There is exactly one profile renderer**, and this is the parameter that makes that possible.
/// The inspection panel replaces the Phase 44 side card's body, keeping its 280px frame; writing a
/// second renderer for the narrow case is the failure the contract exists to prevent, and this
/// project's own Cosmograph source/bundle drift is the worked example of what two copies of one
/// thing cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Density {
    /// The `/entity/:id` page: every section, tables, paging, provenance chips.
    Full,
    /// The canvas inspection panel at 280px (plan 45-13): header block, identifiers, and the first
    /// [`COMPACT_ROWS_PER_SECTION`] rows of each non-empty section with its total.
    Compact,
}

/// How many rows of each non-empty section `Density::Compact` shows (the phase 45 UI spec: "the first 3
/// rows of each non-empty section with its total").
pub const COMPACT_ROWS_PER_SECTION: usize = 3;

/// `GET /api/entity/profile` — the URL for one profile load or one **Show more** press.
///
/// `section`/`cursor` are sent together or not at all: a cursor names the section it was issued
/// for, and the handler rejects one handed to a different section rather than silently paging a
/// list the analyst is not looking at.
pub fn profile_api_url(
    id: &str,
    as_at: Option<&str>,
    section: Option<&str>,
    cursor: Option<&str>,
) -> String {
    let mut url = format!("/api/entity/profile?id={}", encode_segment(id));
    if let Some(date) = as_at.filter(|d| !d.is_empty()) {
        url.push_str(&format!("&as_at={}", encode_segment(date)));
    }
    if let (Some(section), Some(cursor)) = (section, cursor) {
        url.push_str(&format!(
            "&section={}&cursor={}",
            encode_segment(section),
            encode_segment(cursor)
        ));
    }
    url
}

/// What one profile load resolved to.
///
/// `NotFound` is its own variant rather than an `Err` carrying a 404: the phase 45 UI spec gives the two
/// states different copy and different chrome (the not-found state still renders the breadcrumb),
/// and an analyst who pasted an id that does not exist has a different next move from one whose
/// browser could not read the response.
#[derive(Debug, Clone, PartialEq)]
pub enum ProfileLoad {
    NotFound,
    /// The workbench **refused** the request (400). A third answer with a third next move: fix
    /// what you typed. Carries the server's own sentence, which names the value that was wrong —
    /// rendered under [`PROFILE_REQUEST_REJECTED`], never in place of it.
    ///
    /// The string is carried rather than discarded for the same reason [`Self::Failed`] carries
    /// one, and unlike `Failed` it is also **read**: `crates/workbench` returns
    /// `{"error": "`as_at` must be an ISO-8601 date, got …"}`, and the value in that sentence is
    /// the only part of this state the client cannot derive.
    Rejected(String),
    Failed(String),
    Loaded(Box<EntityProfileDto>),
}

/// What a non-2xx response to a profile request means, decided from the **status alone**.
///
/// A separate type and a pure function for the same reason `graph_page.rs` has
/// `population_error_for_status`: the boundary rule is the thing under test, and a rule expressed
/// inside an `async fn` that needs a live server is a rule nothing in the ungated suite can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileFailure {
    /// 404 — an answer, not an error. No entity in this analysis has that id.
    NotFound,
    /// 400 — the request itself was malformed. The analyst can correct it; the workbench is
    /// working. Reachable only from a pasted or hand-edited address: `<input type="date">` emits
    /// either an empty string or an ISO-8601 date and can produce no other value.
    Rejected,
    /// Anything else — the page could not read what came back, and the analyst can do nothing
    /// about it but retry.
    Unreadable,
}

/// See [`ProfileFailure`]. The 4xx/5xx split is deliberately **not** a range test: 401, 403 and
/// 429 are all client-class statuses that this route cannot produce and that an analyst could not
/// fix by editing the address, so folding them in with 400 would put a "correct the address"
/// instruction under a failure no address can correct.
#[must_use]
pub fn profile_failure_for_status(status: u16) -> ProfileFailure {
    match status {
        404 => ProfileFailure::NotFound,
        400 => ProfileFailure::Rejected,
        _ => ProfileFailure::Unreadable,
    }
}

/// The server's sentence, when there is one worth rendering.
///
/// `None` rather than a fallback sentence: [`PROFILE_REQUEST_REJECTED`] is true on its own, and a
/// manufactured second line ("no reason given") would add a sentence that says nothing while
/// looking like it says something. A 400 whose body is not this project's `{"error": …}` shape —
/// axum's own `Query` rejection is plain text, not JSON — lands here as a blank and renders as
/// one line.
#[must_use]
pub fn rejection_detail(server_sentence: &str) -> Option<&str> {
    let trimmed = server_sentence.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

/// Fetches one profile. A 404 is [`ProfileLoad::NotFound`], not an error — it is an answer; a 400
/// is [`ProfileLoad::Rejected`], which is a third one. See [`profile_failure_for_status`].
pub async fn fetch_profile(
    id: String,
    as_at: Option<String>,
    section: Option<String>,
    cursor: Option<String>,
) -> ProfileLoad {
    let url = profile_api_url(&id, as_at.as_deref(), section.as_deref(), cursor.as_deref());
    let response = match gloo_net::http::Request::get(&url).send().await {
        Ok(response) => response,
        Err(err) => return ProfileLoad::Failed(err.to_string()),
    };
    let status = response.status();
    if !response.ok() {
        return match profile_failure_for_status(status) {
            ProfileFailure::NotFound => ProfileLoad::NotFound,
            // `server_error_text` is the investigation module's reader, reused rather than
            // copied: one place in this crate knows that an error body is `{"error": …}`, so a
            // change to the API's error shape has one site to find.
            ProfileFailure::Rejected => {
                ProfileLoad::Rejected(crate::investigation::server_error_text(response, "").await)
            }
            ProfileFailure::Unreadable => {
                ProfileLoad::Failed(format!("entity profile responded with status {status}"))
            }
        };
    }
    match response.json::<EntityProfileDto>().await {
        Ok(dto) => ProfileLoad::Loaded(Box::new(dto)),
        Err(err) => ProfileLoad::Failed(err.to_string()),
    }
}

/// Appends one **Show more** response's window to the profile already on screen.
///
/// The handler's contract makes this the only correct move: a `section=`/`cursor=` request returns
/// *that* section's next window (rows 20..40, not 0..40) and every other section at its first
/// window. Replacing the profile wholesale would therefore make a Show more press **lose** the
/// rows the analyst was already reading, while the caption went on reporting a total that no
/// longer described what was on screen.
///
/// Returns `false` when `section` names nothing this profile pages — the handler rejects such a
/// cursor rather than applying it to another list, and this side does the same rather than
/// silently merging into whichever section happened to be first.
pub fn merge_section(
    current: &mut EntityProfileDto,
    incoming: EntityProfileDto,
    section: &str,
) -> bool {
    macro_rules! append {
        ($field:ident) => {{
            current.$field.rows.extend(incoming.$field.rows);
            current.$field.next_cursor = incoming.$field.next_cursor;
            current.$field.total = incoming.$field.total;
            true
        }};
    }
    macro_rules! append_findings {
        ($field:ident) => {{
            current
                .findings
                .$field
                .rows
                .extend(incoming.findings.$field.rows);
            current.findings.$field.next_cursor = incoming.findings.$field.next_cursor;
            current.findings.$field.total = incoming.findings.$field.total;
            true
        }};
    }
    match section {
        "officers" => append!(officers),
        "addresses" => append!(addresses),
        "contracts" => append!(contracts),
        "planning_applications" => append!(planning_applications),
        "property" => append!(property),
        "osint_links" => append!(osint_links),
        "beneficial_owners" => append!(beneficial_owners),
        "disqualifications" => append!(disqualifications),
        "trustee_of" => append!(trustee_of),
        "donates_to" => append!(donates_to),
        "offshore_links" => append!(offshore_links),
        "related" => append!(related),
        "findings" => append_findings!(findings),
        "alerts" => append_findings!(alerts),
        "osint_alerts" => append_findings!(osint_alerts),
        _ => false,
    }
}

/// The `/entity` search surface: NAV-06's picker, PIV-01's list, and nothing else.
///
/// It exists so that a multi-match search has somewhere to render its list that is not already a
/// profile, and so that `Home / Entity / {name}` has an `Entity` to link to. It opens a profile
/// only through [`crate::disambiguation::EntitySearch`]'s `on_open`, which
/// `disambiguation::apply_outcome` alone invokes — the single choke-point PIV-01's load-bearing
/// clause depends on.
#[component]
#[allow(non_snake_case)]
pub fn EntitySearchPage() -> impl IntoView {
    // `use_navigate`, not `location.assign`. 45-10 chose the full document navigation deliberately
    // and left the choice to this plan's Task 3; the client-side one is what makes this page's
    // `on_open` behave like every pivot anchor on the profile — one `pushState`, one history entry,
    // one wasm instance — rather than like a fresh page load that happens to end up in the same
    // place. `use_navigate` must be called in the component body, where the `<Router>` context is.
    let navigate = use_navigate();
    let open = move |entity: EntityRef| {
        navigate(&profile_href(&entity.id, None), Default::default());
    };

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-lg); padding-top: var(--space-md); max-width: 960px;">
            <Breadcrumb />
            <h1 class="text-display" style="margin: 0;">{ENTITY_SEARCH_HEADING}</h1>
            <EntitySearch placeholder=ENTITY_SEARCH_PLACEHOLDER on_open=open />
        </div>
    }
}

/// `/entity/:id` — PIV-02's screen.
#[component]
#[allow(non_snake_case)]
pub fn EntityProfilePage() -> impl IntoView {
    let params = use_params_map();
    let query = use_query_map();
    // **The screen's whole state, read from the router on every render and stored nowhere else.**
    // That is what makes back and forward correct: a `popstate` hands the router a URL, both halves
    // of `ProfileRoute` fall out of it, and the effect below re-runs. A copy of the as-at date in a
    // signal would be a second source of truth that the back button could not reach — the failure
    // PIV-03's "its previous as-at setting" clause names.
    let route = move || {
        ProfileRoute::new(
            params.read().get("id").unwrap_or_default(),
            query.read().get("as_at"),
        )
    };
    let id = move || route().id;
    let as_at = move || route().as_at;

    let state: RwSignal<Option<ProfileLoad>> = RwSignal::new(None);
    let reload = RwSignal::new(0u32);

    // Re-fetches when the route id changes, when the as-at date changes, and when Retry bumps the
    // counter. The as-at date is read from the URL for the same reason `profile_href` writes it
    // there: one source of truth for a filter whose whole hazard is being forgotten.
    Effect::new(move |_| {
        let _ = reload.get();
        let requested = route();
        if requested.id.is_empty() {
            return;
        }
        state.set(None);
        let for_request = requested.clone();
        leptos::task::spawn_local(async move {
            let loaded = fetch_profile(
                for_request.id.clone(),
                for_request.as_at.clone(),
                None,
                None,
            )
            .await;
            // Client-side routing lets two of these race — see `response_is_still_wanted`. Read
            // the route again, untracked (this is not a reactive context and must not subscribe),
            // and drop a response the analyst has already navigated away from.
            let current = ProfileRoute::new(
                params.get_untracked().get("id").unwrap_or_default(),
                query.get_untracked().get("as_at"),
            );
            if response_is_still_wanted(&for_request, &current) {
                state.set(Some(loaded));
            }
        });
    });

    // One **Show more** press: fetch that section's next window and append it to what is on
    // screen. `merge_section` is where the append happens and why replacing would be wrong.
    let on_show_more = Callback::new(move |(section, cursor): (String, String)| {
        let id = id();
        let as_at = as_at();
        let section_for_merge = section.clone();
        leptos::task::spawn_local(async move {
            if let ProfileLoad::Loaded(incoming) =
                fetch_profile(id, as_at, Some(section), Some(cursor)).await
            {
                state.update(|current| {
                    if let Some(ProfileLoad::Loaded(profile)) = current {
                        merge_section(profile, *incoming, &section_for_merge);
                    }
                });
            }
        });
    });

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-lg); padding-top: var(--space-md); max-width: 960px;">
            {move || {
                let trailing = match state.get() {
                    Some(ProfileLoad::Loaded(profile)) => profile.name.clone(),
                    _ => String::new(),
                };
                view! { <Breadcrumb trailing=trailing /> }
            }}
            {move || match state.get() {
                None => view! {
                    <p class="text-body" style="margin: 0; color: var(--muted-foreground);">
                        {PROFILE_LOADING}
                    </p>
                }.into_any(),
                Some(ProfileLoad::NotFound) => view! {
                    <p class="text-body" style="margin: 0; color: var(--destructive);">
                        {entity_not_found(&id())}
                    </p>
                }.into_any(),
                // No Retry: reloading an unchanged address reproduces the refusal exactly, so a
                // Retry control here would be an affordance that is guaranteed not to work. The
                // recovery is in the address bar, which is what the copy says.
                Some(ProfileLoad::Rejected(reason)) => view! {
                    <div style="display: flex; flex-direction: column; gap: var(--space-xs); align-items: flex-start;">
                        <p class="text-body" style="margin: 0; color: var(--destructive);">
                            {PROFILE_REQUEST_REJECTED}
                        </p>
                        {rejection_detail(&reason).map(|detail| {
                            let detail = detail.to_string();
                            view! {
                                <p class="text-body" style="margin: 0; color: var(--muted-foreground);">
                                    {detail}
                                </p>
                            }
                        })}
                    </div>
                }.into_any(),
                Some(ProfileLoad::Failed(_)) => view! {
                    <div style="display: flex; flex-direction: column; gap: var(--space-sm); align-items: flex-start;">
                        <p class="text-body" style="margin: 0; color: var(--destructive);">
                            {PROFILE_LOAD_FAILURE}
                        </p>
                        <button
                            type="button"
                            style="min-height: 44px; padding: 0 var(--space-md); background: var(--primary); border: none; border-radius: 6px; color: var(--primary-foreground); cursor: pointer;"
                            on:click=move |_| reload.update(|n| *n += 1)
                        >
                            {PROFILE_RETRY}
                        </button>
                    </div>
                }.into_any(),
                Some(ProfileLoad::Loaded(profile)) => view! {
                    <ProfileRenderer
                        profile=*profile
                        density=Density::Full
                        on_show_more=on_show_more
                    />
                }.into_any(),
            }}
        </div>
    }
}

/// The header block: name, kind and status, the copyable id, the as-at banner, the pin control and
/// the canvas deep link (the phase 45 UI spec "Header block").
///
/// The **pin control is here at both densities** (plan 45-15), and that is the whole of
/// the phase 45 UI spec's "pin control in the profile header **and** on the canvas inspection panel":
/// there is one profile renderer (D-11), the panel mounts it at [`Density::Compact`], and the only
/// difference a pin taken there has is its origin, which `pin_origin_for` derives from the density.
/// Unlike the as-at control below it — which navigates to `/entity/:id` and would be an *exit* from
/// the canvas — a pin writes a row and stays where it is.
#[component]
#[allow(non_snake_case)]
fn ProfileHeader(profile: EntityProfileDto, density: Density) -> impl IntoView {
    let name = profile.name.clone();
    let kind_and_status = header_kind_and_status(&profile);
    let id = profile.id.clone();
    let id_for_copy = profile.id.clone();
    let graph_href = open_on_graph_href(&profile);
    let pin_id = profile.id.clone();
    // The window is carried to the control so it can say the window is **not** saved with the pin,
    // and for no other reason: it never reaches the pin's identity (`entity_pin_target`).
    let pin_as_at = profile.as_at.clone();
    let banner = profile.as_at.as_deref().map(as_at_banner);
    // The control is `Density::Full` only. 45-13 mounts this same renderer inside the canvas's
    // 280px side card, where a control that navigates away from `/graph` to `/entity/:id` would be
    // an exit rather than a filter — and the panel's own frame is not the "page header"
    // the phase 45 UI spec places this in.
    let control = (density == Density::Full).then(|| (profile.id.clone(), profile.as_at.clone()));
    let name_class = match density {
        Density::Full => "text-display",
        Density::Compact => "text-heading",
    };
    // `Compact` shows no per-row chips, so the panel states its provenance once, as a count
    // (the phase 45 UI spec; 45-04 re-checked the assumption behind it and kept the count).
    let compact_provenance =
        (density == Density::Compact).then(|| compact_provenance_summary(&profile));

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-sm);">
            <h1 class=name_class style="margin: 0;">{name}</h1>
            <span class="text-body" style="color: var(--muted-foreground);">{kind_and_status}</span>
            {compact_provenance.map(|summary| view! {
                <span class="text-label" style="color: var(--muted-foreground);">{summary}</span>
            })}
            <span style="display: inline-flex; align-items: center; gap: var(--space-xs); flex-wrap: wrap;">
                <span class="font-mono text-label" style="color: var(--muted-foreground); overflow-wrap: anywhere;">{id}</span>
                <CopyButton value=id_for_copy />
            </span>
            {graph_href.map(|href| view! {
                <a class="text-body" href=href style="min-height: 44px; display: inline-flex; align-items: center; color: var(--primary);">
                    {OPEN_ON_GRAPH}
                </a>
            })}
            <PinControl entity_id=pin_id origin=pin_origin_for(density) as_at=pin_as_at />
            {control.map(|(id, as_at)| view! { <AsAtControl id=id as_at=as_at /> })}
            {banner.map(|text| view! {
                <p
                    class="text-body"
                    role="status"
                    style="margin: 0; padding: var(--space-sm) var(--space-md); border-radius: 6px; background: color-mix(in srgb, var(--warning) 15%, transparent); border: 1px solid color-mix(in srgb, var(--warning) 30%, transparent); color: var(--foreground);"
                >
                    {text}
                </p>
            })}
        </div>
    }
}

/// **The one profile renderer** (D-11). `/entity/:id` mounts it at [`Density::Full`]; plan 45-13's
/// canvas inspection panel mounts this same component at [`Density::Compact`] inside the existing
/// 280px side-card frame.
#[component]
#[allow(non_snake_case)]
pub fn ProfileRenderer(
    profile: EntityProfileDto,
    density: Density,
    /// `None` in the inspection panel: `Compact` shows the first rows of each section and pivots
    /// to the full profile for the rest, so it has no **Show more** to wire.
    #[prop(optional)]
    on_show_more: Option<Callback<(String, String)>>,
) -> impl IntoView {
    let header_profile = profile.clone();
    let index_profile = profile.clone();
    // Once per page, above the sections, because that is where the banner's "see below" points and
    // because the rule is about the whole screen. The per-section counts that follow say how many
    // rows it applies to *here*; this says what it means.
    let rule = as_at_rule_statement(profile.as_at.as_deref());

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-lg);">
            <ProfileHeader profile=header_profile density=density />
            {(density == Density::Full).then(|| view! {
                <SectionIndex profile=index_profile />
            })}
            {rule.map(|text| view! {
                <p class="text-body" role="note" style="margin: 0; color: var(--muted-foreground);">
                    {text}
                </p>
            })}
            <ProfileSections profile=profile density=density on_show_more=on_show_more />
        </div>
    }
}

// Declared here, at the foot of the file, and not beside `pub mod sections` at the top: the
// glossary sweep in `tests/abbr_reachable.rs` reads each shipped source only as far as its first
// `#[cfg(test)]` attribute, so a test-only module declared early makes every line after it
// invisible to the sweep. That hole is latent across the whole crate (no existing source has a
// `#[cfg(test)]` item before its trailing test module); `shipped_portion` is hardened in the same
// change that registers this module, and this declaration stays down here regardless.
#[cfg(test)]
pub mod test_fixtures;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::test_fixtures::sample_profile;

    // The four pivot-href tests that used to sit here moved with their functions to
    // `profile/pivot.rs` in plan 45-11 Task 2 — see this module's `pub use pivot::{…}` for why the
    // functions did.

    /// **A refusal and a fault are different answers, and the status is where they part.**
    ///
    /// Plan 45-12 §6.4 measured the defect this pins: every non-404 failure became
    /// `ProfileLoad::Failed`, so a typo in the address bar rendered a sentence about the
    /// workbench. The boundary rule is a pure function precisely so this test needs no server.
    ///
    /// PROVE THIS CAN FAIL: make `profile_failure_for_status`'s `400` arm return
    /// `ProfileFailure::Unreadable` — i.e. restore the defect. Run for real.
    #[test]
    fn a_rejected_request_is_not_a_workbench_fault() {
        assert_eq!(profile_failure_for_status(400), ProfileFailure::Rejected);
        assert_eq!(profile_failure_for_status(404), ProfileFailure::NotFound);
        for status in [500, 502, 503, 504] {
            assert_eq!(
                profile_failure_for_status(status),
                ProfileFailure::Unreadable,
                "a {status} is the workbench's fault and must keep the retryable copy"
            );
        }
        // The client-class statuses this route cannot produce must NOT inherit "correct the
        // address": none of them is correctable from the address bar.
        for status in [401, 403, 405, 409, 422, 429] {
            assert_eq!(
                profile_failure_for_status(status),
                ProfileFailure::Unreadable,
                "{status} is not an address the analyst can correct"
            );
        }
        // Non-vacuity: the three outcomes are genuinely three, not one name repeated.
        let outcomes = std::collections::BTreeSet::from([
            format!("{:?}", profile_failure_for_status(400)),
            format!("{:?}", profile_failure_for_status(404)),
            format!("{:?}", profile_failure_for_status(500)),
        ]);
        assert_eq!(outcomes.len(), 3, "three statuses must give three answers");
    }

    /// **The rejection copy blames the request, never the workbench, and offers no Retry.**
    ///
    /// PROVE THIS CAN FAIL: set `PROFILE_REQUEST_REJECTED = PROFILE_LOAD_FAILURE` — which is
    /// exactly the shipped behaviour before this plan. Run for real.
    #[test]
    fn the_rejection_copy_names_the_analyst_s_next_move() {
        assert_ne!(
            PROFILE_REQUEST_REJECTED, PROFILE_LOAD_FAILURE,
            "a refused request must not render the load-failure sentence"
        );
        assert!(
            !PROFILE_REQUEST_REJECTED.contains("this page cannot read"),
            "the refusal copy must not describe the page as unable to read a response"
        );
        assert!(
            PROFILE_REQUEST_REJECTED.contains("address"),
            "the refusal copy must name where the correction is made: {PROFILE_REQUEST_REJECTED}"
        );
        // Non-vacuity: a blank constant would satisfy the two negatives above.
        assert!(PROFILE_REQUEST_REJECTED.len() > 40);
    }

    /// **The server's sentence is rendered when there is one and nothing is invented when there
    /// is not.**
    ///
    /// The blank case is real: axum's own `Query` rejection answers 400 with plain text, not this
    /// project's `{"error": …}` object, so `server_error_text` returns the empty fallback.
    ///
    /// PROVE THIS CAN FAIL: have `rejection_detail` return `Some(server_sentence)`
    /// unconditionally. Run for real.
    #[test]
    fn a_reasonless_rejection_renders_one_line_rather_than_an_empty_one() {
        assert_eq!(rejection_detail(""), None);
        assert_eq!(rejection_detail("   "), None);
        assert_eq!(rejection_detail("\n\t "), None);
        assert_eq!(
            rejection_detail("`as_at` must be an ISO-8601 date, got \"2021-13-45\""),
            Some("`as_at` must be an ISO-8601 date, got \"2021-13-45\"")
        );
        assert_eq!(rejection_detail("  trimmed  "), Some("trimmed"));
    }

    /// **The refusal state is mounted, and it is mounted without a Retry control.**
    ///
    /// A source-reading test because this crate has no DOM (see the module doc's "Testing
    /// approach"): a variant can exist, be unit-tested, and never be matched in the view — which
    /// is the failure `tests/abbr_reachable.rs` exists for, one level up.
    ///
    /// PROVE THIS CAN FAIL: delete the `Some(ProfileLoad::Rejected(reason))` arm from the view.
    /// Run for real.
    #[test]
    fn the_refusal_state_is_rendered_and_carries_no_retry() {
        let source = include_str!("mod.rs");
        let shipped = source
            .split_once("#[cfg(test)]")
            .expect("this file has a test module")
            .0;
        let arm = shipped
            .find("Some(ProfileLoad::Rejected(")
            .expect("the refusal state must be matched in the view, not merely defined");
        assert!(
            shipped[arm..].contains("PROFILE_REQUEST_REJECTED"),
            "the refusal arm must render the refusal copy"
        );
        let next_arm = shipped[arm..]
            .find("Some(ProfileLoad::Failed(")
            .expect("the failure arm follows the refusal arm");
        assert!(
            !shipped[arm..arm + next_arm].contains("PROFILE_RETRY"),
            "retrying an unchanged address reproduces the refusal — no Retry belongs here"
        );
        // Non-vacuity: the failure arm DOES carry one, so the absence above is a real difference
        // and not a sweep that could never find the token anywhere.
        assert!(
            shipped[arm + next_arm..].contains("PROFILE_RETRY"),
            "non-vacuity: the load-failure arm must still offer Retry"
        );
    }

    #[test]
    fn the_profile_url_sends_a_cursor_only_alongside_its_section() {
        assert_eq!(
            profile_api_url("GB-COH:X6999001", None, None, None),
            "/api/entity/profile?id=GB-COH%3AX6999001"
        );
        assert_eq!(
            profile_api_url("GB-COH:X6999001", Some("2021-06-30"), None, None),
            "/api/entity/profile?id=GB-COH%3AX6999001&as_at=2021-06-30"
        );
        assert_eq!(
            profile_api_url(
                "GB-COH:X6999001",
                None,
                Some("officers"),
                Some("officers@20")
            ),
            "/api/entity/profile?id=GB-COH%3AX6999001&section=officers&cursor=officers%4020"
        );
        // A cursor with no section names nothing the handler would accept, so it is not sent.
        assert_eq!(
            profile_api_url("GB-COH:X6999001", None, None, Some("officers@20")),
            "/api/entity/profile?id=GB-COH%3AX6999001"
        );
    }

    #[test]
    fn the_banner_states_the_null_range_rule_from_the_dto_not_from_a_retyped_word() {
        let banner = as_at_banner("2021-06-30");
        assert!(banner.starts_with("Showing this entity as at 2021-06-30."));
        assert!(
            banner.contains(UNDATED_ROWS_ARE),
            "the bracketed word must come from workbench-dto: {banner}"
        );
    }

    /// **The banner's "see below" is answered, and answered verbatim.**
    ///
    /// The banner defers: it says undated facts are `{included}` and points elsewhere for why.
    /// Until this plan nothing rendered the why — `NULL_RANGE_RULE` appeared in no
    /// `crates/workbench-web` source at all — so the analyst who followed the pointer found a list
    /// and no explanation of it. 45-06's hand-over was explicit that the screen renders the
    /// constant rather than paraphrasing it, so the sentence on screen cannot drift out of
    /// agreement with the filter's behaviour.
    ///
    /// PROVE THIS CAN FAIL: have `as_at_rule_statement` return a paraphrase — anything but
    /// `NULL_RANGE_RULE` itself — and the equality fails. Have it return the rule with no date set
    /// and the unfiltered assertion fails.
    #[test]
    fn the_banners_see_below_is_answered_by_the_rule_the_read_path_applies() {
        let banner = as_at_banner("2021-06-30");
        assert!(
            banner.contains("see below"),
            "the banner promises the rule is stated below it: {banner}"
        );

        let stated = as_at_rule_statement(Some("2021-06-30")).expect("a filtered screen states it");
        assert_eq!(
            stated, NULL_RANGE_RULE,
            "verbatim, from workbench-dto — 45-06: do not paraphrase, do not retype the word"
        );
        // Non-vacuity: the constant is a real sentence that answers the banner's deferral, rather
        // than an empty string that would satisfy the equality above.
        assert!(stated.contains(UNDATED_ROWS_ARE));
        assert!(stated.contains("could not test"));
        assert!(!stated.contains("excluded"));

        // An unfiltered screen states no rule, because no rule was applied to anything. A
        // permanent notice would describe a filter nobody ran — the same defect as a control
        // labelled with a date it is not filtering to.
        assert_eq!(as_at_rule_statement(None), None);
        assert_eq!(as_at_rule_statement(Some("")), None);
    }

    #[test]
    fn the_header_states_the_kind_and_the_cli_status_vocabulary() {
        let profile = sample_profile();
        assert_eq!(
            header_kind_and_status(&profile),
            "Entity · Dissolved 2019-04-02"
        );
    }

    #[test]
    fn the_not_found_sentence_is_the_one_the_api_returns() {
        assert_eq!(
            entity_not_found("GB-COH:X6999001"),
            "No entity with id GB-COH:X6999001 exists in this analysis."
        );
    }

    /// **Show more appends; it never replaces.** The handler returns the named section's *next*
    /// window and every other section at its first, so a renderer that swapped the response in
    /// would drop the rows already on screen while the caption went on quoting a total that no
    /// longer described them.
    ///
    /// PROVE THIS CAN FAIL: change `append!` to assign `incoming.$field.rows` instead of extending
    /// with it, and the row count below reads 1 rather than 2.
    #[test]
    fn a_show_more_response_is_appended_to_the_rows_already_on_screen() {
        let mut current = sample_profile();
        assert_eq!(
            current.officers.rows.len(),
            1,
            "the fixture starts with one row"
        );
        let mut incoming = sample_profile();
        incoming.officers.rows[0].name = "Second Officer".to_string();
        incoming.officers.next_cursor = None;
        incoming.officers.total = 2_400;

        assert!(merge_section(&mut current, incoming, "officers"));
        assert_eq!(current.officers.rows.len(), 2, "the window was appended");
        assert_eq!(current.officers.rows[0].name, "Jane Smith");
        assert_eq!(current.officers.rows[1].name, "Second Officer");
        assert_eq!(
            current.officers.next_cursor, None,
            "the new cursor is taken"
        );
        assert_eq!(current.officers.total, 2_400, "the total is unchanged");
    }

    /// A cursor naming a section this profile does not page is refused, not applied to whichever
    /// list happens to be first — the same refusal `profile_map::window_from_request` makes on the
    /// server, for the same reason.
    #[test]
    fn a_merge_into_an_unknown_section_is_refused() {
        let mut current = sample_profile();
        let before = current.clone();
        assert!(!merge_section(&mut current, sample_profile(), "sanctions"));
        assert_eq!(current, before, "a refused merge changes nothing");
        // Non-vacuity: the same call with a real section name does merge.
        assert!(merge_section(&mut current, sample_profile(), "contracts"));
    }
}
