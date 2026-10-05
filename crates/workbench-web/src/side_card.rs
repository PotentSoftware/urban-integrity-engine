//! `side_card.rs` — the Graph page's node-click **inspection panel** (the phase 44 UI spec "Node click →
//! side card", D-11/D-12).
//!
//! # What this panel is, since plan 45-13
//!
//! Plan 44-10 built it as five facts: name, kind, degree, earliest edge, latest edge. Plan 45-13
//! replaces that body with **plan 45-10's profile renderer at `Density::Compact`** — the same
//! component `/entity/:id` mounts at `Density::Full`, with no second copy of a single section, row,
//! empty state, provenance rule or date-range word.
//!
//! That is D-11, and this project has its own worked example of the alternative: the Cosmograph
//! shim, where a vendored bundle and its documented "source" diverged for eight days across four
//! features and **nothing failed**, because nothing consumed the source. A second profile renderer
//! would diverge exactly that way and be exactly that quiet.
//! `tests/expansion_reachable.rs` is the instrument that would notice.
//!
//! # The node classes that have no profile
//!
//! The canvas shows every node class; `query_entity_by_id` resolves two of them. Measured on the
//! live analysis (45-11 §2.2): **103,973 of 362,609 projected nodes, 28.7%**, carry a class with a
//! profile route. So the common case on this canvas is a click that **cannot** open a profile, and
//! the panel has to answer for it honestly rather than with an error or a blank.
//!
//! It does three things, in this order of preference:
//!
//! 1. **It does not ask.** The class test runs before the fetch, so a `Location` or `Ward` click
//!    issues no request that is known in advance to 404. A panel whose ordinary behaviour is to
//!    provoke a 404 teaches an operator to ignore 404s.
//! 2. **It says which class, and what does open** — derived from `OPENABLE_KINDS`, not retyped, so
//!    the sentence changes on its own the day a third class gains a subject lookup.
//! 3. **It still shows what the graph does record**: degree, earliest and latest edge, from
//!    `GET /api/graph/node`, which answers for every node class. The absent thing is a *profile*,
//!    not the node.
//!
//! Widening the read path was declined one layer up (45-10, 45-11) and is not revisited here:
//! assembling a `Location` into an `EntityProfileDto` would render fourteen of eighteen sections
//! saying "No officers recorded for this entity" about an **address**, which is a wrong answer
//! dressed as an empty one.
//!
//! # Two fetches, and why the node summary is still one of them
//!
//! `GET /api/graph/node` is not superseded by the profile. Degree and the earliest/latest edge
//! timestamps are facts about this node's position in **the mounted graph**, which no profile
//! section carries — they are the reason a click on the canvas is a different question from a visit
//! to `/entity/:id`. They are kept, under their own caption, beneath the profile.
//!
//! # Testing approach (plain-function-first, per `verdict_badge.rs`'s documented rationale)
//!
//! `fetch_node_summary` itself calls `gloo-net`'s Fetch-API transport, which panics outside an
//! actual browser runtime (same rationale as `status_page.rs`/`analysis_picker.rs`), so it is not
//! unit-tested directly — only the pure `node_summary_url` URL-building helper it delegates to.

use leptos::prelude::*;
use urban_integrity_workbench_dto::neighbourhood::NeighbourhoodNodeView;
use urban_integrity_workbench_dto::{EntityRef, NodeSummary};

use crate::entity_ref_view::EntityRefView;
use crate::investigation::pin::PinControl;
use crate::investigation::pin_origin_for;
use crate::neighbourhood::{edge_line, reached_from_line, undated_edges_note, REACHED_BY_CAPTION};
use crate::profile::pivot::{namespace_class, opens_a_profile_ref, OPENABLE_KINDS};
use crate::profile::{
    entity_not_found, fetch_profile, rejection_detail, Density, ProfileLoad, ProfileRenderer,
    PROFILE_LOADING, PROFILE_LOAD_FAILURE,
};
use crate::timestamp::Timestamp;

