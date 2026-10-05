//! `motif_bars.rs` — the `ward-concentration` motif thumbnail (D-09): horizontal ranked bars, no
//! nodes, no edges, no simulated node-link layout of any kind. This is the phase's proof that the
//! `kind`-selects-layout rule is real (criterion 3): a probe whose finding is a distribution over
//! places is drawn as a distribution, full stop.
//!
//! Data shape this file consumes, established by `crates/probe/src/ward_concentration.rs`:
//! `evidence[0].text` is a single combined row, `"Top wards: A: 12, B: 9, C: 3."`, and
//! `evidence[0].refs` carries the same wards as `ward:{code}` `RecordRef`s. This file parses the
//! text for VALUES and reads `.refs` for display names — it never re-derives a ranking
//! client-side, matching the planning decision already made in the phase 50 pattern notes.
//!
//! The name lookup is keyed on the ward code, never on position (WR-02, phase 50 code review): if the
//! text pairs and the ref list ever drift — a ward dropped from one but not the other, or the two
//! reordered — a positional zip would silently attach one ward's name to another ward's count.
//! [`bar_display_labels`] instead pairs each bar with the ref whose id names that bar's own code,
//! and falls back to the parsed code itself when no ref matches.
//!
//! Follows `verdict_badge.rs`'s pure-fn-then-thin-wrapper split — see that file's own top-of-file
//! doc comment for the rationale (no headless-browser test runner in this workspace).
//!
//! Mounted by `finding_card.rs`'s `RankedBars`-kind thumbnail dispatch (plan 50-06 Task 2), itself
//! reached from `finding_board.rs` -> `analysis_page.rs` -> `app.rs` (plan 50-06 Task 3).

use leptos::prelude::*;
use urban_integrity_workbench_dto::{EntityRef, EvidenceRowView};

/// How many drawn bars the thumbnail budget allows — the phase 50 UI spec's "up to 5 bars".
const BAR_BUDGET: usize = 5;

/// Parses `"Top wards: A: 12, B: 9, C: 3."` into `[("A", 12), ("B", 9), ("C", 3)]`. Tolerates a
/// missing trailing period. Returns an empty vec for text not matching the `"Top wards: "`
/// prefix — never panics on an unexpected probe's text. Splits each pair on its LAST `": "` (not
/// the first) so a ward code that itself happens to contain a colon does not break the parse.
pub fn parse_ranked_pairs(text: &str) -> Vec<(String, u64)> {
    let Some(rest) = text.strip_prefix("Top wards: ") else {
        return Vec::new();
    };
    let rest = rest.strip_suffix('.').unwrap_or(rest);
    rest.split(", ")
        .filter_map(|pair| {
            let (label, value) = pair.rsplit_once(": ")?;
            let value: u64 = value.trim().parse().ok()?;
            Some((label.trim().to_string(), value))
        })
        .collect()
}

/// The display label for each parsed bar, in bar order: the `name` of the ref whose id is that
/// bar's ward code (`ward:{code}`, or the bare code), else the parsed code itself. Matching on the
/// id rather than the index means a text/ref length or order mismatch degrades to showing a ward
/// code, never to showing the wrong ward's name (WR-02). A matched ref with an empty name also
/// falls back to the code, so a bar is never left unlabelled.
pub fn bar_display_labels(pairs: &[(String, u64)], refs: &[EntityRef]) -> Vec<String> {
    pairs
        .iter()
        .map(|(code, _)| {
            let prefixed = format!("ward:{code}");
            refs.iter()
                .find(|r| r.id == prefixed || r.id == *code)
                .map(|r| r.name.trim())
                .filter(|name| !name.is_empty())
                .map_or_else(|| code.clone(), str::to_string)
        })
        .collect()
}

/// Buckets `value` into `1..=5` against the drawn set's own `min`/`max` (per-card bucketing, never
/// a scale shared across cards — Color section). `min == max` (every drawn bar equal) returns `5`
/// directly: the single value is its own max. Monotonic: a higher value never returns a lower
/// bucket than a lower value, for any fixed `min`/`max`.
pub fn bucket_index(value: u64, min: u64, max: u64) -> u8 {
    if min == max {
        return 5;
    }
    let span = (max - min) as f64;
    let normalized = value.saturating_sub(min) as f64 / span;
    let bucket = (normalized * 4.0).round() as i64 + 1;
    bucket.clamp(1, 5) as u8
}

/// The "+{n} more wards" truncation note (Copywriting Contract) — `None` when every ranked ward
/// was drawn.
pub fn bars_truncation_note(real_count: usize, drawn_count: usize) -> Option<String> {
    if real_count <= drawn_count {
        None
    } else {
        Some(format!("+{} more wards", real_count - drawn_count))
    }
}

