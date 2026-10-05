//! D-11's verdict legend — one const table, `{emoji, label, one-sentence definition}` per
//! `Verdict` variant, rendered near the top of the report (after BLUF, before findings).
//!
//! Generalises the existing terminal `render_verdict` mapping
//! (`crates/cli/src/commands/probe/run.rs`) into a single documented vocabulary rather than
//! inventing a second one — every verdict label elsewhere in the document should trace back to
//! one of these 7 rows.

/// `(emoji, label, one-sentence definition)` for all 7 `Verdict` variants, in `Verdict`'s own
/// declaration order (`crates/schema/src/types/verdict.rs`). Definitions are hand-authored,
/// consistent with `Verdict`'s doc comment — there is no per-variant doc comment on the enum to
/// mechanically extract from, so this table is not generated.
const LEGEND: &[(&str, &str, &str)] = &[
    (
        "🔴",
        "Hit",
        "A capable population was fully evaluated and matches met the probe's actionable threshold.",
    ),
    (
        "🟡",
        "Weak",
        "A capable population was fully evaluated and some matches were found, but below the probe's actionable threshold.",
    ),
    (
        "🟢",
        "Clear",
        "A capable population was fully evaluated and no matches were found.",
    ),
    (
        "⚪",
        "NotApplicable",
        "Only part of the population was structurally eligible to be evaluated; the excluded portion is reported alongside the reason.",
    ),
    (
        "❓",
        "DataGap",
        "The probe could not run at all because a required input was missing — nothing was measured.",
    ),
    (
        "🚫",
        "Blocked",
        "The probe could not run because an unmet prerequisite (typically another probe's own DataGap or a Strategy flag) prevented it.",
    ),
    (
        "⏭️",
        "Skipped",
        "The probe's gate predicate found no qualifying upstream signal (a declared dependency did not return Hit/Weak, or — for a Tier-2 probe — no Tier-1 probe did), so it was never run at all; see the Skipped Probes section for its specific cause.",
    ),
];

/// The verdict legend: `{emoji, label, one-sentence definition}` for all 7 `Verdict` variants
/// (D-11). No arguments — the table is fixed, independent of any specific run's findings.
pub fn render() -> String {
    let mut out = String::new();
    out.push_str("## Verdict Legend\n\n");
    out.push_str("| Emoji | Verdict | Meaning |\n");
    out.push_str("|---|---|---|\n");
    for (emoji, label, definition) in LEGEND {
        // Defensive: a literal `|` in a definition would break a Markdown table cell. None of
        // the hand-authored definitions above contain one, but guard against a future edit that
        // introduces one silently.
        let escaped = definition.replace('|', "\\|");
        out.push_str(&format!("| {emoji} | {label} | {escaped} |\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legend_contains_hit_and_its_emoji() {
        let rendered = render();
        assert!(rendered.contains("Hit"));
        assert!(rendered.contains("🔴"));
    }

    #[test]
    fn legend_contains_all_7_variants_with_distinct_emoji() {
        let rendered = render();
        for label in [
            "Hit",
            "Weak",
            "Clear",
            "NotApplicable",
            "DataGap",
            "Blocked",
            "Skipped",
        ] {
            assert!(
                rendered.contains(label),
                "legend is missing variant label {label}"
            );
        }
        for emoji in ["🔴", "🟡", "🟢", "⚪", "❓", "🚫", "⏭️"] {
            assert!(rendered.contains(emoji), "legend is missing emoji {emoji}");
        }
    }

    #[test]
    fn legend_emoji_are_never_reused_across_two_variants() {
        let emoji: Vec<&str> = LEGEND.iter().map(|(e, _, _)| *e).collect();
        let mut unique = emoji.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            emoji.len(),
            unique.len(),
            "legend has a duplicate/reused emoji across two variants: {emoji:?}"
        );
    }
}
