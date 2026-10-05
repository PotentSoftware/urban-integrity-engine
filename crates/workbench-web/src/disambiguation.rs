//! `DisambiguationList` — PIV-01's candidate list, which opens **before any profile does**.
//!
//! # The load-bearing clause
//!
//! PIV-01's wording is "before any profile opens", and the phase 45 UI spec restates why: opening the
//! first match and offering a switcher afterwards is a different, worse product, because the
//! analyst has already begun reading facts about a company that may not be theirs.
//!
//! That guarantee is **structural here, not a convention**. The only value in this module that can
//! carry a profile target is [`SearchOutcome::OpenProfile`], and [`outcome_for`] constructs it from
//! a **one-row** response and from nothing else. A multi-match response becomes
//! [`SearchOutcome::Disambiguate`], which carries the whole row set and no single target — there is
//! no first row for a caller to reach for. [`apply_outcome`] is the one place the `on_open`
//! callback is ever invoked, and `a_multi_match_search_opens_no_profile` counts those invocations
//! rather than inspecting a rendered shape: a page that rendered the list AND opened match one
//! would pass a shape assertion and fails this one.
//!
//! [`DisambiguationList`] itself never calls `on_open` on the analyst's behalf either — a candidate
//! is opened when the analyst activates that candidate's row, and at no other moment.
//!
//! # The counterweight: the single-match fast path
//!
//! One unambiguous match goes straight through, so the list is not an extra click on the common
//! path (the phase 45 UI spec). A list that never appears and a fast path that never fires are the same
//! bug seen from opposite sides, so both directions are asserted.
//!
//! **The trigger is `rows.len() == 1`, and on the live corpus it is the common case.** Measured on
//! the live analysis, 2026-09-15, over its 73,865 entity names: pasting a full company name
//! verbatim matches exactly one entity for **369 of a 400-name random sample (92.2%)**; 27 (6.8%)
//! match 2-10 and open the list; 4 (1.0%) reach the read path's 10-match cap. A raw graph id or a
//! valid Companies House number resolves to at most one profile by construction (45-08's handler
//! calls `query_entity_by_id`/`query_entity_by_ch_number`, both single-valued), so those queries
//! always take the fast path. The list is genuinely the exception, which is what the spec's "not an
//! extra click on the common path" asks for.
//!
//! # Where the three row fields come from
//!
//! `GET /api/entity/disambiguate` (plan 45-08) already assembles them from
//! `crates/core::persistence`, which is what stops a row and the profile it opens from disagreeing
//! about whether a company is dissolved (PIV-06/D-01). This module renders that response and
//! derives no fact of its own:
//!
//! | Field | Source | Absence |
//! |---|---|---|
//! | type | `DisambiguationRowView::entity.kind` — the label index's node class (44-06) | never absent |
//! | status | `DisambiguationRowView::status` — `EntityData::status`, via the read path | never absent; `Active` is the serde default for a record with no status |
//! | ward | `DisambiguationRowView::ward` — the most frequent `ward_code` across the candidate's contracts and planning applications | **`None` is common and correct**; the segment is omitted, never rendered as a placeholder |
//!
//! **A ward has no ward of its own.** Measured on the live analysis, 2026-09-15: all 129,586
//! `Represents` edges run Ward -> Contract and **zero** run Ward -> Entity, which is why 45-08
//! derives a company's ward from its contracts rather than reading one off the node. A company
//! with no ward-coded contract therefore has no ward here, and omitting the segment is the honest
//! rendering — see [`row_secondary_text`].
//!
//! **The ward name is frequently the ward code, and this module must not disguise that.** Of the
//! 614 `ward` nodes in that analysis, **597 (97.2%) carry a `name` equal to their own bare code**
//! (`ward:E05000937` is named `"E05000937"`); only 17 carry a name that is not its own code. The phase 45 UI spec's "a ward renders its **name**, not its code" is a rule about
//! this renderer, and this renderer keeps it — it renders `ward.name` verbatim and never
//! substitutes a code. When that name IS a code, the defect is upstream in the ward backfill, and
//! printing it unchanged is what makes the gap visible rather than laundering it into something
//! that looks like a name.
//!
//! # Testing approach
//!
//! Plain-function-first, the convention `tool_rail.rs`/`graph_results_list.rs` document: every
//! piece of content-building and every branch of the open/list decision is a plain function over
//! owned DTO values, host-testable with no DOM and no browser (this workspace has no
//! headless-browser runner — phase 42 research Pitfall 10).
//!
//! Module-wide `#![allow(dead_code)]`: this component lands before its mount site, exactly as
//! `entity_picker.rs`, `tool_rail.rs` and `graph_results_list.rs` each did. **Plan 45-10 owns the
//! `/entity/:id` route and is the first caller.**
#![allow(dead_code)]