/// Minimal RFC 3986 percent-encoder for a single query VALUE (never a key): encodes every byte
/// outside the unreserved set (`A-Z a-z 0-9 - . _ ~`) as `%XX`. Entity ids in this project always
/// carry a colon (`director:jane-smith`, `ward:X05001499`, `GB-COH:99000001`) which MUST be
/// encoded — `gloo-net`'s `Request::get(...).query(...)` builder already percent-encodes via
/// `web_sys::UrlSearchParams` internally (confirmed: `gloo-net`'s own `Cargo.toml` already
/// requires the `web-sys/UrlSearchParams` feature, so it reaches this crate's build via Cargo's
/// feature unification with no `Cargo.toml` edit needed here), but that path is NOT host-testable
/// (constructing a `web_sys` type outside an actual wasm/JS runtime aborts the test process, the
/// same `cosmograph_trait.rs`/`js_error_message` rule) — so [`node_summary_url`] is instead a
/// plain, pure string function this hand-rolled encoder backs, keeping the URL-building logic
/// host-testable per this module's own `<behavior>` requirement.
fn percent_encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{byte:02X}"));
            }
        }
    }
    out
}

/// `GET /api/graph/node?id=` — the native endpoint plan 44-08 built (`node_summary_handler`).
pub const NODE_SUMMARY_URL_PATH: &str = "/api/graph/node";

/// The D-23 "unreadable endpoint shape" error copy for [`NODE_SUMMARY_URL_PATH`], matching every
/// other fetch-failure copy string in this crate's established pattern (`GRAPH_FETCH_ERROR_COPY`,
/// `ANALYSIS_FETCH_ERROR_COPY`).
pub const NODE_SUMMARY_FETCH_ERROR_COPY: &str =
    "The workbench returned data this page cannot read (/api/graph/node).";

/// Builds `"/api/graph/node?id=<id>"` with `id` percent-encoded — entity ids always contain a
/// colon (`director:jane-smith`, `ward:X05001499`), which this function's encoder handles.
pub fn node_summary_url(id: &str) -> String {
    format!(
        "{NODE_SUMMARY_URL_PATH}?id={}",
        percent_encode_query_value(id)
    )
}

/// Fetches and deserialises [`NodeSummary`] from [`node_summary_url`]`(id)`. Same fetch/error-
/// string shape as every other fetch in this crate.
async fn fetch_node_summary(id: &str) -> Result<NodeSummary, String> {
    let response = gloo_net::http::Request::get(&node_summary_url(id))
        .send()
        .await
        .map_err(|_| NODE_SUMMARY_FETCH_ERROR_COPY.to_string())?;
    if !response.ok() {
        return Err(NODE_SUMMARY_FETCH_ERROR_COPY.to_string());
    }
    response
        .json::<NodeSummary>()
        .await
        .map_err(|_| NODE_SUMMARY_FETCH_ERROR_COPY.to_string())
}

/// The panel's frame: plan 44-10's 280px card, now scrollable.
///
/// `Density::Compact` renders a header, the null-range rule where one applies, and the first three
/// rows of each of eighteen sections — routinely taller than the canvas. 44-10's card had a fixed
/// height only because its body was five lines; a fixed-height frame around this body would clip
/// the sections silently, which is the same class of failure as a silently shorter graph.
const CARD_STYLE: &str = "position: absolute; top: var(--space-md); right: var(--space-md); width: 280px; max-height: calc(100% - 2 * var(--space-md)); overflow-y: auto; overscroll-behavior: contain; background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: var(--space-lg); display: flex; flex-direction: column; gap: var(--space-sm); z-index: 2;";
const CLOSE_BUTTON_STYLE: &str = "min-width: 44px; min-height: 44px; background: transparent; border: none; cursor: pointer; color: var(--muted-foreground); font-size: 18px; line-height: 1; flex-shrink: 0;";
const CAPTION_STYLE: &str = "color: var(--muted-foreground);";

/// The caption over the two facts the profile does not carry.
pub const CANVAS_FACTS_CAPTION: &str = "On this canvas";

/// One node class as it reads in a sentence: `"OffshoreEntity"` -> `"offshore entity"`,
/// `"PoliticalActor"` -> `"political actor"`, `"Ward"` -> `"ward"`.
///
/// Derived from the class name rather than tabulated, so a twelfth `NodeWeight` variant reads
/// correctly the day it arrives instead of falling through a `match` to a placeholder. `node_
/// palette::node_kind_label` cannot serve here: it deliberately collapses five classes into one
/// legend swatch, and a panel saying "no profile page exists for an OSINT node" would be naming a
/// colour rather than a class.
pub fn class_phrase(kind: &str) -> String {
    let mut out = String::with_capacity(kind.len() + 2);
    for (i, ch) in kind.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push(' ');
        }
        out.extend(ch.to_lowercase());
    }
    out
}

