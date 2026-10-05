//! The analyst workbench's app shell — header (app mark, the saved-investigations link,
//! `AnalysisPicker`, theme toggle) rendered once above the route table, per the phase 43 UI spec's
//! "App shell layout" section (D-01, D-07, D-09). The predecessor's left-nav `<aside>` (four
//! disabled stub rows, each carrying a build sequencing tag as its trailing text) is gone entirely
//! — the navigation model here is the Home page's card grid plus the `Breadcrumb` each non-Home
//! page renders, not a sidebar (D-01, D-02).
//!
//! # One shell destination, added by plan 45-15
//!
//! PIV-05 asks for the investigation list to be *"reachable from the shell navigation, not only
//! from a page the operator happens to be on"*, and a Home card alone does not satisfy that: Home
//! is a page the operator happens to be on. So the header carries a link to `/investigations`, and
//! the header is rendered outside `<Routes>`, which is what makes it present on every route.
//!
//! That is why `<Router>` now wraps the whole shell rather than only the route table: a header link
//! that was a bare `<a href>` would reload the wasm application on every click, which is a page
//! load dressed as navigation. `<A>` needs the router's context, and the header is inside it while
//! staying outside `<Routes>`.
//!
//! `App` is the top-level component: the header renders unconditionally, then `<Router><Routes>`
//! maps the real pages this milestone ships — `/` (`HomePage`), `/analysis` (`AnalysisPage`,
//! including its own `#<probe-name>` hash handling), `/graph` (`GraphPage`), `/help`
//! (`HelpPage`), `/help/glossary` (`GlossaryPage`). The predecessor Cosmograph spike route is
//! deliberately NOT mounted here any more (D-07) — `cosmograph_spike`'s module and its
//! `CosmographSpikePage`/shim-binding code stay declared in `main.rs` for a future milestone to
//! remount, they are simply unreachable from this route table today.

use crate::alert_detail_page::AlertDetailPage;
use crate::alerts::{AlertsPage, ALERTS_NAV_LABEL, ALERTS_PATH};
use crate::analysis_page::AnalysisPage;
use crate::analysis_picker::AnalysisPicker;
use crate::finding_detail_page::FindingDetailPage;
use crate::graph_page::GraphPage;
use crate::help::algorithms::{AlgorithmHelpPage, AlgorithmsIndexPage};
use crate::help::glossary_page::GlossaryPage;
use crate::help::HelpPage;
use crate::home_page::HomePage;
use crate::investigation::page::{InvestigationPage, InvestigationsPage};
use crate::investigation::{INVESTIGATIONS_PATH, LIST_HEADING};
use crate::profile::{EntityProfilePage, EntitySearchPage};
use crate::theme::Theme;
use leptos::prelude::*;
use leptos_router::components::{Route, Router, Routes, A};
use leptos_router::path;

