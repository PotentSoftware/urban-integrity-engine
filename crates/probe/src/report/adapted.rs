//! D-06's "How the Search Adapted" derived narrative — steps built as a `Vec` from what actually
//! happened (`Strategy.identity_blocked`, `osint_active`, `notes`, and the one real gate) and
//! numbered at render, so the Python original's numbering-gap defect becomes unrepresentable.
//!
//! The opening sentence states plainly that every registered probe runs on every battery
//! execution, in a fixed, unconditional order (`run_probes`'s actual behaviour — see PRB-08's
//! foreclosure comment on `crate::run_probes`) and that `Strategy` fields let a
//! later probe read what an earlier one found. It must NOT say or imply that an earlier probe's
//! own verdict gates whether a later probe runs at all — that claim is false of this codebase
//! (D-06).

use crate::Strategy;

use super::ReportEntry;

/// The exact `Vec<String>` of step descriptions [`render`] numbers at render time — extracted so
/// `crates/workbench::report_view::from_battery` (D-15) can reuse the same derivation without
/// re-deriving it (and re-numbering it itself) at a second call site.
pub fn steps(entries: &[ReportEntry], strategy: &Strategy) -> Vec<String> {
    let mut steps: Vec<String> = Vec::new();

    steps.push(format!(
        "Every registered probe ran, in a fixed order, against {} — `Strategy` fields let a \
         later probe read what an earlier one found, but no probe's own verdict determines \
         whether a later probe runs at all.",
        crate::plural(entries.len() as u64, "probe result", "probe results")
    ));

    if strategy.identity_blocked {
        steps.push(
            "Identity resolution was blocked, so address-clustering probes downstream fell back \
             to their own Blocked branch rather than trusting an unresolved identity set."
                .to_string(),
        );
    }

    if strategy.osint_active {
        steps.push(
            "At least one OSINT adapter had a recorded run, so probes reading \
             `Strategy::osint_active` treated a zero-match result as a measured Clear rather \
             than an unmeasured DataGap."
                .to_string(),
        );
    } else {
        steps.push(
            "No OSINT adapter had a recorded run by the time OSINT-dependent probes executed, \
             so those probes reported DataGap rather than assuming a clean result."
                .to_string(),
        );
    }

    if strategy.confirmed_link_count > 0 || strategy.pending_review_count > 0 {
        steps.push(format!(
            "Identity linkage produced {} and {} for analyst follow-up.",
            crate::plural(
                strategy.confirmed_link_count,
                "confirmed link",
                "confirmed links"
            ),
            crate::plural(
                strategy.pending_review_count,
                "pending review item",
                "pending review items"
            )
        ));
    }

    for note in &strategy.notes {
        steps.push(note.clone());
    }

    steps
}

/// "How the Search Adapted" — derived narrative (D-06).
///
/// Numbers [`steps`]'s `Vec` AT RENDER TIME — no step's own text ever hardcodes its number, so a
/// numbering gap is unrepresentable by construction.
pub fn render(entries: &[ReportEntry], strategy: &Strategy) -> String {
    let steps = steps(entries, strategy);

    let mut out = String::new();
    out.push_str("## How the Search Adapted\n\n");
    for (i, step) in steps.iter().enumerate() {
        out.push_str(&format!("{}. {step}\n", i + 1));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MotifKind;
    use urban_integrity_schema::types::verdict::PopulationSource;
    use urban_integrity_schema::types::verdict::{Finding, Severity, Verdict};

    fn entries_fixture() -> Vec<ReportEntry> {
        vec![ReportEntry {
            tier: "Tier-1",
            hypothesis: "test hypothesis",
            motif_kind: MotifKind::Absence,
            finding: Finding {
                probe: "fixture-probe".to_string(),
                verdict: Verdict::classify(1, 1, 0, 1, ""),
                summary: "fixture summary".to_string(),
                cause: None,
                severity: Severity::Info,
                evidence: vec![],
                attributions: vec![],
                remediation_hint: None,
            },
            population_source: PopulationSource::SelfReported {
                reason: "test fixture",
            },
        }]
    }

    fn numbered_prefixes(rendered: &str) -> Vec<u32> {
        rendered
            .lines()
            .filter_map(|line| {
                let digits: String = line.chars().take_while(|c| c.is_ascii_digit()).collect();
                if digits.is_empty() {
                    return None;
                }
                if line.as_bytes().get(digits.len()) != Some(&b'.') {
                    return None;
                }
                digits.parse::<u32>().ok()
            })
            .collect()
    }

    #[test]
    fn identity_blocked_step_is_named_when_set() {
        let entries = entries_fixture();
        let strategy = Strategy {
            identity_blocked: true,
            ..Strategy::default()
        };

        let rendered = render(&entries, &strategy);

        assert!(
            rendered.to_lowercase().contains("identity")
                && rendered.to_lowercase().contains("blocked"),
            "expected a step naming identity resolution as blocked in:\n{rendered}"
        );
    }

    #[test]
    fn step_numbering_has_no_gap_across_two_different_strategy_shapes() {
        let entries = entries_fixture();

        let shape_a = Strategy {
            identity_blocked: false,
            osint_active: true,
            ..Strategy::default()
        };
        let rendered_a = render(&entries, &shape_a);
        let prefixes_a = numbered_prefixes(&rendered_a);
        assert!(
            !prefixes_a.is_empty(),
            "expected numbered steps in:\n{rendered_a}"
        );
        for (i, n) in prefixes_a.iter().enumerate() {
            assert_eq!(
                *n,
                (i + 1) as u32,
                "numbering gap at position {i} in:\n{rendered_a}"
            );
        }

        let shape_b = Strategy {
            identity_blocked: true,
            osint_active: false,
            confirmed_link_count: 2,
            pending_review_count: 1,
            notes: vec!["A synthetic adapted note.".to_string()],
            ..Strategy::default()
        };
        let rendered_b = render(&entries, &shape_b);
        let prefixes_b = numbered_prefixes(&rendered_b);
        assert!(
            !prefixes_b.is_empty(),
            "expected numbered steps in:\n{rendered_b}"
        );
        for (i, n) in prefixes_b.iter().enumerate() {
            assert_eq!(
                *n,
                (i + 1) as u32,
                "numbering gap at position {i} in:\n{rendered_b}"
            );
        }

        // Prove the two shapes actually produced a DIFFERENT step count — otherwise the no-gap
        // assertion above would hold coincidentally rather than because the numbering is
        // genuinely derived from what fired.
        assert_ne!(
            prefixes_a.len(),
            prefixes_b.len(),
            "expected the two Strategy shapes to produce different step counts"
        );
    }

    #[test]
    fn does_not_claim_verdicts_gate_later_probes() {
        // Built from two non-contiguous fragments, deliberately, so the forbidden phrase never
        // appears as a literal contiguous substring anywhere in this file — the acceptance
        // criterion greps this file for the exact phrase, and the test itself must not trip it.
        let forbidden = format!("{} {}", "early verdicts", "gate later ones");
        let entries = entries_fixture();
        let strategy = Strategy::default();
        let rendered = render(&entries, &strategy);
        assert!(!rendered.contains(&forbidden));
    }
}