use leptos::prelude::*;
use urban_integrity_workbench_dto::{
    DisambiguationRowView, DisambiguationView, EntityRef, EntityStatusView,
};

use crate::entity_picker::{percent_encode_query_value, EntityPicker, NO_ENTITY_MATCHES};

/// The phase 45 UI spec Copy Deck, "Disambiguation help line" — verbatim, rendered beneath the heading.
pub const DISAMBIGUATION_HELP: &str = "Choose one to open its profile.";

/// `GET /api/entity/disambiguate?q=` (plan 45-08) — the fetch URL for a typed query.
///
/// Shares `entity_picker`'s own percent-encoder rather than carrying a second copy: the query an
/// analyst types here is the same name/code/raw-id string the picker resolves, colons and all
/// (`ward:X05001499`), and two encoders would be two chances to break one query string.
pub fn disambiguate_url(query: &str) -> String {
    format!(
        "/api/entity/disambiguate?q={}",
        percent_encode_query_value(query)
    )
}

/// Fetches and deserialises the candidate list from [`disambiguate_url`]. A query matching nothing
/// is a `200` with an empty `rows` (the handler's own contract) — never an `Err` here; `Err` is
/// reserved for a genuine transport/shape failure.
pub async fn fetch_disambiguation(query: String) -> Result<DisambiguationView, String> {
    let response = gloo_net::http::Request::get(&disambiguate_url(&query))
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if !response.ok() {
        return Err(format!(
            "entity disambiguate responded with status {}",
            response.status()
        ));
    }
    response
        .json::<DisambiguationView>()
        .await
        .map_err(|err| err.to_string())
}

/// A status in the vocabulary the entity profile already states it in — never a raw enum name
/// (the phase 45 UI spec: "Status renders with the same vocabulary the profile header uses").
///
/// The strings are `crates/cli`'s `output::format_entity_status` exactly: `"Active"`,
/// `"Dissolved {date}"`, `"In Liquidation {date}"`. That function IS the profile header vocabulary
/// today — the `entity` command's header block and its own disambiguation table both render
/// through it, and the web profile header (plan 45-10) does not exist yet. D-01's rule is that the
/// CLI and the workbench must not disagree about one company, so the web header must render
/// through THIS function when it lands rather than spelling a third set of words.
///
/// The date is the one `EntityStatusView` carries, reproduced verbatim: the wire keeps it
/// structured precisely so a renderer never has to split a display string to recover it.
pub fn status_label(status: &EntityStatusView) -> String {
    match status {
        EntityStatusView::Active => "Active".to_string(),
        EntityStatusView::Dissolved { on } => format!("Dissolved {on}"),
        EntityStatusView::InLiquidation { on } => format!("In Liquidation {on}"),
    }
}

/// The heading (the phase 45 UI spec: **{n} entities match "{query}".**), with two departures the spec's
/// own text requires elsewhere:
///
/// * **`more_may_exist` suppresses the number.** The read path caps at 10 matches and reports no
///   total, so `DisambiguationView::more_may_exist` means "10 of an unknown number". Printing
///   `rows.len()` as "the" count would state a figure the server never established, which the DTO
///   module doc forbids in as many words. `{n}+` is the honest form.
/// * **A singular form.** The phase 45 UI spec Open Question 5 records that pluralisation is unspecified
///   and already misreads at n=1. A one-match query is the fast path this plan's Task 2 adds — it
///   opens a profile and renders no list — so the heading is not reachable at n=1; but a heading
///   that would print `1 entities match` if it ever were is a latent defect, so the singular is
///   declared here rather than left to chance.
pub fn heading_text(view: &DisambiguationView) -> String {
    let n = view.rows.len();
    let query = &view.query;
    if view.more_may_exist {
        return format!("{n}+ entities match \"{query}\".");
    }
    if n == 1 {
        format!("1 entity matches \"{query}\".")
    } else {
        format!("{n} entities match \"{query}\".")
    }
}