#[component]
pub fn App() -> impl IntoView {
    let theme = RwSignal::new(Theme::load());

    // Applies the resolved CSS class (`""` for dark-default, `"light"` for the override) to
    // `document.documentElement` reactively whenever `theme` changes, including on first run —
    // this is what actually switches the token set defined in style/tokens.css, and it re-runs
    // for `Theme::System` too since `css_class()` re-resolves `matchMedia` each call.
    Effect::new(move |_| {
        let class = theme.get().css_class();
        if let Some(document) = web_sys::window().and_then(|w| w.document()) {
            if let Some(el) = document.document_element() {
                el.set_class_name(class);
            }
        }
    });

    let cycle_theme = move |_| {
        let next = theme.get().next();
        next.persist();
        theme.set(next);
    };

    // Which investigation the pin control writes into (PIV-05). Provided once, here, so the
    // profile header and the canvas inspection panel share one value rather than two copies that
    // can disagree about whether the entity on screen is already a member. The investigation
    // ITSELF lives in the analysis database — this is the session's choice of which one is open,
    // and nothing about it is persisted anywhere.
    crate::investigation::provide_active_investigation();

    view! {
        <Router>
        <div style="display: flex; flex-direction: column; min-height: 100vh; background: var(--background); color: var(--foreground);">
            <header style="display: flex; align-items: center; justify-content: space-between; height: 56px; padding: 0 var(--space-lg); background: var(--card); border-bottom: 1px solid var(--border); flex-shrink: 0; position: sticky; top: 0; z-index: 20;">
                <div style="display: flex; align-items: baseline; gap: var(--space-sm);">
                    <span aria-hidden="true" style="font-weight: 700; color: var(--primary);">"UI"</span>
                    <span class="text-heading">"UrbanIntegrity Engine"</span>
                    <span class="text-label" style="color: var(--muted-foreground);">"Workbench"</span>
                </div>
                <div style="display: flex; align-items: center; gap: var(--space-sm);">
                    // PIV-05: the saved-investigation list, reachable from the shell on every
                    // route rather than only from Home. An `<A>` rather than an `<a href>` so a
                    // click is a client-side navigation and not a wasm reload.
                    <A
                        href=INVESTIGATIONS_PATH
                        attr:class="text-body"
                        attr:style="min-height: 44px; display: inline-flex; align-items: center; padding: 0 var(--space-sm); color: var(--foreground); text-decoration: none;"
                    >
                        {LIST_HEADING}
                    </A>
                    // TRI-01/TRI-02, 46-09: the alert / motif triage inbox, reachable from every
                    // route the same way the investigation list is.
                    <A
                        href=ALERTS_PATH
                        attr:class="text-body"
                        attr:style="min-height: 44px; display: inline-flex; align-items: center; padding: 0 var(--space-sm); color: var(--foreground); text-decoration: none;"
                    >
                        {ALERTS_NAV_LABEL}
                    </A>
                    // Analysis picker (D-10, amended): fetches the real read-only `_catalog`
                    // list from /api/analyses; no analysis-specific example string appears
                    // anywhere here (enforced by scripts/check-no-analysis-literals.sh).
                    <AnalysisPicker />
                    // aria-label="Toggle theme (currently {mode})" (the phase 42 UI spec "Theme toggle")
                    // — {mode} is theme.get().label(), the CURRENT effective theme, never the
                    // next one the click cycles to. 44x44px minimum hit target (WCAG 2.5.5).
                    <button
                        on:click=cycle_theme
                        aria-label=move || format!("Toggle theme (currently {})", theme.get().label())
                        style="min-width: 44px; min-height: 44px; display: flex; align-items: center; justify-content: center; background: transparent; border: 1px solid var(--border); border-radius: 6px; color: var(--foreground); cursor: pointer;"
                    >
                        {move || theme_icon(theme.get())}
                    </button>
                </div>
            </header>
            <main style="flex: 1 1 auto; padding: var(--space-lg); background: var(--background); min-height: 0;">
                    <Routes fallback=|| "Not found.">
                        <Route path=path!("") view=HomePage />
                        <Route path=path!("analysis") view=AnalysisPage />
                        <Route path=path!("alerts") view=AlertsPage />
                        <Route path=path!("alert/:id") view=AlertDetailPage />
                        <Route path=path!("graph") view=GraphPage />
                        <Route path=path!("entity") view=EntitySearchPage />
                        <Route path=path!("entity/:id") view=EntityProfilePage />
                        <Route path=path!("finding/:probe") view=FindingDetailPage />
                        <Route path=path!("help") view=HelpPage />
                        <Route path=path!("help/glossary") view=GlossaryPage />
                        <Route path=path!("help/algorithms") view=AlgorithmsIndexPage />
                        <Route path=path!("help/algorithms/:slug") view=AlgorithmHelpPage />
                        <Route path=path!("investigations") view=InvestigationsPage />
                        <Route path=path!("investigations/:id") view=InvestigationPage />
                    </Routes>
            </main>
        </div>
        </Router>
    }
}

/// Sun / moon / system-monitor glyph reflecting the CURRENT effective theme (the phase 42 UI spec
/// "Theme toggle" — "Icon swaps ... to reflect the current effective theme, not the next one").
/// Hand-authored inline SVG (outline style, `currentColor` stroke), matching this phase's
/// icon-library convention (the phase 42 UI spec "Design System" Icon library row) — no icon crate
/// added for a 3-glyph set.
fn theme_icon(theme: Theme) -> AnyView {
    match theme {
        Theme::Light => view! {
            <svg width="18" height="18" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true">
                <circle cx="10" cy="10" r="4"></circle>
                <path d="M10 1v2M10 17v2M3.5 3.5l1.4 1.4M15.1 15.1l1.4 1.4M1 10h2M17 10h2M3.5 16.5l1.4-1.4M15.1 4.9l1.4-1.4"></path>
            </svg>
        }
        .into_any(),
        Theme::Dark => view! {
            <svg width="18" height="18" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true">
                <path d="M17 11.5A7 7 0 0 1 8.5 3a7 7 0 1 0 8.5 8.5Z"></path>
            </svg>
        }
        .into_any(),
        Theme::System => view! {
            <svg width="18" height="18" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true">
                <rect x="2" y="4" width="16" height="10" rx="1"></rect>
                <path d="M7 17h6M10 14v3"></path>
            </svg>
        }
        .into_any(),
    }
}
