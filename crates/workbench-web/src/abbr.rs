//! `Abbr` — the shared `<abbr>` tooltip component consuming Plan 03's glossary parser, per
//! the phase 43 UI spec's "`<abbr>` tooltip" section (D-03, D-04, NAV-04).
//!
//! Module-wide `#![allow(dead_code)]`: interface-first plan, Wave 3 page plans 06-08 are `Abbr`'s
//! first real callers — see `timestamp.rs`'s module doc for why a per-item `#[allow(dead_code)]`
//! on the `Abbr` fn cannot suppress the macro-generated `AbbrProps` struct's unused `token` field.
#![allow(dead_code)]

use crate::glossary;
use leptos::prelude::*;

/// The popover/tooltip text for a glossary token: `"{meaning} — {notes}"`, or just `"{meaning}"`
/// if `notes` is empty. Returns `None` for a token the glossary does not have — the component
/// must never fabricate an expansion or render a tooltip for unrecognised input; this is a
/// defensive fallback for genuinely-unexpected input, not a rule the CI guard (Plan 03) is
/// expected to let happen in production copy.
// Real consumer is the `Abbr` component below; nothing in `App`'s tree renders `Abbr` yet (Wave
// 3 page plans 06-08 are its first callers) — matching `glossary.rs`'s established
// `#[allow(dead_code)]` convention for interface-first plans in this phase.
#[allow(dead_code)]
pub fn abbr_expansion_text(token: &str) -> Option<String> {
    let row = glossary::lookup(token)?;
    if row.notes.is_empty() {
        Some(row.meaning.to_string())
    } else {
        Some(format!("{} — {}", row.meaning, row.notes))
    }
}

/// Renders `token` wrapped in `<abbr title=...>`, dotted-underline styled, inside a focusable
/// `<span tabindex="0">` so keyboard focus and tap (not just native hover) surface the expansion,
/// per the phase 43 UI spec. A small popover mirrors the `title` text on `:focus-visible`/click-toggle,
/// dismissed on blur/outside-click. If the token isn't in the glossary, renders the bare token
/// with no `<abbr>` wrapper (defensive fallback — the CI guard should already have caught this).
#[component]
#[allow(non_snake_case)]
pub fn Abbr(token: String) -> impl IntoView {
    let expansion = abbr_expansion_text(&token);
    let show_popover = RwSignal::new(false);

    match expansion {
        Some(title_text) => {
            let title_for_popover = title_text.clone();
            view! {
                <span
                    tabindex="0"
                    style="position: relative; display: inline-block;"
                    on:click=move |_| show_popover.update(|v| *v = !*v)
                    on:blur=move |_| show_popover.set(false)
                    on:focusout=move |_| show_popover.set(false)
                >
                    <abbr
                        title=title_text
                        class="abbr"
                        style="text-decoration: underline dotted; text-decoration-color: var(--muted-foreground); cursor: help;"
                    >
                        {token}
                    </abbr>
                    {move || {
                        show_popover.get().then(|| {
                            view! {
                                <span
                                    class="text-label"
                                    style="position: absolute; bottom: 100%; left: 0; z-index: 10; background: var(--card); border: 1px solid var(--border); border-radius: 6px; padding: var(--space-xs) var(--space-sm); white-space: nowrap; box-shadow: 0 2px 8px rgba(0,0,0,0.15);"
                                >
                                    {title_for_popover.clone()}
                                </span>
                            }
                        })
                    }}
                </span>
            }
            .into_any()
        }
        None => view! { <span>{token}</span> }.into_any(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbr_expansion_text_ch_contains_meaning_and_note() {
        let text = abbr_expansion_text("CH").expect("CH must be present in the real glossary");
        assert!(
            text.contains("Companies House"),
            "expansion text was: {text}"
        );
        assert!(
            text.contains(" — "),
            "expansion text must be formatted as '{{meaning}} — {{note}}': {text}"
        );
    }

    /// PROVE THIS CAN FAIL: an earlier version of `abbr_expansion_text` fabricated a fallback
    /// string (`"Unknown abbreviation"`) for any token `glossary::lookup` didn't recognise. This
    /// test asserts the correct behaviour: `None`, never a fabricated expansion.
    #[test]
    fn abbr_expansion_text_returns_none_for_unknown_token() {
        assert_eq!(abbr_expansion_text("ZZQX"), None);
    }
}
