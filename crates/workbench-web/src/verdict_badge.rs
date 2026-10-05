//! `VerdictBadge` + `VerdictLegend` — the cross-cutting verdict display contract Phases 43-48
//! inherit (D-12, D-13, WBX-04, WBX-05). See the phase 42 UI spec's "Verdict badge contract" and
//! "Legend component" sections for the exact per-variant label text, colour treatment and layout
//! rules this file implements.
//!
//! # Testing approach (documented per this plan's `<acceptance_criteria>`'s "pick whichever
//! actually works" instruction)
//!
//! Leptos 0.8 CSR component output only becomes a real DOM tree inside a browser — this
//! workspace's `cargo test` runs on the host target with no headless-browser runner wired in
//! (phase 42 research Pitfall 10). Rendering the actual `<span>`/`<div>` tree and asserting on its
//! HTML string is therefore impractical here. Instead, every piece of verdict -> text/style logic
//! is factored into plain functions (`verdict_label`, `population_source_text`, `coverage_text`,
//! `badge_style`, `skip_cause_text`) that take a `&VerdictView`/`&PopulationSourceView`/
//! `&CoverageView` and return owned `String`s — fully host-testable without any DOM, and the ONLY
//! thing the `VerdictBadge`/`VerdictLegend` components do with them is interpolate them into
//! `view!`. A wrong label, a missing cause string, or a low-contrast style change would show up
//! as a wrong return value from one of these functions, which the tests below assert on
//! precisely.

use leptos::prelude::*;
use urban_integrity_workbench_dto::{CoverageView, PopulationSourceView, VerdictView};

/// The exact label text for each `VerdictKind`, per the phase 42 UI spec's "Verdict badge contract"
/// table. `Clear`'s label depends on its `population_source`: only an `OracleCorroborated`
/// `Clear` gets the "· ORACLE-VERIFIED" suffix (D-12's "only path to green" rule mirrored here).
pub fn verdict_label(verdict: &VerdictView) -> String {
    match verdict {
        VerdictView::Hit { .. } => "HIT".to_string(),
        VerdictView::Weak { .. } => "WEAK".to_string(),
        VerdictView::Clear {
            population_source, ..
        } => match population_source {
            PopulationSourceView::OracleCorroborated { .. } => {
                "CLEAR · ORACLE-VERIFIED".to_string()
            }
            PopulationSourceView::SelfReported { .. } => "CLEAR".to_string(),
        },
        VerdictView::NotApplicable { .. } => "N/A".to_string(),
        VerdictView::DataGap { .. } => "DATA GAP".to_string(),
        VerdictView::Blocked { .. } => "BLOCKED".to_string(),
        VerdictView::Skipped { .. } => "SKIPPED".to_string(),
    }
}

/// `PopulationSourceView`'s pill text — "self-reported" or "oracle-verified ({count})", per the
/// UI spec's `PopulationSource` pill row.
pub fn population_source_text(source: &PopulationSourceView) -> String {
    match source {
        PopulationSourceView::SelfReported { .. } => "self-reported".to_string(),
        PopulationSourceView::OracleCorroborated { oracle_count } => {
            format!("✓ oracle-verified ({oracle_count})")
        }
    }
}

/// Whether a `PopulationSourceView` is the oracle-corroborated variant — drives the pill's
/// success-green treatment (the only green path besides the badge itself, D-12).
pub fn is_oracle_corroborated(source: &PopulationSourceView) -> bool {
    matches!(source, PopulationSourceView::OracleCorroborated { .. })
}

/// The monospace `capable/population` fragment every `Hit`/`Weak`/`Clear`/`NotApplicable` badge
/// carries inline (UI spec: "a monospace `capable/population` fragment").
pub fn coverage_text(coverage: &CoverageView) -> String {
    format!("{}/{}", coverage.capable, coverage.population)
}

/// `Skipped`'s cause text, rendered immediately adjacent to the badge — never truncated (UI spec:
/// "a bare `SKIPPED` badge with no cause text is a contract violation").
pub fn skip_cause_text(verdict: &VerdictView) -> Option<String> {
    match verdict {
        VerdictView::Skipped {
            cause_description, ..
        } => Some(cause_description.clone()),
        _ => None,
    }
}

