//! `EntityPicker` — the typeahead entity picker (NAV-06, the phase 44 UI spec's "Entity picker"
//! component contract), a standalone, reusable component the tool rail (a later plan) mounts
//! three instances of (From, To, Ward). Depends only on `GET /api/entities/search` (plan 44-08)
//! and the `EntityRef` DTO (plan 44-01) — never on `graph_page.rs`'s canvas state.
//!
//! **Scope-agnostic by design (T-44-11-02, accept disposition):** this component performs no
//! population/out-of-scope check. It resolves a typed name/code/raw id to an `EntityRef` and
//! reports the pick to the caller via `on_select` — the caller (a later tool-rail plan, which
//! already holds the Arrow-derived population id set from `graph_page.rs`) decides in/out-of-scope
//! and renders the out-of-scope error copy beneath the field.
//!
//! **Debounce (T-44-11-01, mitigate):** the per-keystroke search is debounced via a
//! cancel-and-reschedule `set_timeout` (no new dependency, no existing in-crate debounce helper),
//! bounding request frequency to roughly one per pause in typing rather than one per keystroke.
//!
//! Module-wide `#![allow(dead_code)]`: interface-first plan, mirroring `entity_ref_view.rs`'s own
//! module doc — the tool rail (a later plan) is `EntityPicker`'s first real caller. A per-item
//! `#[allow(dead_code)]` on the `EntityPicker` fn cannot suppress the macro-generated
//! `EntityPickerProps` struct's unused fields, so the allow lives at module scope instead.
#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use leptos::prelude::*;
use urban_integrity_workbench_dto::EntityRef;

use crate::entity_ref_view::EntityRefView;

/// The fixed "no match" dropdown-body copy (NAV-06, verbatim, locked). Rendered in place of the
/// row list — never an empty dropdown with nothing in it.
pub const NO_ENTITY_MATCHES: &str = "No entity matches";

/// Monotonic per-instance id source so multiple `EntityPicker` mounts (From/To/Ward) never share
/// `aria-controls`/`aria-activedescendant`/row `id` DOM identifiers.
static PICKER_SEQ: AtomicUsize = AtomicUsize::new(0);

fn next_picker_id() -> String {
    let n = PICKER_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("entity-picker-{n}")
}

/// Percent-encodes every byte outside RFC 3986's `unreserved` set (`ALPHA` / `DIGIT` / `-` / `.` /
/// `_` / `~`) as `%XX` (uppercase hex) — a raw-id-shaped query such as `ward:X05001499` embeds a
/// colon safely without ever risking a broken query string.
///
/// `pub(crate)` because `disambiguation.rs` builds its own endpoint's query string from the SAME
/// typed string this picker resolves (PIV-01's list and NAV-06's dropdown answer one query box).
/// Sharing this function is the difference between one encoder and two chances to break a query.
pub(crate) fn percent_encode_query_value(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{byte:02X}"));
            }
        }
    }
    out
}

/// `GET /api/entities/search?q=` (plan 44-08) — the fetch URL for a given typed query.
pub fn entity_search_url(query: &str) -> String {
    format!(
        "/api/entities/search?q={}",
        percent_encode_query_value(query)
    )
}

/// Fetches and deserialises the typeahead result set from [`entity_search_url`]. An empty/no-match
/// server response is a `200` with an empty JSON array (plan 44-08's own contract) — never an
/// `Err` here; `Err` is reserved for a genuine transport/shape failure.
pub async fn fetch_entity_search(query: String) -> Result<Vec<EntityRef>, String> {
    let response = gloo_net::http::Request::get(&entity_search_url(&query))
        .send()
        .await
        .map_err(|err| err.to_string())?;
    if !response.ok() {
        return Err(format!(
            "entities search responded with status {}",
            response.status()
        ));
    }
    response
        .json::<Vec<EntityRef>>()
        .await
        .map_err(|err| err.to_string())
}

/// The direction a keyboard `ArrowUp`/`ArrowDown` press moves the dropdown's roving highlight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