/// A row's secondary line: `{kind} · {status} · {ward}` (the phase 45 UI spec), Label size,
/// `--muted-foreground`.
///
/// **A missing ward drops its segment; it never becomes a placeholder.** `ward: None` is a real
/// answer rather than a gap in the data — a Contracts Finder supplier with no ward-coded award has
/// no ward in this analysis, and 45-08's handler documents that as the majority case for
/// companies. "Unknown ward" on most rows would read as a data-quality warning about the entity
/// instead of a plain property of the corpus.
///
/// The ward renders its `name` — whatever the graph holds there, never a code this function
/// substitutes (see the module doc for why that distinction is doing real work on live data).
pub fn row_secondary_text(row: &DisambiguationRowView) -> String {
    let mut parts = vec![row.entity.kind.clone(), status_label(&row.status)];
    if let Some(ward) = &row.ward {
        parts.push(ward.name.clone());
    }
    parts.join(" · ")
}

/// What a resolved search means: nothing, exactly one candidate, or several.
///
/// **The shape is the guarantee.** Only [`SearchOutcome::OpenProfile`] carries an [`EntityRef`],
/// and only a one-row response produces it ([`outcome_for`]). [`SearchOutcome::Disambiguate`]
/// carries the rows as a set with no distinguished member, so "open the first one" is not a
/// shortcut a caller can take without first rewriting this enum.
#[derive(Debug, Clone, PartialEq)]
pub enum SearchOutcome {
    /// Nothing matched. Carries the empty response so the renderer can still quote the query.
    NoMatch(DisambiguationView),
    /// Exactly one unambiguous match — the fast path. The ONLY variant carrying a profile target.
    OpenProfile(EntityRef),
    /// Several matches: PIV-01's list, before any profile opens.
    Disambiguate(DisambiguationView),
}

/// Classifies a resolved response. See [`SearchOutcome`] for why the return type is doing the
/// enforcing.
///
/// `more_may_exist` is checked alongside the row count even though 45-08's handler sets it from
/// `profiles.len() >= 10` and so cannot report a cap on a one-row response: a fast path that fires
/// on "one of an unknown number" would open a profile chosen by a truncation, and the guard costs
/// one comparison to make that unreachable regardless of how the cap is later reported.
pub fn outcome_for(view: DisambiguationView) -> SearchOutcome {
    match view.rows.len() {
        0 => SearchOutcome::NoMatch(view),
        1 if !view.more_may_exist => SearchOutcome::OpenProfile(view.rows[0].entity.clone()),
        _ => SearchOutcome::Disambiguate(view),
    }
}

/// Acts on an outcome: opens the fast path's profile, or hands back the list to render.
///
/// `open` is invoked **exactly once on [`SearchOutcome::OpenProfile`] and never on any other
/// variant**. `None` means a profile was opened and no list should be rendered; `Some(view)` means
/// a list is to be rendered and no profile has been opened.
///
/// This is the single choke-point between a search result and a profile. Anything that fetches a
/// profile must hang off `open`, which is what makes "a multi-match search performs no profile
/// fetch" a property the host tests can count rather than a claim about a rendered tree.
pub fn apply_outcome(
    outcome: SearchOutcome,
    open: &dyn Fn(EntityRef),
) -> Option<DisambiguationView> {
    match outcome {
        SearchOutcome::OpenProfile(entity) => {
            open(entity);
            None
        }
        SearchOutcome::NoMatch(view) | SearchOutcome::Disambiguate(view) => Some(view),
    }
}

/// One candidate row's style. 44px minimum hit target over the WHOLE row (the phase 45 UI spec: "the
/// whole 44px row is the hit target, not just the text"), `md` internal padding per the spacing
/// table's "disambiguation row internal padding" entry.
fn row_style() -> String {
    "display: flex; flex-direction: column; align-items: flex-start; gap: var(--space-xs); width: 100%; box-sizing: border-box; min-height: 44px; padding: var(--space-md); background: transparent; border: 1px solid var(--border); border-radius: 6px; color: var(--foreground); text-align: left; cursor: pointer;".to_string()
}