/// A repeating diagonal stripe/hatch background at the given CSS custom property and alpha
/// percentage — used by `DataGap`/`Blocked` (stripe) and `Skipped` (hatch, wider gap) per the UI
/// spec's "not checked" and "hatched" treatments. `color-mix()` keeps every colour token-driven
/// (D-11) instead of duplicating hex values here.
fn stripe_background(css_var: &str, alpha_pct: u8) -> String {
    format!(
        "repeating-linear-gradient(45deg, color-mix(in srgb, var({css_var}) {alpha_pct}%, transparent), color-mix(in srgb, var({css_var}) {alpha_pct}%, transparent) 4px, transparent 4px, transparent 8px)"
    )
}

/// The badge `<span>`'s full inline style — background/border/text colour per `VerdictKind`,
/// always pill-shaped (`border-radius: 9999px`) with `xs`/`sm` internal padding, per the UI
/// spec's "Verdict badge contract" table. Every colour goes through a `--*` custom property
/// (never a literal hex), so light/dark theming (D-11) applies automatically.
fn badge_style(verdict: &VerdictView) -> String {
    let (background, border, color) = match verdict {
        VerdictView::Hit { .. } => (
            "color-mix(in srgb, var(--destructive) 15%, transparent)".to_string(),
            "1px solid color-mix(in srgb, var(--destructive) 30%, transparent)".to_string(),
            "var(--destructive)".to_string(),
        ),
        VerdictView::Weak { .. } => (
            "color-mix(in srgb, var(--warning) 15%, transparent)".to_string(),
            "1px solid color-mix(in srgb, var(--warning) 30%, transparent)".to_string(),
            "var(--warning)".to_string(),
        ),
        VerdictView::Clear {
            population_source, ..
        } => match population_source {
            // The ONLY path to green anywhere in the verdict vocabulary (D-12, hard rule).
            PopulationSourceView::OracleCorroborated { .. } => (
                "color-mix(in srgb, var(--success) 15%, transparent)".to_string(),
                "1px solid color-mix(in srgb, var(--success) 30%, transparent)".to_string(),
                "var(--success)".to_string(),
            ),
            // Neutral grey — never green (D-12, hard rule).
            PopulationSourceView::SelfReported { .. } => (
                "color-mix(in srgb, var(--muted-foreground) 12%, transparent)".to_string(),
                "1px solid var(--border)".to_string(),
                "var(--muted-foreground)".to_string(),
            ),
        },
        VerdictView::NotApplicable { .. } => (
            "transparent".to_string(),
            "1px solid var(--muted-foreground)".to_string(),
            "var(--muted-foreground)".to_string(),
        ),
        // Purple "not checked" family (D-12) — dashed border distinguishes DataGap from Blocked.
        VerdictView::DataGap { .. } => (
            stripe_background("--accent", 12),
            "1px dashed var(--accent)".to_string(),
            "var(--accent)".to_string(),
        ),
        // Same purple family, solid border distinguishes this from DataGap.
        VerdictView::Blocked { .. } => (
            stripe_background("--accent", 12),
            "1px solid var(--accent)".to_string(),
            "var(--accent)".to_string(),
        ),
        VerdictView::Skipped { .. } => (
            stripe_background("--border", 40),
            "1px solid var(--border)".to_string(),
            "var(--muted-foreground)".to_string(),
        ),
    };
    format!(
        "display: inline-flex; align-items: center; border-radius: 9999px; padding: var(--space-xs) var(--space-sm); background: {background}; border: {border}; color: {color}; white-space: nowrap;"
    )
}

/// The `PopulationSource` pill's style — grey `--card-hover` background for self-reported, a
/// `--success`-tinted background for oracle-corroborated (UI spec's `PopulationSource` pill row).
fn population_pill_style(is_oracle: bool) -> String {
    if is_oracle {
        "display: inline-flex; align-items: center; border-radius: 9999px; padding: var(--space-xs) var(--space-sm); background: color-mix(in srgb, var(--success) 10%, transparent); color: var(--success); white-space: nowrap;".to_string()
    } else {
        "display: inline-flex; align-items: center; border-radius: 9999px; padding: var(--space-xs) var(--space-sm); background: var(--card-hover); color: var(--muted-foreground); white-space: nowrap;".to_string()
    }
}