/// Renders `evidence`'s ranked bars: parses up to `BAR_BUDGET` ranked pairs from `evidence.text`,
/// pairs each with its `evidence.refs` entry by ward code ([`bar_display_labels`]), buckets each bar's
/// magnitude against the drawn set's own min/max, and renders horizontal `<div>` bars (24px tall,
/// `--magnitude-N` fill) plus the truncation note when the probe ranked more than `BAR_BUDGET`
/// wards. No simulated layout, no nodes, no edges — the whole point of this motif kind.
#[component]
#[allow(non_snake_case)]
pub fn MotifBars(evidence: EvidenceRowView) -> impl IntoView {
    let pairs = parse_ranked_pairs(&evidence.text);
    let drawn: Vec<(String, u64)> = pairs.iter().take(BAR_BUDGET).cloned().collect();
    let note = bars_truncation_note(pairs.len(), drawn.len());

    let max_value = drawn.iter().map(|(_, v)| *v).max().unwrap_or(0);
    let min_value = drawn.iter().map(|(_, v)| *v).min().unwrap_or(0);

    let labels = bar_display_labels(&drawn, &evidence.refs);

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-sm); width: 100%;">
            {drawn.iter().zip(labels).map(|((_, value), display_label)| {
                let bucket = bucket_index(*value, min_value, max_value);
                let fill = format!("var(--magnitude-{bucket})");
                let width_pct = if max_value == 0 {
                    0.0
                } else {
                    (*value as f64 / max_value as f64) * 100.0
                };
                let value_text = value.to_string();
                view! {
                    <div style="display: flex; align-items: center; gap: var(--space-sm); height: 24px;">
                        <span
                            class="text-label"
                            title=display_label.clone()
                            style="flex: 0 0 auto; max-width: 40%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--foreground);"
                        >
                            {display_label.clone()}
                        </span>
                        <div style="flex: 1 1 auto; height: 100%; display: flex; align-items: center;">
                            <div style=format!(
                                "height: 100%; width: {width_pct}%; background: {fill}; border-radius: 2px;"
                            )></div>
                        </div>
                        <span class="text-label font-mono" style="flex: 0 0 auto; color: var(--muted-foreground);">
                            {value_text}
                        </span>
                    </div>
                }
            }).collect_view()}
            {note.map(|text| view! {
                <div class="text-label" style="color: var(--muted-foreground);">{text}</div>
            })}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_exact_ui_spec_example_string() {
        let parsed = parse_ranked_pairs("Top wards: A: 12, B: 9, C: 3.");
        assert_eq!(
            parsed,
            vec![
                ("A".to_string(), 12),
                ("B".to_string(), 9),
                ("C".to_string(), 3),
            ]
        );
    }

    #[test]
    fn tolerates_a_missing_trailing_period() {
        let parsed = parse_ranked_pairs("Top wards: A: 12, B: 9");
        assert_eq!(parsed, vec![("A".to_string(), 12), ("B".to_string(), 9)]);
    }

    /// PROVE THIS CAN FAIL: remove the `strip_prefix` guard and this parses garbage text as a
    /// single malformed pair instead of returning empty.
    #[test]
    fn malformed_text_returns_empty_vec_not_a_panic() {
        assert_eq!(parse_ranked_pairs("not a ranked-pairs row at all"), vec![]);
        assert_eq!(parse_ranked_pairs(""), vec![]);
        assert_eq!(parse_ranked_pairs("Top wards: "), vec![]);
    }

    #[test]
    fn bucket_index_min_equals_max_returns_five() {
        assert_eq!(bucket_index(7, 7, 7), 5);
        assert_eq!(bucket_index(0, 0, 0), 5);
    }

    /// PROVE THIS CAN FAIL: use a floor instead of a round and two of these five values collapse
    /// into the same bucket.
    #[test]
    fn bucket_index_is_monotonic_across_five_distinct_values() {
        let buckets: Vec<u8> = [0u64, 5, 10, 15, 20]
            .iter()
            .map(|&v| bucket_index(v, 0, 20))
            .collect();
        assert_eq!(buckets, vec![1, 2, 3, 4, 5]);
        for pair in buckets.windows(2) {
            assert!(
                pair[1] >= pair[0],
                "buckets must never decrease: {buckets:?}"
            );
        }
    }

    fn ward_ref(code: &str, name: &str) -> EntityRef {
        EntityRef {
            id: format!("ward:{code}"),
            name: name.to_string(),
            kind: "ward".to_string(),
        }
    }

    #[test]
    fn display_labels_use_each_wards_own_ref_name_when_text_and_refs_agree() {
        let pairs = parse_ranked_pairs("Top wards: X05000001: 12, X05000002: 9.");
        let refs = vec![
            ward_ref("X05000001", "North Ward"),
            ward_ref("X05000002", "South Ward"),
        ];
        assert_eq!(
            bar_display_labels(&pairs, &refs),
            vec!["North Ward".to_string(), "South Ward".to_string()]
        );
    }

    /// PROVE THIS CAN FAIL: revert to `refs.get(i)` and the first bar (X05000001, 12) is labelled
    /// "South Ward" — the ward the refs list puts first after X05000001 was dropped from it.
    #[test]
    fn mismatched_text_and_refs_never_attach_one_wards_name_to_another_wards_count() {
        let pairs = parse_ranked_pairs("Top wards: X05000001: 12, X05000002: 9, X05000003: 3.");
        // Shorter AND reordered relative to the text: X05000001 missing, the other two swapped.
        let refs = vec![
            ward_ref("X05000003", "East Ward"),
            ward_ref("X05000002", "South Ward"),
        ];
        assert_eq!(
            bar_display_labels(&pairs, &refs),
            vec![
                "X05000001".to_string(),
                "South Ward".to_string(),
                "East Ward".to_string(),
            ]
        );
    }

    #[test]
    fn display_labels_fall_back_to_the_parsed_code_with_no_refs_or_an_empty_name() {
        let pairs = parse_ranked_pairs("Top wards: X05000001: 12, X05000002: 9.");
        assert_eq!(
            bar_display_labels(&pairs, &[]),
            vec!["X05000001".to_string(), "X05000002".to_string()]
        );
        let refs = vec![ward_ref("X05000001", "  ")];
        assert_eq!(
            bar_display_labels(&pairs, &refs),
            vec!["X05000001".to_string(), "X05000002".to_string()]
        );
    }

    #[test]
    fn truncation_note_both_arms() {
        assert_eq!(bars_truncation_note(5, 5), None);
        assert_eq!(bars_truncation_note(3, 5), None);
        assert_eq!(
            bars_truncation_note(8, 5),
            Some("+3 more wards".to_string())
        );
    }
}