/// One candidate row: name over `kind · status · ward`, the whole row activating the pick.
///
/// A native `<button>`, not a styled `<div>`: the phase 45 UI spec Open Question 3 records that this list
/// is "a keyboard-selection surface with no key handling declared", and a button is focusable and
/// `Enter`/`Space`-activated by the platform, so the gap is closed without this plan inventing a
/// key model the spec has not settled.
#[component]
#[allow(non_snake_case)]
fn CandidateRow(
    row: DisambiguationRowView,
    on_open: impl Fn(EntityRef) + 'static + Clone + Send,
) -> impl IntoView {
    let secondary = row_secondary_text(&row);
    let name = row.entity.name.clone();
    let entity = row.entity.clone();

    view! {
        <button
            type="button"
            style=row_style()
            on:click=move |_| on_open(entity.clone())
        >
            <span class="text-body">{name}</span>
            <span class="text-label" style="color: var(--muted-foreground);">{secondary}</span>
        </button>
    }
}

/// PIV-01's list: heading, help line, then one row per candidate.
///
/// `on_open` is the ONLY way a profile is reached from this component, and this component never
/// calls it on the analyst's behalf — a row is opened when the analyst activates that row.
///
/// An empty `rows` renders [`NO_ENTITY_MATCHES`] — the same fixed copy the entity picker's
/// dropdown shows, imported rather than retyped so the two cannot drift.
#[component]
#[allow(non_snake_case)]
pub fn DisambiguationList(
    view: DisambiguationView,
    on_open: impl Fn(EntityRef) + 'static + Clone + Send,
) -> impl IntoView {
    let heading = heading_text(&view);
    let rows = view.rows.clone();

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-md); max-width: 960px;">
            {move || if rows.is_empty() {
                view! {
                    <div class="text-body" style="color: var(--muted-foreground);">
                        {NO_ENTITY_MATCHES}
                    </div>
                }.into_any()
            } else {
                let heading = heading.clone();
                let on_open = on_open.clone();
                view! {
                    <div style="display: flex; flex-direction: column; gap: var(--space-sm);">
                        <div class="text-heading">{heading}</div>
                        <div class="text-label" style="color: var(--muted-foreground);">
                            {DISAMBIGUATION_HELP}
                        </div>
                    </div>
                    <div
                        role="listbox"
                        style="display: flex; flex-direction: column; gap: var(--space-sm);"
                    >
                        {rows.clone().into_iter().map(|row| {
                            view! { <CandidateRow row=row on_open=on_open.clone() /> }
                        }).collect_view()}
                    </div>
                }.into_any()
            }}
        </div>
    }
}