/// Pure, host-testable row-highlight-index transition: `len == 0` -> `None` (nothing to
/// highlight); no current highlight -> the first row (`Down`) or the last row (`Up`); otherwise
/// increment/decrement with wraparound at either end (the phase 44 UI spec's "Keyboard" contract:
/// "ArrowDown/ArrowUp moves a visible highlight through dropdown rows (wrapping at either end)").
pub fn next_highlight(current: Option<usize>, len: usize, direction: Direction) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match (current, direction) {
        (None, Direction::Down) => Some(0),
        (None, Direction::Up) => Some(len - 1),
        (Some(i), Direction::Down) => Some((i + 1) % len),
        (Some(i), Direction::Up) => Some(if i == 0 { len - 1 } else { i - 1 }),
    }
}

/// What pressing `Enter` in the dropdown means, as a value rather than as control flow buried in
/// the key handler.
///
/// Naming it exposes the branch PIV-01 turns on. Three of the four cases were already the picker's
/// behaviour; the fourth was **dead**, and its deadness was the defect: with several rows matching
/// and none highlighted, `Enter` did nothing at all. That is precisely the moment an analyst has
/// asked a question the typeahead cannot answer on its own, and PIV-01's disambiguation list is
/// the answer to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnterAction {
    /// Commit the row at this index — an explicit pick.
    Commit(usize),
    /// **The single-match fast path.** Exactly one row matches and the analyst highlighted
    /// nothing, so there is nothing to disambiguate: the sole row IS the answer, and it is
    /// committed without a list ever being rendered (the phase 45 UI spec: "A single unambiguous match
    /// navigates straight to the profile. The list is not an extra click on the common path").
    CommitSoleRow,
    /// Several rows match and none is highlighted — resolve the typed query as a whole, which is
    /// what opens PIV-01's list.
    Search,
    /// Nothing matched; `Enter` is inert.
    Ignore,
}

impl EnterAction {
    /// The row index this action commits, if it commits one.
    pub fn commit_index(self) -> Option<usize> {
        match self {
            EnterAction::Commit(i) => Some(i),
            EnterAction::CommitSoleRow => Some(0),
            EnterAction::Search | EnterAction::Ignore => None,
        }
    }
}

/// Pure, host-testable `Enter` decision: an explicit highlight always wins; with no highlight,
/// exactly one row is the fast path, several rows are a search, and none is inert.
pub fn enter_action(highlight: Option<usize>, len: usize) -> EnterAction {
    match (highlight, len) {
        (Some(i), _) => EnterAction::Commit(i),
        (None, 0) => EnterAction::Ignore,
        (None, 1) => EnterAction::CommitSoleRow,
        (None, _) => EnterAction::Search,
    }
}

/// Splits a dropdown row into its `(name, "kind · id")` display fragments — `name` renders in Body
/// size, `kind`/`id` in Label size `--muted-foreground` (the phase 44 UI spec's `name · kind · id` row
/// shape).
///
/// **Ward display rule, corrected.** This comment used to say a Ward-kind `EntityRef.name` "is
/// already the ward's *name*, never the bare code, because the server-side label index (plan
/// 44-06's `label_index.rs`) guarantees it". The label index guarantees no such thing and never
/// did: it fills `name` from `node_display_name`, which returns `WardData.name` verbatim, so a
/// ward node holding its own code as its name renders that code here. Measured on the live
/// analysis, 2026-09-15: **597 of 614 ward nodes carry a `name` equal to their own bare code**, so
/// the code is what an analyst sees for 97.2% of wards today.
///
/// What is true is the narrower statement: this function performs no ward-specific branching and
/// never substitutes a code for a name. The defect is upstream, in whatever writes a ward node
/// with no name (`crates/ingestor/src/backfill/ward.rs`); the test below pins the pass-through,
/// not a guarantee about the data.
fn dropdown_row_parts(entity: &EntityRef) -> (String, String) {
    (
        entity.name.clone(),
        format!("{} · {}", entity.kind, entity.id),
    )
}

