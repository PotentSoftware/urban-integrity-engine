//! Theme cycling, persistence and system-preference resolution (D-11, T-42-06).
//!
//! `Theme` is a closed 3-variant enum. `Theme::load()` parses the stored `localStorage` value
//! against this closed set; any value that isn't exactly `"system"`, `"light"` or `"dark"` falls
//! back to `Theme::System` — the value is never `eval`'d or interpolated into HTML/CSS directly
//! (the threat register's T-42-06 mitigation).

const STORAGE_KEY: &str = "workbench-theme";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    System,
    Light,
    Dark,
}

impl Theme {
    /// Cycles System -> Light -> Dark -> System (the phase 42 UI spec "Theme toggle").
    pub fn next(self) -> Theme {
        match self {
            Theme::System => Theme::Light,
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::System,
        }
    }

    /// The CURRENT effective theme (`"system"`, `"light"`, or `"dark"`) — used verbatim in the
    /// theme-toggle button's `aria-label="Toggle theme (currently {mode})"`, never the mode it is
    /// about to switch to.
    pub fn label(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    /// Resolves to the CSS class Leptos applies to `document.documentElement` — `"light"` or
    /// `""` (dark is `:root`'s default, no class needed). `System` resolves via
    /// `window.matchMedia("(prefers-color-scheme: dark)")`; falls back to dark (mockup's
    /// default, per the research notes' `web_sys` fallback discipline) on any `web-sys` error.
    ///
    /// Deliberately NOT `#[cfg(target_arch = "wasm32")]`-gated: `web-sys`/`wasm-bindgen` carry no
    /// crate-level `target_arch` gate (the phase 42 research notes, confirmed against
    /// `github.com/rustwasm/wasm-bindgen`), so this compiles on the host target too — real DOM
    /// calls (`window()`) just resolve to `None` outside a browser, which the `unwrap_or`/`?`
    /// chains below already treat as "no support, use the fallback." Gating this by
    /// `target_arch` would break `cargo build --workspace`'s host-target compile of this crate.
    pub fn css_class(self) -> &'static str {
        match self {
            Theme::Light => "light",
            Theme::Dark => "",
            Theme::System => {
                if system_prefers_dark() {
                    ""
                } else {
                    "light"
                }
            }
        }
    }

    fn stored_value(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    /// Parses a raw `localStorage` value against the closed 3-variant set (T-42-06). Any
    /// unrecognised string — including a tampered or stale value — falls back to `Theme::System`
    /// rather than panicking or being interpolated anywhere.
    fn from_stored(raw: &str) -> Theme {
        match raw {
            "light" => Theme::Light,
            "dark" => Theme::Dark,
            _ => Theme::System,
        }
    }

    /// Persists this theme to `localStorage` under `workbench-theme`.
    pub fn persist(self) {
        if let Some(window) = web_sys::window() {
            if let Ok(Some(storage)) = window.local_storage() {
                let _ = storage.set_item(STORAGE_KEY, self.stored_value());
            }
        }
    }

    /// Loads the persisted theme, defaulting to `Theme::System` if unset, unreadable, or
    /// unrecognised.
    pub fn load() -> Theme {
        web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .and_then(|storage| storage.get_item(STORAGE_KEY).ok().flatten())
            .map(|raw| Theme::from_stored(&raw))
            .unwrap_or(Theme::System)
    }
}

fn system_prefers_dark() -> bool {
    web_sys::window()
        .and_then(|w| w.match_media("(prefers-color-scheme: dark)").ok().flatten())
        .map(|mql| mql.matches())
        // No web-sys / matchMedia support — fall back to dark, the mockup's default.
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Host-target unit test, no `web-sys` DOM calls needed — proves the cycle order.
    #[test]
    fn theme_cycles_system_light_dark_system() {
        assert_eq!(Theme::System.next(), Theme::Light);
        assert_eq!(Theme::Light.next(), Theme::Dark);
        assert_eq!(Theme::Dark.next(), Theme::System);
    }

    /// T-42-06 mitigation: any unrecognised stored string falls back to `Theme::System`, never
    /// interpolated or trusted directly.
    #[test]
    fn from_stored_falls_back_to_system_for_unrecognised_values() {
        assert_eq!(Theme::from_stored("light"), Theme::Light);
        assert_eq!(Theme::from_stored("dark"), Theme::Dark);
        assert_eq!(Theme::from_stored("system"), Theme::System);
        assert_eq!(Theme::from_stored(""), Theme::System);
        assert_eq!(
            Theme::from_stored("<script>alert(1)</script>"),
            Theme::System
        );
        assert_eq!(Theme::from_stored("DARK"), Theme::System);
    }

    #[test]
    fn label_reflects_current_not_next_mode() {
        assert_eq!(Theme::System.label(), "system");
        assert_eq!(Theme::Light.label(), "light");
        assert_eq!(Theme::Dark.label(), "dark");
    }
}