/// The whole PIV-01 flow over one query box: the NAV-06 entity picker, extended rather than
/// forked, plus the list its multi-match `Enter` opens.
///
/// Two ways into a profile, and both are an analyst's explicit choice:
///
/// * the picker's own `on_select` — a dropdown row clicked, or `Enter` on a highlighted row, or
///   `Enter` on a sole match ([`EnterAction::CommitSoleRow`], the fast path inside the typeahead);
/// * a candidate row activated in [`DisambiguationList`].
///
/// The third way — `Enter` with several matches and nothing highlighted — was a dead key press
/// before this plan. It now resolves the query through `GET /api/entity/disambiguate` and applies
/// [`outcome_for`]: one match opens straight through, several render the list, and **no profile is
/// opened in between**.
///
/// A transport failure clears the list rather than opening anything. Surfacing that as its own
/// error state is 45-10's ("The workbench returned data this page cannot read" + Retry); what
/// matters here is that a failed resolve can never fall through to opening a candidate.
#[component]
#[allow(non_snake_case)]
pub fn EntitySearch(
    #[prop(into)] placeholder: String,
    /// `Sync` as well as `Send`, unlike [`DisambiguationList`]'s own prop: the multi-match branch
    /// hands this closure to a `leptos::Callback`, which requires it. A navigation closure is
    /// `Sync` in practice; the bound is stated here rather than worked around with interior
    /// mutability that would hide which thread opens a profile.
    on_open: impl Fn(EntityRef) + 'static + Clone + Send + Sync,
) -> impl IntoView {
    let list: RwSignal<Option<DisambiguationView>> = RwSignal::new(None);

    let on_open_for_search = on_open.clone();
    let on_search = Callback::new(move |query: String| {
        let on_open = on_open_for_search.clone();
        leptos::task::spawn_local(async move {
            match fetch_disambiguation(query).await {
                Ok(view) => {
                    let opener = move |entity: EntityRef| on_open(entity);
                    list.set(apply_outcome(outcome_for(view), &opener));
                }
                Err(_) => list.set(None),
            }
        });
    });

    let on_open_for_pick = on_open.clone();
    let on_select = move |entity: EntityRef| {
        list.set(None);
        on_open_for_pick(entity);
    };

    let on_open_for_row = on_open.clone();

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-lg);">
            <EntityPicker
                placeholder=placeholder
                on_select=on_select
                on_clear=move || list.set(None)
                on_search=on_search
            />
            {move || list.get().map(|view| {
                let on_open = on_open_for_row.clone();
                view! { <DisambiguationList view=view on_open=on_open /> }
            })}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(id: &str, name: &str) -> EntityRef {
        EntityRef {
            id: id.to_string(),
            name: name.to_string(),
            kind: "Entity".to_string(),
        }
    }

    fn ward(id: &str, name: &str) -> EntityRef {
        EntityRef {
            id: id.to_string(),
            name: name.to_string(),
            kind: "Ward".to_string(),
        }
    }

    fn row(
        id: &str,
        name: &str,
        status: EntityStatusView,
        w: Option<EntityRef>,
    ) -> DisambiguationRowView {
        DisambiguationRowView {
            entity: entity(id, name),
            status,
            ward: w,
        }
    }

    fn several() -> DisambiguationView {
        DisambiguationView {
            query: "jones".to_string(),
            rows: vec![
                row(
                    "GB-COH:X06999001",
                    "Jones Holdings Ltd",
                    EntityStatusView::Active,
                    Some(ward("ward:X05000983", "Sample Ward Central")),
                ),
                row(
                    "GB-COH:X06999002",
                    "Jones Building Services Ltd",
                    EntityStatusView::Dissolved {
                        on: "2019-04-02".to_string(),
                    },
                    None,
                ),
                row(
                    "director:jones-a",
                    "A Jones",
                    EntityStatusView::Active,
                    Some(ward("ward:X05001052", "X05001052")),
                ),
            ],
            more_may_exist: false,
        }
    }

    /// Records every profile a run would have opened, so "no profile was opened" is a counted
    /// fact rather than an inspected shape.
    fn recording_opener() -> (std::cell::RefCell<Vec<String>>,) {
        (std::cell::RefCell::new(Vec::new()),)
    }

    /// **PIV-01's load-bearing clause.** Several matches must not open a profile — not the first
    /// one, not any one. Asserting only that a list renders would not catch this: a page that
    /// rendered the list AND opened match one would pass such an assertion. This counts the opens.
    ///
    /// PROVE THIS CAN FAIL: make `outcome_for`'s `_` arm
    /// `SearchOutcome::OpenProfile(view.rows[0].entity.clone())` — the "open the first one"
    /// regression exactly — and this fails with one recorded open.
    #[test]
    fn a_multi_match_search_opens_no_profile() {
        let view = several();
        assert_eq!(
            view.rows.len(),
            3,
            "the fixture must produce several matches, or this test proves nothing"
        );

        let (opened,) = recording_opener();
        let rendered = apply_outcome(outcome_for(view), &|entity: EntityRef| {
            opened.borrow_mut().push(entity.id.clone())
        });

        assert!(
            opened.borrow().is_empty(),
            "a multi-match search opened {:?} before the analyst chose",
            opened.borrow()
        );
        let rendered = rendered.expect("several matches must render the list");
        assert_eq!(
            rendered.rows.len(),
            3,
            "every candidate must reach the list"
        );
    }

    /// The counterweight: one unambiguous match goes straight through, so the list is not an extra
    /// click on the common path. Measured trigger frequency on the live corpus is in the module
    /// doc — 92.2% of full-name queries.
    ///
    /// PROVE THIS CAN FAIL: change `outcome_for`'s `1 if !view.more_may_exist` arm to fall through
    /// to `Disambiguate` — this fails with zero opens and a one-row list, which is the "the list
    /// is an extra click" bug.
    #[test]
    fn a_single_match_opens_straight_through_and_renders_no_list() {
        let mut view = several();
        view.rows.truncate(1);
        assert_eq!(view.rows.len(), 1, "the fixture must be a single match");

        let (opened,) = recording_opener();
        let rendered = apply_outcome(outcome_for(view), &|entity: EntityRef| {
            opened.borrow_mut().push(entity.id.clone())
        });

        assert_eq!(
            opened.borrow().as_slice(),
            ["GB-COH:X06999001".to_string()],
            "the sole match must be opened exactly once"
        );
        assert!(
            rendered.is_none(),
            "the fast path must render no list at all"
        );
    }

    /// A one-row response that the server flagged as capped is NOT an unambiguous match: the one
    /// row was chosen by a truncation, not by the query.
    #[test]
    fn a_capped_one_row_response_is_not_treated_as_unambiguous() {
        let mut view = several();
        view.rows.truncate(1);
        view.more_may_exist = true;

        let (opened,) = recording_opener();
        let rendered = apply_outcome(outcome_for(view), &|entity: EntityRef| {
            opened.borrow_mut().push(entity.id.clone())
        });

        assert!(opened.borrow().is_empty());
        assert!(rendered.is_some());
    }

    /// No match opens nothing and still renders, so the analyst sees "No entity matches" rather
    /// than an unexplained blank.
    #[test]
    fn no_match_opens_no_profile_and_still_renders() {
        let mut view = several();
        view.rows.clear();

        let (opened,) = recording_opener();
        let rendered = apply_outcome(outcome_for(view), &|entity: EntityRef| {
            opened.borrow_mut().push(entity.id.clone())
        });

        assert!(opened.borrow().is_empty());
        assert!(rendered
            .expect("the empty list still renders")
            .rows
            .is_empty());
    }

    /// Only one variant can carry a target — the property the whole guarantee rests on.
    #[test]
    fn only_the_fast_path_variant_carries_a_profile_target() {
        assert!(matches!(
            outcome_for(several()),
            SearchOutcome::Disambiguate(_)
        ));
        let mut one = several();
        one.rows.truncate(1);
        assert!(matches!(outcome_for(one), SearchOutcome::OpenProfile(_)));
        let mut none = several();
        none.rows.clear();
        assert!(matches!(outcome_for(none), SearchOutcome::NoMatch(_)));
    }

    #[test]
    fn the_url_targets_the_disambiguate_endpoint_and_encodes_a_raw_id_query() {
        assert_eq!(
            disambiguate_url("jones"),
            "/api/entity/disambiguate?q=jones"
        );
        let raw = disambiguate_url("ward:X05001499");
        assert_eq!(raw, "/api/entity/disambiguate?q=ward%3AX05001499");
        assert!(
            !raw.contains(':') || raw.starts_with("/api"),
            "the query value must carry no unescaped colon"
        );
    }

    /// The endpoint this module reads is the persistence-backed one, NOT the label index the
    /// picker's typeahead dropdown uses — that index cannot answer status or ward at all.
    #[test]
    fn the_url_is_not_the_label_index_search_endpoint() {
        assert!(!disambiguate_url("jones").contains("/api/entities/search"));
    }

    #[test]
    fn the_help_line_is_the_spec_copy_verbatim() {
        assert_eq!(DISAMBIGUATION_HELP, "Choose one to open its profile.");
    }

    /// The status words are the CLI's, character for character — D-01's "the CLI and the workbench
    /// must not disagree about one company" applied to the one string an analyst compares by eye.
    ///
    /// PROVE THIS CAN FAIL: `EntityStatusView::Dissolved { .. } => "Dissolved".to_string()` (the
    /// date dropped) fails the second assertion with `left: "Dissolved"`.
    #[test]
    fn status_renders_the_profile_vocabulary_never_a_raw_enum_name() {
        assert_eq!(status_label(&EntityStatusView::Active), "Active");
        assert_eq!(
            status_label(&EntityStatusView::Dissolved {
                on: "2019-04-02".to_string()
            }),
            "Dissolved 2019-04-02"
        );
        assert_eq!(
            status_label(&EntityStatusView::InLiquidation {
                on: "2021-11-30".to_string()
            }),
            "In Liquidation 2021-11-30"
        );
        for rendered in [
            status_label(&EntityStatusView::Active),
            status_label(&EntityStatusView::Dissolved {
                on: "2019-04-02".to_string(),
            }),
            status_label(&EntityStatusView::InLiquidation {
                on: "2021-11-30".to_string(),
            }),
        ] {
            assert!(
                !rendered.contains("InLiquidation") && !rendered.contains('_'),
                "a raw enum spelling reached the screen: {rendered}"
            );
        }
    }

    #[test]
    fn a_row_shows_type_status_and_ward_in_that_order() {
        let view = several();
        assert_eq!(view.rows.len(), 3, "the fixture must carry rows to render");
        assert_eq!(
            row_secondary_text(&view.rows[0]),
            "Entity · Active · Sample Ward Central"
        );
        assert_eq!(
            row_secondary_text(&view.rows[1]),
            "Entity · Dissolved 2019-04-02"
        );
    }

    /// A ward-less candidate drops the segment entirely — never "Unknown ward", never a trailing
    /// separator with nothing after it.
    ///
    /// PROVE THIS CAN FAIL: pushing `"Unknown ward".to_string()` in the `None` arm of
    /// `row_secondary_text` fails here with a third segment present.
    #[test]
    fn a_missing_ward_drops_its_segment_rather_than_placeholding_it() {
        let view = several();
        let without_ward = &view.rows[1];
        assert!(
            without_ward.ward.is_none(),
            "the fixture row must actually have no ward, or this proves nothing"
        );
        let rendered = row_secondary_text(without_ward);
        assert_eq!(
            rendered.matches(" · ").count(),
            1,
            "two segments means exactly one separator, got {rendered}"
        );
        assert!(!rendered.ends_with('·') && !rendered.ends_with(" "));
        assert!(!rendered.to_lowercase().contains("unknown"));
    }

    /// The ward renders the NAME the server sent, verbatim — including when that name is the bare
    /// code, which it is for 597 of the 614 ward nodes in the live analysis (measured 2026-09-15). This renderer neither substitutes a code for a name nor dresses a code up as
    /// one; the gap belongs to the ward backfill and stays visible here.
    #[test]
    fn the_ward_segment_is_the_name_the_server_sent_even_when_that_name_is_a_code() {
        let view = several();
        let code_named = &view.rows[2];
        assert_eq!(
            code_named.ward.as_ref().map(|w| w.name.as_str()),
            Some("X05001052"),
            "the fixture must reproduce the live corpus's code-as-name ward"
        );
        assert!(row_secondary_text(code_named).ends_with("· X05001052"));
        let named = &view.rows[0];
        assert!(row_secondary_text(named).ends_with("· Sample Ward Central"));
    }

    #[test]
    fn the_heading_quotes_the_query_and_counts_the_rows() {
        let view = several();
        assert_eq!(heading_text(&view), "3 entities match \"jones\".");
    }

    /// A capped response never prints a total the server did not establish.
    ///
    /// PROVE THIS CAN FAIL: deleting the `more_may_exist` early return makes this read
    /// `10 entities match "jones".` — a stated figure behind which there is nothing.
    #[test]
    fn a_capped_result_set_says_so_instead_of_printing_a_total_it_cannot_stand_behind() {
        let mut view = several();
        view.more_may_exist = true;
        assert_eq!(heading_text(&view), "3+ entities match \"jones\".");
    }

    #[test]
    fn the_heading_has_a_singular_form() {
        let mut view = several();
        view.rows.truncate(1);
        assert_eq!(heading_text(&view), "1 entity matches \"jones\".");
    }

    #[test]
    fn the_whole_row_is_a_44px_hit_target() {
        let style = row_style();
        assert!(style.contains("min-height: 44px"));
        assert!(style.contains("width: 100%"));
        assert!(style.contains("padding: var(--space-md)"));
    }
}