/// A dropdown row's inline style — `--card-hover` background when the roving keyboard highlight is
/// on this row, transparent otherwise (mirrors `analysis_picker.rs`'s `row_style` shape).
fn row_style(highlighted: bool) -> String {
    let background = if highlighted {
        "var(--card-hover)"
    } else {
        "transparent"
    };
    format!(
        "display: flex; align-items: baseline; gap: var(--space-xs); padding: var(--space-sm); border-radius: 6px; cursor: pointer; min-height: 44px; background: {background};"
    )
}

/// Clears any in-flight debounce timer (T-44-11-01's cancel-and-reschedule mitigation) — safe to
/// call whether or not a timer is currently pending.
fn clear_pending_timer(pending_timer: RwSignal<Option<TimeoutHandle>>) {
    if let Some(handle) = pending_timer.get_untracked() {
        handle.clear();
    }
    pending_timer.set(None);
}

/// Kicks off a debounced search: fetches [`fetch_entity_search`] for `query_text` and stores the
/// result (or an empty list on transport failure, matching the "no match" visual — a failed fetch
/// is not surfaced as a distinct error state in this phase's scope), resetting the keyboard
/// highlight for the fresh result set.
fn trigger_search(
    query_text: String,
    results: RwSignal<Option<Vec<EntityRef>>>,
    highlight: RwSignal<Option<usize>>,
) {
    leptos::task::spawn_local(async move {
        let rows = fetch_entity_search(query_text).await.unwrap_or_default();
        results.set(Some(rows));
        highlight.set(None);
    });
}

/// Commits a resolved pick: cancels any pending debounce, sets the resolved chip, discards the raw
/// typed query and dropdown state (the phase 44 UI spec: "The raw typed query is discarded on selection —
/// the chip is the only display"), then reports the pick to the caller via `on_select` so the
/// caller can perform its own out-of-scope check (T-44-11-02).
fn commit_selection(
    entity: EntityRef,
    query: RwSignal<String>,
    results: RwSignal<Option<Vec<EntityRef>>>,
    highlight: RwSignal<Option<usize>>,
    pending_timer: RwSignal<Option<TimeoutHandle>>,
    resolved: RwSignal<Option<EntityRef>>,
    on_select: &impl Fn(EntityRef),
) {
    clear_pending_timer(pending_timer);
    resolved.set(Some(entity.clone()));
    query.set(String::new());
    results.set(None);
    highlight.set(None);
    on_select(entity);
}

/// One dropdown row: `role="option"`, `aria-selected` tracking the roving highlight, click commits
/// the selection via [`commit_selection`].
#[component]
#[allow(non_snake_case, clippy::too_many_arguments)]
fn ResultRow(
    entity: EntityRef,
    index: usize,
    picker_id: String,
    highlight: RwSignal<Option<usize>>,
    query: RwSignal<String>,
    results: RwSignal<Option<Vec<EntityRef>>>,
    pending_timer: RwSignal<Option<TimeoutHandle>>,
    resolved: RwSignal<Option<EntityRef>>,
    on_select: impl Fn(EntityRef) + 'static,
) -> impl IntoView {
    let (name, kind_and_id) = dropdown_row_parts(&entity);
    let entity_for_click = entity;
    let is_highlighted = move || highlight.get() == Some(index);

    view! {
        <div
            id=format!("{picker_id}-option-{index}")
            role="option"
            aria-selected=move || is_highlighted().to_string()
            style=move || row_style(is_highlighted())
            on:click=move |_| {
                commit_selection(
                    entity_for_click.clone(),
                    query,
                    results,
                    highlight,
                    pending_timer,
                    resolved,
                    &on_select,
                );
            }
        >
            <span class="text-body">{name}</span>
            <span class="text-label" style="color: var(--muted-foreground);">{kind_and_id}</span>
        </div>
    }
}