/// The classes that DO open, as a sentence fragment — read from `OPENABLE_KINDS`, never retyped.
///
/// The day `query_entity_by_id` learns a third class, `OPENABLE_KINDS` grows by one line (45-10's
/// hand-over) and this sentence follows without being edited. A hard-coded "entity and offshore
/// entity" would go on naming two classes while three opened.
pub fn openable_classes_phrase() -> String {
    let names: Vec<String> = OPENABLE_KINDS.iter().map(|k| class_phrase(k)).collect();
    match names.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// What the panel says about a node whose class has no profile route.
///
/// **Not an error and not a blank.** It is an answer about the *read path*, and it is followed on
/// screen by the facts the graph does record about this node — so the panel is never empty and
/// never implies the node is missing.
pub fn no_profile_route_sentence(class: &str) -> String {
    format!(
        "No profile page exists for a {} node yet. The profile read path resolves {} nodes only, \
         so this panel shows what the graph records about this one.",
        class_phrase(class),
        openable_classes_phrase()
    )
}

/// The class to name in [`no_profile_route_sentence`], or `None` when this node opens a profile.
///
/// When the id's namespace and the `kind` field disagree, the **namespace** is named: 45-11
/// measured fifty live references whose `kind` said `Entity` over an id that named a contract, a
/// ward or an address, and the id was right every time. `opens_a_profile_ref` already refuses such
/// a reference; this names what it refused.
pub fn class_without_a_profile(entity: &EntityRef) -> Option<String> {
    if opens_a_profile_ref(entity) {
        return None;
    }
    Some(
        namespace_class(&entity.id)
            .map(str::to_string)
            .unwrap_or_else(|| entity.kind.clone()),
    )
}

/// The Graph page's node inspection panel (the phase 44 UI spec "Node click → side card", D-11).
///
/// Hosts plan 45-10's [`ProfileRenderer`] at [`Density::Compact`] for a node whose class has a
/// profile route, and [`no_profile_route_sentence`] for one that does not — in both cases above the
/// graph-local facts (`degree`, earliest/latest edge) that `GET /api/graph/node` answers for every
/// class. 280px wide, `md` inset from the canvas's top-right corner (an inline `position:
/// absolute`; the caller gives the canvas container `position: relative`), scrollable, with a
/// 44x44px `x` close button.
///
/// `on_show_more` is deliberately **not** passed to the renderer: `Compact` shows the first three
/// rows of each section and the profile page is where the rest is read, so a Show more button here
/// would page a list inside a 280px column that the analyst can open in full with one click on the
/// name above it.
#[component]
#[allow(non_snake_case)]
pub fn SideCard(
    node_id: String,
    /// How the current expansion reached this node, when it did (CNV-02): the edges the walk
    /// crossed, with their types and recorded ranges. `None` for the seed, for a node outside the
    /// expansion, and when there is no expansion at all — three situations in which there is no
    /// edge to describe and inventing one would be the failure.
    reached: Signal<Option<NeighbourhoodNodeView>>,
    on_close: impl Fn() + 'static + Clone,
) -> impl IntoView {
    let id_for_summary = node_id.clone();
    let summary = LocalResource::new(move || {
        let id = id_for_summary.clone();
        async move { fetch_node_summary(&id).await }
    });
    let close_for_button = on_close.clone();

    // The profile is fetched only once the node summary has named a class that can open one. That
    // ordering is the point: it is what keeps a Ward click from issuing a request whose 404 was
    // knowable before it was sent.
    let profile: RwSignal<Option<ProfileLoad>> = RwSignal::new(None);
    Effect::new(move |_| {
        let Some(Ok(node_summary)) = summary.get().map(|r| r.map_err(|e| e.clone())) else {
            return;
        };
        if !opens_a_profile_ref(&node_summary.entity) {
            return;
        }
        let id = node_summary.entity.id.clone();
        profile.set(None);
        leptos::task::spawn_local(async move {
            let loaded = fetch_profile(id, None, None, None).await;
            profile.set(Some(loaded));
        });
    });

    view! {
        <div style=CARD_STYLE>
            <div style="display: flex; justify-content: space-between; align-items: flex-start; gap: var(--space-sm);">
                <div style="display: flex; flex-direction: column; gap: var(--space-md); flex: 1; min-width: 0;">
                    <Suspense fallback=move || view! {
                        <span class="text-body" style=CAPTION_STYLE>"Loading\u{2026}"</span>
                    }>
                        {move || summary.get().map(|result| match result {
                            Ok(node_summary) => {
                                let entity = node_summary.entity.clone();
                                let no_route = class_without_a_profile(&entity);
                                view! {
                                    <div style="display: flex; flex-direction: column; gap: var(--space-md);">
                                        {match no_route {
                                            // A class with no profile route: name it, say what does
                                            // open, and keep the node's own name and kind on screen.
                                            // `EntityRefView` renders it as plain text here on its
                                            // own, through the same policy that refused the profile.
                                            Some(class) => view! {
                                                <div style="display: flex; flex-direction: column; gap: var(--space-sm);">
                                                    <EntityRefView entity=entity.clone() />
                                                    <span class="text-label" style=CAPTION_STYLE>
                                                        {entity.kind.clone()}
                                                    </span>
                                                    <p class="text-body" role="note" style=format!("margin: 0; {CAPTION_STYLE}")>
                                                        {no_profile_route_sentence(&class)}
                                                    </p>
                                                    // PIV-05: a pin is a REFERENCE to an id and
                                                    // needs no profile. 45-11 measured that 71.3%
                                                    // of the nodes on this canvas have no profile
                                                    // route, so without this branch "pin an entity
                                                    // from the canvas" would be unavailable for
                                                    // seven clicks in ten. Same component, same
                                                    // origin as the compact profile's.
                                                    <PinControl
                                                        entity_id=entity.id.clone()
                                                        origin=pin_origin_for(Density::Compact)
                                                        as_at=None
                                                    />
                                                </div>
                                            }.into_any(),
                                            // A class that opens: the one profile renderer, compact.
                                            None => view! {
                                                {move || match profile.get() {
                                                    None => view! {
                                                        <span class="text-body" style=CAPTION_STYLE>{PROFILE_LOADING}</span>
                                                    }.into_any(),
                                                    Some(ProfileLoad::Loaded(loaded)) => view! {
                                                        <ProfileRenderer profile=*loaded density=Density::Compact />
                                                    }.into_any(),
                                                    Some(ProfileLoad::NotFound) => view! {
                                                        <p class="text-body" style="margin: 0; color: var(--destructive);">
                                                            {entity_not_found(&entity.id)}
                                                        </p>
                                                    }.into_any(),
                                                    // A refusal in THIS frame is not the
                                                    // analyst's to correct: the canvas built this
                                                    // request from a node they clicked, not from
                                                    // an address they typed. So the profile
                                                    // screen's "correct the address" copy is
                                                    // deliberately not reused here — the panel
                                                    // renders the server's own sentence, which
                                                    // names what was refused, and falls back to
                                                    // the generic failure copy when the body
                                                    // carried no reason. Unreachable today (this
                                                    // panel sends no `as_at`, and the only other
                                                    // 400 on the route needs a blank id), and
                                                    // handled rather than left to a wildcard so
                                                    // it stays unreachable by evidence rather
                                                    // than by assumption.
                                                    Some(ProfileLoad::Rejected(reason)) => {
                                                        let line = rejection_detail(&reason)
                                                            .unwrap_or(PROFILE_LOAD_FAILURE)
                                                            .to_string();
                                                        view! {
                                                            <p class="text-body" style="margin: 0; color: var(--destructive);">
                                                                {line}
                                                            </p>
                                                        }.into_any()
                                                    },
                                                    Some(ProfileLoad::Failed(_)) => view! {
                                                        <p class="text-body" style="margin: 0; color: var(--destructive);">
                                                            {PROFILE_LOAD_FAILURE}
                                                        </p>
                                                    }.into_any(),
                                                }}
                                            }.into_any(),
                                        }}
                                        // CNV-02: the edges the expansion crossed to get here,
                                        // each with its type and the range the record states —
                                        // rendered through `range_text`, the profile screen's own
                                        // function, so the canvas and the panel above it spell a
                                        // date range one way.
                                        {move || reached.get().map(|node| {
                                            let note = undated_edges_note(&node.edges);
                                            let lines: Vec<String> =
                                                node.edges.iter().map(edge_line).collect();
                                            view! {
                                                <div style="display: flex; flex-direction: column; gap: var(--space-xs); border-top: 1px solid var(--border); padding-top: var(--space-sm);">
                                                    <span class="text-label" style=CAPTION_STYLE>{REACHED_BY_CAPTION}</span>
                                                    {lines.into_iter().map(|line| view! {
                                                        <span class="text-body">{line}</span>
                                                    }).collect_view()}
                                                    <span class="text-label" style=CAPTION_STYLE>
                                                        {reached_from_line(&node)}
                                                    </span>
                                                    {note.map(|text| view! {
                                                        <span class="text-label" style=CAPTION_STYLE>{text}</span>
                                                    })}
                                                </div>
                                            }
                                        })}
                                        // The two facts no profile section carries: where this node
                                        // sits in the mounted graph.
                                        <div style="display: flex; flex-direction: column; gap: var(--space-xs); border-top: 1px solid var(--border); padding-top: var(--space-sm);">
                                            <span class="text-label" style=CAPTION_STYLE>{CANVAS_FACTS_CAPTION}</span>
                                            <span class="text-body font-mono">{format!("Degree: {}", node_summary.degree)}</span>
                                            {node_summary.earliest_edge.clone().map(|at| view! {
                                                <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
                                                    <span class="text-label" style=CAPTION_STYLE>"Earliest edge"</span>
                                                    <Timestamp at=at />
                                                </div>
                                            })}
                                            {node_summary.latest_edge.clone().map(|at| view! {
                                                <div style="display: flex; flex-direction: column; gap: var(--space-xs);">
                                                    <span class="text-label" style=CAPTION_STYLE>"Latest edge"</span>
                                                    <Timestamp at=at />
                                                </div>
                                            })}
                                        </div>
                                    </div>
                                }.into_any()
                            }
                            Err(message) => view! {
                                <span class="text-body" style="color: var(--destructive);">{message}</span>
                            }.into_any(),
                        })}
                    </Suspense>
                </div>
                <button
                    aria-label="Close"
                    style=CLOSE_BUTTON_STYLE
                    on:click=move |_| close_for_button()
                >
                    "\u{00d7}"
                </button>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_summary_url_percent_encodes_the_colon_in_a_typical_entity_id() {
        assert_eq!(
            node_summary_url("director:jane-smith"),
            "/api/graph/node?id=director%3Ajane-smith"
        );
    }

    #[test]
    fn node_summary_url_percent_encodes_a_ward_code_id() {
        assert_eq!(
            node_summary_url("ward:X05001499"),
            "/api/graph/node?id=ward%3AX05001499"
        );
    }

    #[test]
    fn node_summary_url_percent_encodes_a_company_number_id_with_a_hyphenated_prefix() {
        assert_eq!(
            node_summary_url("GB-COH:99000001"),
            "/api/graph/node?id=GB-COH%3A99000001"
        );
    }

    /// PROVE THIS CAN FAIL: an id with no colon at all must still round-trip untouched — this
    /// distinguishes "the encoder correctly leaves safe characters alone" from "the encoder
    /// mangles everything," which a colon-only fixture could not distinguish.
    #[test]
    fn node_summary_url_leaves_an_id_with_no_special_characters_untouched() {
        assert_eq!(
            node_summary_url("plainid123"),
            "/api/graph/node?id=plainid123"
        );
    }

    fn entity(id: &str, kind: &str) -> EntityRef {
        EntityRef {
            id: id.to_string(),
            name: "Example Holdings Ltd".to_string(),
            kind: kind.to_string(),
        }
    }

    /// A class name reads as English in a sentence, including one nobody has written a row for.
    ///
    /// PROVE THIS CAN FAIL: drop the `i > 0` guard and `"Ward"` becomes `" ward"`; drop the space
    /// insertion entirely and `"OffshoreEntity"` becomes `"offshoreentity"`. Both were run.
    #[test]
    fn a_node_class_reads_as_english_in_the_sentence_that_names_it() {
        assert_eq!(class_phrase("Ward"), "ward");
        assert_eq!(class_phrase("OffshoreEntity"), "offshore entity");
        assert_eq!(class_phrase("PoliticalActor"), "political actor");
        // A class this crate has never seen still reads correctly — the reason this is derived
        // rather than tabulated.
        assert_eq!(class_phrase("SomeFutureKind"), "some future kind");
    }

    /// The sentence names what DOES open by reading the policy, so it cannot go on naming two
    /// classes the day a third opens.
    ///
    /// PROVE THIS CAN FAIL: hard-code `"entity and offshore entity"` in
    /// `openable_classes_phrase`, then add a class to `OPENABLE_KINDS`; the phrase stops agreeing
    /// with the policy and this fails. Run in both directions.
    #[test]
    fn the_no_profile_sentence_names_the_classes_the_read_path_actually_opens() {
        let phrase = openable_classes_phrase();
        for kind in OPENABLE_KINDS {
            assert!(
                phrase.contains(&class_phrase(kind)),
                "{phrase} must name {kind}"
            );
        }
        // Non-vacuity: the policy really does name more than one class, so the joining is doing
        // work rather than returning a single word.
        assert!(
            OPENABLE_KINDS.len() >= 2,
            "the policy must open more than one class"
        );
        assert!(
            phrase.contains(" and "),
            "two or more classes are joined: {phrase}"
        );

        let sentence = no_profile_route_sentence("Location");
        assert!(sentence.contains("a location node"), "{sentence}");
        assert!(sentence.contains(&phrase), "{sentence}");
        // It is an answer about the read path, not an error and not a claim the node is missing.
        assert!(
            !sentence.contains("error") && !sentence.contains("not found"),
            "the sentence must not read as a failure: {sentence}"
        );
        assert!(
            sentence.contains("what the graph records about this one"),
            "the sentence must point at what it does show: {sentence}"
        );
    }

    /// Which node gets the sentence, and which gets the profile.
    ///
    /// PROVE THIS CAN FAIL: return `None` unconditionally from `class_without_a_profile` and the
    /// Location/Ward assertions fail; return `Some` unconditionally and the company assertion does.
    #[test]
    fn only_a_class_the_read_path_cannot_open_gets_the_no_profile_sentence() {
        assert_eq!(
            class_without_a_profile(&entity("GB-COH:X6999001", "Entity")),
            None
        );
        assert_eq!(
            class_without_a_profile(&entity("GB-OCOD:X6999002", "OffshoreEntity")),
            None
        );
        assert_eq!(
            class_without_a_profile(&entity("ward:X05999001", "Ward")).as_deref(),
            Some("Ward")
        );
        assert_eq!(
            class_without_a_profile(&entity("LOC:AB101JB:7175", "Location")).as_deref(),
            Some("Location")
        );
    }

    /// When the id's namespace and the `kind` field disagree, the namespace is what gets named.
    ///
    /// 45-11 measured fifty live references whose `kind` said `Entity` over an id naming a
    /// contract, a ward or an address — and the id was right every time. A panel that echoed the
    /// `kind` would tell the analyst "no profile page exists for an entity node", which is false
    /// about entities and useless about this one.
    ///
    /// PROVE THIS CAN FAIL: swap the `namespace_class(...).unwrap_or(kind)` for `entity.kind`
    /// alone and this fails.
    #[test]
    fn a_contract_id_wearing_the_entity_catch_all_is_named_as_a_contract() {
        let mislabelled = entity("CF:X9999-0001", "Entity");
        assert_eq!(
            class_without_a_profile(&mislabelled).as_deref(),
            Some("Contract")
        );
        assert!(no_profile_route_sentence("Contract").contains("a contract node"));
        // Non-vacuity: the same `kind` over an entity namespace still opens, so this is about the
        // namespace guard rather than about `Entity` never opening.
        assert_eq!(
            class_without_a_profile(&entity("GB-COH:X6999001", "Entity")),
            None
        );
    }

    #[test]
    fn node_summary_fetch_error_copy_names_the_node_endpoint() {
        assert!(NODE_SUMMARY_FETCH_ERROR_COPY.contains("/api/graph/node"));
    }
}
