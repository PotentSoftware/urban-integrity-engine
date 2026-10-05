//! `CopyButton` — the shared clipboard copy-to-clipboard affordance (D-01), used inline by
//! `EntityRefView` and by ward-code rendering, per the phase 43 UI spec's "Copyable identifier"
//! section.

use leptos::prelude::*;
use std::time::Duration;
use wasm_bindgen_futures::JsFuture;

/// The button's visible label for the given `just_copied` state — `"Copy id"` at rest, `"Copied"`
/// for the 2 seconds immediately after a successful clipboard write.
pub fn copy_button_label(just_copied: bool) -> &'static str {
    if just_copied {
        "Copied"
    } else {
        "Copy id"
    }
}

/// Writes `value` to the OS clipboard and renders the label-state transition (the phase 43 UI spec
/// "Copyable identifier": click copies the raw `id`, button label swaps to "Copied" for 2 seconds
/// then reverts, no toast, no modal). `aria-label="Copy id"` is constant — the visible label swap
/// already announces the state change. 44x44px minimum hit target (padded, not a 44px visible
/// icon), matching `app.rs`'s theme-toggle button convention.
#[component]
#[allow(non_snake_case, dead_code)]
pub fn CopyButton(value: String) -> impl IntoView {
    let just_copied = RwSignal::new(false);

    let on_click = move |_| {
        let value = value.clone();
        let Some(window) = web_sys::window() else {
            return;
        };
        let promise = window.navigator().clipboard().write_text(&value);
        leptos::task::spawn_local(async move {
            // A rejected write_text Promise (e.g. permissions denied) is a browser-level
            // failure with no sensitive data exposed either way (T-43-04-02, accept
            // disposition) — the button's own visible state (label never flips to "Copied")
            // is the only user-facing signal, matching this crate's "no toast, no modal"
            // convention, so a rejection is intentionally swallowed here.
            if JsFuture::from(promise).await.is_ok() {
                just_copied.set(true);
                set_timeout(move || just_copied.set(false), Duration::from_secs(2));
            }
        });
    };

    view! {
        <button
            on:click=on_click
            aria-label="Copy id"
            style="min-width: 44px; min-height: 44px; display: inline-flex; align-items: center; justify-content: center; background: transparent; border: 1px solid var(--border); border-radius: 6px; color: var(--muted-foreground); cursor: pointer;"
        >
            <span class="text-label">{move || copy_button_label(just_copied.get())}</span>
        </button>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_button_label_reflects_just_copied_state() {
        assert_eq!(copy_button_label(false), "Copy id");
        assert_eq!(copy_button_label(true), "Copied");
    }
}