/// The Empty/Typing states: the `<input role="combobox">` plus, once a debounced search resolves,
/// the dropdown (rows, or the fixed [`NO_ENTITY_MATCHES`] copy on an empty result).
#[allow(clippy::too_many_arguments)]
fn typing_view(
    picker_id: String,
    listbox_id: String,
    placeholder: String,
    query: RwSignal<String>,
    results: RwSignal<Option<Vec<EntityRef>>>,
    highlight: RwSignal<Option<usize>>,
    resolved: RwSignal<Option<EntityRef>>,
    pending_timer: RwSignal<Option<TimeoutHandle>>,
    on_select: impl Fn(EntityRef) + 'static + Clone + Send,
) -> impl IntoView {
    let on_input = move |ev: web_sys::Event| {
        let value = event_target_value(&ev);
        query.set(value.clone());
        clear_pending_timer(pending_timer);
        if value.trim().is_empty() {
            results.set(None);
            highlight.set(None);
            return;
        }
        let handle = set_timeout_with_handle(
            move || trigger_search(value.clone(), results, highlight),
            Duration::from_millis(250),
        );
        if let Ok(handle) = handle {
            pending_timer.set(Some(handle));
        }
    };

    let picker_id_for_activedescendant = picker_id.clone();
    let listbox_id_for_aria = listbox_id.clone();

    view! {
        <div style="position: relative;">
            <input
                type="text"
                role="combobox"
                aria-expanded=move || (!query.get().trim().is_empty() && results.get().is_some()).to_string()
                aria-controls=listbox_id_for_aria
                aria-activedescendant=move || {
                    highlight
                        .get()
                        .map(|i| format!("{picker_id_for_activedescendant}-option-{i}"))
                        .unwrap_or_default()
                }
                placeholder=placeholder.clone()
                prop:value=move || query.get()
                on:input=on_input
                style="width: 100%; box-sizing: border-box; padding: var(--space-xs) var(--space-sm); background: var(--card); border: 1px solid var(--border); border-radius: 6px; color: var(--foreground); min-height: 44px;"
            />
            {move || {
                if query.get().trim().is_empty() {
                    return ().into_any();
                }
                match results.get() {
                    None => ().into_any(),
                    Some(rows) if rows.is_empty() => view! {
                        <div
                            id=listbox_id.clone()
                            role="listbox"
                            style="position: absolute; top: calc(100% + 2px); left: 0; right: 0; background: var(--card); border: 1px solid var(--border); border-radius: 6px; padding: var(--space-sm); z-index: 10;"
                        >
                            <div class="text-body" style="color: var(--muted-foreground); text-align: center;">
                                {NO_ENTITY_MATCHES}
                            </div>
                        </div>
                    }.into_any(),
                    Some(rows) => {
                        let picker_id = picker_id.clone();
                        let on_select = on_select.clone();
                        view! {
                            <div
                                id=listbox_id.clone()
                                role="listbox"
                                style="position: absolute; top: calc(100% + 2px); left: 0; right: 0; max-height: 400px; overflow-y: auto; background: var(--card); border: 1px solid var(--border); border-radius: 6px; padding: var(--space-xs); z-index: 10; display: flex; flex-direction: column; gap: 2px;"
                            >
                                {rows.into_iter().take(20).enumerate().map(|(i, entity)| {
                                    view! {
                                        <ResultRow
                                            entity=entity
                                            index=i
                                            picker_id=picker_id.clone()
                                            highlight=highlight
                                            query=query
                                            results=results
                                            pending_timer=pending_timer
                                            resolved=resolved
                                            on_select=on_select.clone()
                                        />
                                    }
                                }).collect_view()}
                            </div>
                        }.into_any()
                    }
                }
            }}
        </div>
    }
}