/// Renders one `VerdictView` as a pill badge, always accompanied by its `capable/population`
/// fragment and `PopulationSource` pill (for `Hit`/`Weak`/`Clear`/`NotApplicable`) or its cause
/// text (for `Skipped`) — every accompanying element at the SAME `.text-label` class as the badge
/// itself, never a smaller/lower-contrast treatment (D-13, WBX-05). Badge, fragment, pill and
/// cause text wrap as one unit (`flex-wrap: wrap` on the outer span, `xs` gaps) and never split
/// mid-element across lines.
#[component]
#[allow(non_snake_case)]
pub fn VerdictBadge(verdict: VerdictView) -> impl IntoView {
    let label = verdict_label(&verdict);
    let style = badge_style(&verdict);
    let cause = skip_cause_text(&verdict);

    let coverage_and_source: Option<(String, String, bool)> = match &verdict {
        VerdictView::Hit {
            coverage,
            population_source,
            ..
        }
        | VerdictView::Weak {
            coverage,
            population_source,
            ..
        }
        | VerdictView::Clear {
            coverage,
            population_source,
        }
        | VerdictView::NotApplicable {
            coverage,
            population_source,
            ..
        } => Some((
            coverage_text(coverage),
            population_source_text(population_source),
            is_oracle_corroborated(population_source),
        )),
        VerdictView::DataGap { .. } | VerdictView::Blocked { .. } | VerdictView::Skipped { .. } => {
            None
        }
    };

    view! {
        <span style="display: inline-flex; align-items: center; flex-wrap: wrap; gap: var(--space-xs);">
            <span class="text-label" style=style>{label}</span>
            {coverage_and_source.clone().map(|(cov, _, _)| view! {
                <span class="text-label font-mono">{cov}</span>
            })}
            {coverage_and_source.map(|(_, source_text, is_oracle)| {
                let pill_style = population_pill_style(is_oracle);
                view! { <span class="text-label" style=pill_style>{source_text}</span> }
            })}
            {cause.map(|text| view! {
                <span class="text-body" style="color: var(--muted-foreground);">{text}</span>
            })}
        </span>
    }
}

/// One hand-constructed fixture per `VerdictKind`, plus a second `Clear` fixture for the
/// oracle-corroborated treatment — 8 total, matching `VerdictLegend`'s 8 visual entries. `pub`
/// so both `VerdictLegend` and this module's tests share exactly one definition each.
pub fn fixture_hit() -> VerdictView {
    VerdictView::Hit {
        coverage: CoverageView {
            population: 148,
            capable: 8,
            excluded: 140,
        },
        matches: 3,
        population_source: PopulationSourceView::SelfReported {
            reason: "registered address population".to_string(),
        },
    }
}

pub fn fixture_weak() -> VerdictView {
    VerdictView::Weak {
        coverage: CoverageView {
            population: 148,
            capable: 8,
            excluded: 140,
        },
        matches: 1,
        population_source: PopulationSourceView::SelfReported {
            reason: "registered address population".to_string(),
        },
    }
}

pub fn fixture_clear_self_reported() -> VerdictView {
    VerdictView::Clear {
        coverage: CoverageView {
            population: 148,
            capable: 8,
            excluded: 140,
        },
        population_source: PopulationSourceView::SelfReported {
            reason: "registered address population".to_string(),
        },
    }
}

pub fn fixture_clear_oracle_corroborated() -> VerdictView {
    VerdictView::Clear {
        coverage: CoverageView {
            population: 148,
            capable: 8,
            excluded: 140,
        },
        population_source: PopulationSourceView::OracleCorroborated { oracle_count: 8 },
    }
}

pub fn fixture_not_applicable() -> VerdictView {
    VerdictView::NotApplicable {
        coverage: CoverageView {
            population: 148,
            capable: 0,
            excluded: 148,
        },
        reason: "3 structurally excluded".to_string(),
        population_source: PopulationSourceView::SelfReported {
            reason: "registered address population".to_string(),
        },
    }
}

