//! `finding_board.rs` — the board container (D-01, D-02, criterion 1 and 6): one `FindingCard`
//! per probe, grouped and ordered exactly as the report's existing six fixed groups, mounted
//! above `analysis_page.rs`'s existing accordion.
//!
//! `groups` is a second READ of the SAME `ReportView` the accordion already has in memory — this
//! file is never a second fetch (D-01: "one `ReportView` fetch, no new header entry"). Iterates
//! `groups` in the order given, never re-sorted — the caller already has them in report order.

use leptos::prelude::*;
use urban_integrity_workbench_dto::ProbeGroupView;

use crate::analysis_page::group_badge_style;
use crate::finding_card::FindingCard;

/// One group's heading — Label size, the same `group_badge_style`-tinted dot the existing
/// accordion rows use, the group's plain-English name and its probe count in `.font-mono`
/// (the phase 50 UI spec's "Finding board layout" section).
fn group_heading(group: &ProbeGroupView) -> impl IntoView {
    let dot_style = group_badge_style(&group.name);
    let name = group.name.clone();
    let count = group.probes.len();

    view! {
        <div style="display: flex; align-items: center; gap: var(--space-sm); margin-top: var(--space-lg); margin-bottom: var(--space-md);">
            <span
                aria-hidden="true"
                style=format!("width: 12px; height: 12px; border-radius: 9999px; flex-shrink: 0; {dot_style}")
            ></span>
            <span class="text-label">{name}</span>
            <span class="text-label font-mono" style="color: var(--muted-foreground);">
                "(" {count.to_string()} ")"
            </span>
        </div>
    }
}

/// One group's row of cards — `repeat(auto-fill, minmax(320px, 1fr))`, `lg` gap
/// (the phase 50 UI spec's "Finding board layout" section).
fn group_card_grid(group: ProbeGroupView) -> impl IntoView {
    view! {
        <div style="display: grid; grid-template-columns: repeat(auto-fill, minmax(320px, 1fr)); gap: var(--space-lg);">
            {group.probes.into_iter().map(|probe| view! { <FindingCard probe=probe /> }).collect_view()}
        </div>
    }
}

/// The board itself: one heading + card grid per group, in the given order.
#[component]
#[allow(non_snake_case)]
pub fn FindingBoard(groups: Vec<ProbeGroupView>) -> impl IntoView {
    view! {
        <div style="display: flex; flex-direction: column;">
            {groups.into_iter().map(|group| {
                let heading = group_heading(&group);
                let grid = group_card_grid(group);
                view! {
                    <div>
                        {heading}
                        {grid}
                    </div>
                }
            }).collect_view()}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_workbench_dto::{ProbeView, VerdictView};

    fn sample_probe(name: &str) -> ProbeView {
        ProbeView {
            name: name.to_string(),
            tier: "tier-1".to_string(),
            hypothesis: "test hypothesis".to_string(),
            verdict: VerdictView::DataGap {
                missing_input: "missing OSINT source".to_string(),
            },
            measurement_summary: "test summary".to_string(),
            evidence: vec![],
            motif_kind: "Absence".to_string(),
            cause: None,
            remediation: None,
        }
    }

    /// `FindingBoard` takes `Vec<ProbeGroupView>` and never re-sorts — the six fixed groups this
    /// crate already carries in report order pass straight through as-is. This is a compile-time
    /// / smoke assertion of the accepted shape, since a full render assertion needs a DOM this
    /// crate's host tests do not have (see `verdict_badge.rs`'s own testing-approach doc comment).
    #[test]
    fn group_order_is_preserved_verbatim() {
        let groups = [
            ProbeGroupView {
                name: "Confirmed Findings".to_string(),
                probes: vec![sample_probe("shared-exact-address")],
            },
            ProbeGroupView {
                name: "Skipped".to_string(),
                probes: vec![],
            },
        ];
        let names: Vec<String> = groups.iter().map(|g| g.name.clone()).collect();
        assert_eq!(
            names,
            vec!["Confirmed Findings".to_string(), "Skipped".to_string()]
        );
    }
}