/// The typeahead entity picker (NAV-06): resolves a typed name, code or raw id to `name · kind ·
/// id` via [`entity_search_url`]/[`fetch_entity_search`], collapsing to a read-only
/// `EntityRefView` chip on selection. `on_select` fires exactly once per resolved pick, at the
/// moment of selection (click, `Enter`, or the sole row) — the caller performs any out-of-scope
/// check (T-44-11-02).
#[component]
#[allow(non_snake_case)]
pub fn EntityPicker(
    #[prop(into)] placeholder: String,
    on_select: impl Fn(EntityRef) + 'static + Clone + Send,
    /// Called when the resolved chip is cleared (the `×` button, or `Escape` on a resolved chip).
    /// WITHOUT this the caller never learns the pick was undone: clearing reset only the picker's
    /// own `resolved`/`query`/`results` signals, so the tool rail's `from_ref`/`to_ref` kept the
    /// previous entity. Run therefore stayed enabled and re-ran the OLD endpoints, which looked
    /// exactly like "clicking Run does nothing" (operator QA 2026-09-12).
    on_clear: impl Fn() + 'static + Clone + Send,
    /// Called with the raw typed query when `Enter` cannot resolve a single row — see
    /// [`EnterAction::Search`]. `None` (the default) leaves the previously dead branch dead, which
    /// is what the tool rail's three From/To/Ward pickers want: they resolve one endpoint each and
    /// have no list to open. `disambiguation.rs` supplies it, and that callback is how PIV-01's
    /// list is reached.
    #[prop(optional)]
    on_search: Option<Callback<String>>,
) -> impl IntoView {
    let picker_id = next_picker_id();
    let listbox_id = format!("{picker_id}-listbox");

    let query: RwSignal<String> = RwSignal::new(String::new());
    let results: RwSignal<Option<Vec<EntityRef>>> = RwSignal::new(None);
    let highlight: RwSignal<Option<usize>> = RwSignal::new(None);
    let resolved: RwSignal<Option<EntityRef>> = RwSignal::new(None);
    let pending_timer: RwSignal<Option<TimeoutHandle>> = RwSignal::new(None);

    let on_select_for_keydown = on_select.clone();
    let on_clear_for_keydown = on_clear.clone();

    // `role="combobox"`/listbox keyboard model (the phase 44 UI spec "Keyboard"): ArrowDown/ArrowUp move
    // the roving highlight (wrapping at either end via `next_highlight`); Enter selects the
    // highlighted row (or the sole row if exactly one match and nothing explicitly highlighted);
    // Escape closes the dropdown without selecting, or clears the resolved chip back to Empty if
    // pressed while a chip is shown; Tab moves focus, discarding a half-typed, non-highlighted
    // query. Bound on the wrapping `<div>` so it covers both the input (Typing) and the clear
    // button (Resolved chip) via event bubbling.
    let on_keydown = move |ev: web_sys::KeyboardEvent| match ev.key().as_str() {
        "ArrowDown" => {
            ev.prevent_default();
            let len = results.get_untracked().map(|r| r.len()).unwrap_or(0);
            highlight.set(next_highlight(
                highlight.get_untracked(),
                len,
                Direction::Down,
            ));
        }
        "ArrowUp" => {
            ev.prevent_default();
            let len = results.get_untracked().map(|r| r.len()).unwrap_or(0);
            highlight.set(next_highlight(
                highlight.get_untracked(),
                len,
                Direction::Up,
            ));
        }
        "Enter" => {
            ev.prevent_default();
            let rows = results.get_untracked().unwrap_or_default();
            let action = enter_action(highlight.get_untracked(), rows.len());
            let selected = action.commit_index().and_then(|i| rows.get(i).cloned());
            if let Some(entity) = selected {
                commit_selection(
                    entity,
                    query,
                    results,
                    highlight,
                    pending_timer,
                    resolved,
                    &on_select_for_keydown,
                );
            } else if action == EnterAction::Search {
                // The typed query goes to the caller UNCHANGED and the field is NOT cleared: the
                // disambiguation heading quotes the query back, so discarding it here would leave
                // the list unable to say what it is a list of.
                if let Some(on_search) = on_search {
                    on_search.run(query.get_untracked());
                }
            }
        }
        "Escape" => {
            let had_chip = resolved.get_untracked().is_some();
            clear_pending_timer(pending_timer);
            if had_chip {
                resolved.set(None);
                query.set(String::new());
                // Same contract as the `×` button: a cleared chip must reach the caller, or its
                // `from_ref`/`to_ref` silently keeps an entity the user has just removed.
                on_clear_for_keydown();
            }
            results.set(None);
            highlight.set(None);
        }
        "Tab" => {
            clear_pending_timer(pending_timer);
            query.set(String::new());
            results.set(None);
            highlight.set(None);
        }
        _ => {}
    };

    view! {
        <div style="position: relative;" on:keydown=on_keydown>
            {move || match resolved.get() {
                Some(entity) => {
                    let on_clear_for_click = on_clear.clone();
                    let on_clear_click = move |_| {
                        clear_pending_timer(pending_timer);
                        resolved.set(None);
                        query.set(String::new());
                        results.set(None);
                        highlight.set(None);
                        on_clear_for_click();
                    };
                    view! {
                        <span style="display: inline-flex; align-items: center; gap: var(--space-xs);">
                            // `no_link=true`: this chip is the form control's current VALUE, not
                            // a reference to a fact — it sits against its own "Clear" button, and a
                            // mis-click that navigated out would discard a half-composed graph
                            // query. The analyst arrived here by searching for this entity; the
                            // pivot they want is on the results the query returns.
                            // `tests/pivot_reachable.rs` pins this as the ONLY suppression.
                            <EntityRefView entity=entity.clone() no_link=true />
                            <button
                                aria-label="Clear"
                                on:click=on_clear_click
                                style="min-width: 44px; min-height: 44px; display: inline-flex; align-items: center; justify-content: center; background: transparent; border: 1px solid var(--border); border-radius: 6px; color: var(--muted-foreground); cursor: pointer;"
                            >
                                "×"
                            </button>
                        </span>
                    }.into_any()
                }
                None => typing_view(
                    picker_id.clone(),
                    listbox_id.clone(),
                    placeholder.clone(),
                    query,
                    results,
                    highlight,
                    resolved,
                    pending_timer,
                    on_select.clone(),
                ).into_any(),
            }}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> EntityRef {
        EntityRef {
            id: "director:jane-smith".to_string(),
            name: "Jane Smith".to_string(),
            kind: "Entity".to_string(),
        }
    }

    fn ward() -> EntityRef {
        EntityRef {
            id: "ward:X05001499".to_string(),
            name: "Sample Ward Central".to_string(),
            kind: "Ward".to_string(),
        }
    }

    #[test]
    fn entity_search_url_encodes_plain_query() {
        assert_eq!(entity_search_url("jane"), "/api/entities/search?q=jane");
    }

    /// A raw-id-shaped query (`ward:X05001499`) carries a colon — percent-encoded here, never
    /// breaking the query string.
    #[test]
    fn entity_search_url_percent_encodes_colon_in_raw_id_shaped_query() {
        let url = entity_search_url("ward:X05001499");
        assert_eq!(url, "/api/entities/search?q=ward%3AX05001499");
        assert!(
            !url.contains(':'),
            "the colon must not appear unescaped in the query string"
        );
    }

    #[test]
    fn no_entity_matches_copy_is_exact() {
        assert_eq!(NO_ENTITY_MATCHES, "No entity matches");
    }

    /// The fast path, named. Exactly one match with nothing highlighted commits that row — no
    /// list, no extra key press.
    ///
    /// PROVE THIS CAN FAIL: change the `(None, 1)` arm of `enter_action` to
    /// `EnterAction::Search` — this test fails with `Search`, and the single unambiguous match
    /// becomes an extra click on the common path.
    #[test]
    fn enter_commits_the_sole_row_when_nothing_is_highlighted() {
        assert_eq!(enter_action(None, 1), EnterAction::CommitSoleRow);
        assert_eq!(enter_action(None, 1).commit_index(), Some(0));
    }

    /// The branch that used to be dead: several matches and no highlight is a SEARCH, never a
    /// commit of row zero. A picker that committed the first row here would open a profile the
    /// analyst never chose, which is the exact regression PIV-01 exists to prevent.
    ///
    /// PROVE THIS CAN FAIL: change the `(None, _)` arm to `EnterAction::Commit(0)` — the
    /// `commit_index` assertion fails with `Some(0)`.
    #[test]
    fn enter_with_several_matches_and_no_highlight_searches_rather_than_committing_the_first() {
        for len in [2usize, 3, 20] {
            assert_eq!(enter_action(None, len), EnterAction::Search, "len {len}");
            assert_eq!(
                enter_action(None, len).commit_index(),
                None,
                "a multi-match Enter must commit nothing (len {len})"
            );
        }
    }

    #[test]
    fn enter_honours_an_explicit_highlight_and_is_inert_with_no_rows() {
        assert_eq!(enter_action(Some(2), 5), EnterAction::Commit(2));
        assert_eq!(enter_action(Some(2), 5).commit_index(), Some(2));
        assert_eq!(enter_action(None, 0), EnterAction::Ignore);
        assert_eq!(enter_action(None, 0).commit_index(), None);
    }

    #[test]
    fn next_highlight_returns_none_for_empty_results() {
        assert_eq!(next_highlight(None, 0, Direction::Down), None);
        assert_eq!(next_highlight(Some(0), 0, Direction::Up), None);
    }

    #[test]
    fn next_highlight_starts_at_first_row_on_first_arrow_down() {
        assert_eq!(next_highlight(None, 5, Direction::Down), Some(0));
    }

    #[test]
    fn next_highlight_starts_at_last_row_on_first_arrow_up() {
        assert_eq!(next_highlight(None, 5, Direction::Up), Some(4));
    }

    /// ArrowDown past the last row wraps to the first (the phase 44 UI spec: "wrapping at either end").
    ///
    /// PROVE THIS CAN FAIL: run against a version of `next_highlight` that saturates instead of
    /// wraps (`Some(i) if i + 1 >= len => Some(len - 1)`) — this assertion fails with `Some(4)`
    /// instead of `Some(0)` before the wraparound arm is corrected to `(i + 1) % len`.
    #[test]
    fn next_highlight_wraps_from_last_row_to_first_on_arrow_down() {
        assert_eq!(next_highlight(Some(4), 5, Direction::Down), Some(0));
    }

    #[test]
    fn next_highlight_wraps_from_first_row_to_last_on_arrow_up() {
        assert_eq!(next_highlight(Some(0), 5, Direction::Up), Some(4));
    }

    #[test]
    fn next_highlight_increments_and_decrements_within_bounds() {
        assert_eq!(next_highlight(Some(1), 5, Direction::Down), Some(2));
        assert_eq!(next_highlight(Some(2), 5, Direction::Up), Some(1));
    }

    /// Ward display rule regression guard: whatever the label index put in `name` reaches the row's
    /// name fragment unchanged, and the id fragment carries the id.
    ///
    /// The fixture names a ward, so this proves the pass-through — it does NOT prove that live ward
    /// nodes carry names, which they mostly do not (see `dropdown_row_parts`' corrected comment).
    #[test]
    fn dropdown_row_parts_uses_ward_name_not_bare_code_for_ward_kind_entity() {
        let (name, kind_and_id) = dropdown_row_parts(&ward());
        assert_eq!(name, "Sample Ward Central");
        assert!(kind_and_id.contains("ward:X05001499"));
        assert!(
            !name.contains("X05001499"),
            "the ward's name must render, never the bare code"
        );
    }

    #[test]
    fn dropdown_row_parts_formats_kind_and_id_for_a_plain_entity() {
        let (name, kind_and_id) = dropdown_row_parts(&sample());
        assert_eq!(name, "Jane Smith");
        assert_eq!(kind_and_id, "Entity · director:jane-smith");
    }

    #[test]
    fn row_style_applies_card_hover_only_when_highlighted() {
        assert!(row_style(true).contains("var(--card-hover)"));
        assert!(!row_style(false).contains("var(--card-hover)"));
        assert!(row_style(false).contains("transparent"));
    }
}