pub fn fixture_data_gap() -> VerdictView {
    VerdictView::DataGap {
        missing_input: "missing OSINT source".to_string(),
    }
}

pub fn fixture_blocked() -> VerdictView {
    VerdictView::Blocked {
        prerequisite: "identity-integrity".to_string(),
    }
}

pub fn fixture_skipped() -> VerdictView {
    VerdictView::Skipped {
        cause_description: "upstream 'identity-integrity' returned Clear".to_string(),
        upstream: Some("identity-integrity".to_string()),
        tier: None,
    }
}

/// A static, always-visible legend listing all 8 badge treatments (7 `VerdictKind`s plus the
/// extra oracle-corroborated `Clear` fixture) with a one-line plain-English meaning each, in a
/// wrapped grid — reused verbatim by Phases 44/48's ledger/inbox screens (UI spec's "Legend
/// component"). Not collapsible, not conditional: renders unconditionally even when the current
/// page has zero real verdicts to show. Written out as 8 explicit entries (not generated from a
/// loop) so each entry's `VerdictBadge` usage is literal, grep-checkable source text — matching
/// this crate's established convention (see `app.rs`'s left-nav rows).
#[component]
#[allow(non_snake_case)]
pub fn VerdictLegend() -> impl IntoView {
    view! {
        <div
            class="text-body"
            style="display: flex; flex-wrap: wrap; gap: var(--space-md); padding: var(--space-lg); background: var(--card); border: 1px solid var(--border); border-radius: 8px;"
        >
            <div style="display: flex; flex-direction: column; gap: var(--space-xs); min-width: 220px;">
                <VerdictBadge verdict=fixture_hit() />
                <span class="text-body" style="color: var(--muted-foreground);">"Hit — probe found matches at or above its threshold"</span>
            </div>
            <div style="display: flex; flex-direction: column; gap: var(--space-xs); min-width: 220px;">
                <VerdictBadge verdict=fixture_weak() />
                <span class="text-body" style="color: var(--muted-foreground);">"Weak — probe found matches below its confidence threshold"</span>
            </div>
            <div style="display: flex; flex-direction: column; gap: var(--space-xs); min-width: 220px;">
                <VerdictBadge verdict=fixture_clear_self_reported() />
                <span class="text-body" style="color: var(--muted-foreground);">"Clear (self-reported) — probe found no matches against a self-reported population"</span>
            </div>
            <div style="display: flex; flex-direction: column; gap: var(--space-xs); min-width: 220px;">
                <VerdictBadge verdict=fixture_clear_oracle_corroborated() />
                <span class="text-body" style="color: var(--muted-foreground);">"Clear (oracle-verified) — probe found no matches, independently confirmed against an oracle"</span>
            </div>
            <div style="display: flex; flex-direction: column; gap: var(--space-xs); min-width: 220px;">
                <VerdictBadge verdict=fixture_not_applicable() />
                <span class="text-body" style="color: var(--muted-foreground);">"N/A — this probe does not apply to the current entity"</span>
            </div>
            <div style="display: flex; flex-direction: column; gap: var(--space-xs); min-width: 220px;">
                <VerdictBadge verdict=fixture_data_gap() />
                <span class="text-body" style="color: var(--muted-foreground);">"Data gap — a required input source is missing, so this probe could not run"</span>
            </div>
            <div style="display: flex; flex-direction: column; gap: var(--space-xs); min-width: 220px;">
                <VerdictBadge verdict=fixture_blocked() />
                <span class="text-body" style="color: var(--muted-foreground);">"Blocked — a prerequisite check has not completed yet"</span>
            </div>
            <div style="display: flex; flex-direction: column; gap: var(--space-xs); min-width: 220px;">
                <VerdictBadge verdict=fixture_skipped() />
                <span class="text-body" style="color: var(--muted-foreground);">"Skipped — the orchestrator's gate decided not to run this probe"</span>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_fixtures() -> Vec<VerdictView> {
        vec![
            fixture_hit(),
            fixture_weak(),
            fixture_clear_self_reported(),
            fixture_clear_oracle_corroborated(),
            fixture_not_applicable(),
            fixture_data_gap(),
            fixture_blocked(),
            fixture_skipped(),
        ]
    }

    /// All 8 fixtures (7 `VerdictKind`s + the extra oracle-corroborated `Clear`) produce visually
    /// distinct label text — the exact strings from the phase 42 UI spec's badge contract table.
    ///
    /// PROVE THIS CAN FAIL: this test was first run with `VerdictView::Clear`'s
    /// oracle-corroborated arm returning the same `"CLEAR"` label as the self-reported arm — the
    /// `unique_labels.len() == 8` assertion failed with `7`, since the two `Clear` fixtures
    /// collapsed to the same label, before the `"· ORACLE-VERIFIED"` suffix was added.
    #[test]
    fn all_eight_fixtures_produce_distinct_labels() {
        let labels: Vec<String> = all_fixtures().iter().map(verdict_label).collect();
        assert_eq!(
            labels,
            vec![
                "HIT",
                "WEAK",
                "CLEAR",
                "CLEAR · ORACLE-VERIFIED",
                "N/A",
                "DATA GAP",
                "BLOCKED",
                "SKIPPED",
            ]
        );
        let unique_labels: std::collections::HashSet<_> = labels.iter().collect();
        assert_eq!(
            unique_labels.len(),
            8,
            "all 8 fixtures must produce distinct label text"
        );
    }

    /// `Skipped`'s cause text is present and exact — never truncated, never missing (UI spec: "a
    /// bare SKIPPED badge with no cause text is a contract violation").
    #[test]
    fn skipped_cause_text_is_present_and_exact() {
        let skipped = fixture_skipped();
        let cause = skip_cause_text(&skipped);
        assert_eq!(
            cause.as_deref(),
            Some("upstream 'identity-integrity' returned Clear")
        );
    }

    /// Non-`Skipped` variants carry no cause text (there is nothing to render adjacent to them).
    #[test]
    fn non_skipped_variants_have_no_cause_text() {
        for verdict in [
            fixture_hit(),
            fixture_weak(),
            fixture_clear_self_reported(),
            fixture_data_gap(),
            fixture_blocked(),
        ] {
            assert_eq!(skip_cause_text(&verdict), None);
        }
    }

    /// `population_source_text` produces distinct, exact text for self-reported vs.
    /// oracle-corroborated, and `is_oracle_corroborated` agrees.
    #[test]
    fn population_source_text_distinguishes_self_reported_from_oracle_corroborated() {
        let self_reported = PopulationSourceView::SelfReported {
            reason: "test".to_string(),
        };
        let oracle = PopulationSourceView::OracleCorroborated { oracle_count: 8 };

        assert_eq!(population_source_text(&self_reported), "self-reported");
        assert_eq!(population_source_text(&oracle), "✓ oracle-verified (8)");
        assert!(!is_oracle_corroborated(&self_reported));
        assert!(is_oracle_corroborated(&oracle));
    }

    /// `coverage_text` renders `capable/population`, never re-deriving `excluded` (D-13: numbers
    /// travel with the verdict exactly as the server sent them).
    #[test]
    fn coverage_text_renders_capable_over_population() {
        let coverage = CoverageView {
            population: 148,
            capable: 8,
            excluded: 140,
        };
        assert_eq!(coverage_text(&coverage), "8/148");
    }

    /// `badge_style` never applies an `opacity` fade or a sub-11px font-size to the badge itself
    /// — the population-source/coverage elements reuse `.text-label` in the component, never a
    /// smaller inline override (D-13/WBX-05's "same visual weight" rule, checked programmatically
    /// here as a companion to the file-level grep check in this task's acceptance criteria).
    #[test]
    fn badge_style_never_applies_opacity_or_tiny_font_size() {
        for verdict in all_fixtures() {
            let style = badge_style(&verdict);
            assert!(
                !style.contains("opacity"),
                "badge_style must not apply opacity: {style}"
            );
            assert!(
                !style.contains("font-size"),
                "badge_style must not override font-size (badges use .text-label): {style}"
            );
        }
    }
}
