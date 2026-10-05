//! Where a fact came from, on screen (PIV-03, 45-CONTEXT D-04) — and the one deliberate departure
//! this plan makes from the phase 45 UI spec.
//!
//! # The departure: the section is the unit for *missing* provenance, the row for present
//!
//! The spec puts one chip on every fact row, `Unattributed` included. Plan 45-04 then measured the
//! population that chip would describe, and the measurement changes the answer:
//!
//! > 74.8% of profile rows are sourced, and **three sections are 88.9–100% unattributed while every
//! > other populated section is 0%**. Addresses are 100% unattributed corpus-wide — all 173,203
//! > `LocatedAt` destinations sit in the `location:`/`LOC:` namespaces, which every mapper that
//! > normalises an address mints into.
//!
//! So an unattributed row is almost never an unlucky row; it is a row in a section whose *whole*
//! provenance is structurally unestablishable. Rendering sixty identical `Unattributed` chips down
//! an address column is sixty repetitions of one fact, and 45-04's own recommendation was to state
//! it once, at the section, using the read path's refusal reason verbatim.
//!
//! This module therefore renders:
//!
//! | Case | Row | Section |
//! |---|---|---|
//! | attributed | `{source} · {date}` chip, plus the tier badge where the fact came from an identity link | silent |
//! | some rows unattributed | those rows carry an `Unattributed` chip — they are the exception, so they are pointed at | "Provenance incomplete — {k} of {n} rows …" plus the reasons |
//! | every row unattributed | no chip | "Provenance incomplete — no source is recorded for any of these {n} rows …" plus the reasons |
//! | not a single-source fact | no chip | "Not attributable to one source: …" — [`LpaPresenceView`] is a count spanning two datasets, and naming one would be the placeholder attribution D-04 forbids |
//!
//! What does **not** change is the rule the spec exists to protect: a fact whose provenance cannot
//! be established is never defaulted to a plausible-looking source, and never rendered as though it
//! were sourced. It is stated — once, where it is true, in the words the read path used.
//!
//! # The two tier tokens ship as a neutral pair, not as two new hues
//!
//! The phase 45 UI spec declares `--tier-confirmed` and `--tier-auto` with no values, and its own
//! checker flagged that: "indigo, cyan, slate, orange, teal, pink and lime are taken by the node
//! palette … so a tier badge and a section marker can collide in one row". Counting `tokens.css`,
//! seven node hues, six edge hues and five semantic roles are already spoken for. Rather than take
//! the eighteenth hue and hope, the pair is **`--foreground` and `--muted-foreground`**: a
//! human-confirmed link reads solid, an auto-established one reads quiet, the distinction an
//! analyst must make is carried by the word as well as the weight, and neither can be mistaken for
//! a verdict or for a node kind. `pending_review` has no token and no variant — ENR-11 — and
//! `LinkTier` has no third variant to construct one from.

use leptos::prelude::*;
use urban_integrity_workbench_dto::{EntityProfileDto, LinkTier, Provenance};

use crate::abbr::Abbr;
use crate::glossary;
use crate::profile::sections::{iso_date, section_rows, SectionId, SECTION_ORDER};

/// The chip on a row whose provenance could not be established. Rendered only where such a row is
/// the exception in its section — see the module doc.
pub const UNATTRIBUTED: &str = "Unattributed";

/// The section marker's opening words, shared by both incomplete cases so an analyst scanning the
/// page recognises the same statement wherever it appears.
pub const PROVENANCE_INCOMPLETE: &str = "Provenance incomplete";

/// How a source-dataset value reaches the screen.
///
/// **One lookup, and every value the wire can carry goes through it.** The phase 45 UI spec makes this a
/// contract rather than a call-site convention, because the value is data: a new adapter whose name
/// happens to be a glossary token would otherwise ship an unwrapped abbreviation, and the
/// `<abbr>` sweep cannot see a string that is not in the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceLabel {
    /// The value is a glossary token and renders inside `<Abbr>` (D-03/NAV-04).
    Glossary(String),
    /// The value is not a glossary token — `OpenSanctions`, `detector`, `probe`, or an adapter
    /// naming itself. Rendered as plain text, never wrapped in an `<abbr>` that would have no
    /// expansion to show.
    Plain(String),
}

impl SourceLabel {
    pub fn text(&self) -> &str {
        match self {
            SourceLabel::Glossary(s) | SourceLabel::Plain(s) => s,
        }
    }
}

/// The single lookup every source-dataset value passes through. See [`SourceLabel`].
///
/// **"In the glossary" is a broader question than "is an abbreviation", and this function asks the
/// narrower one.** `docs/abbreviations.md` carries rows for things that are names rather than
/// abbreviations — `OpenSanctions`, whose meaning column is an em dash precisely because there is
/// nothing to expand, and `Arcus`. `glossary::lookup` finds those; wrapping one in `<abbr>` would
/// produce a tooltip reading "— — External sanctions/PEP screening service". So the predicate here
/// is the one D-04's own guard and the `<abbr>` sweep use: two-to-six characters, all ASCII
/// uppercase, and present in the glossary. A value failing any of the three is plain text.
pub fn source_label(source: &str) -> SourceLabel {
    let shaped_like_an_abbreviation =
        (2..=6).contains(&source.len()) && source.chars().all(|c| c.is_ascii_uppercase());
    if shaped_like_an_abbreviation && glossary::lookup(source).is_some() {
        SourceLabel::Glossary(source.to_string())
    } else {
        SourceLabel::Plain(source.to_string())
    }
}

/// The word for an identity link's confidence tier.
///
/// The wire spellings are the `identity_links.status` values (`"confirmed"`, `"auto_linked"`); this
/// is what an analyst reads, so the underscore goes.
pub fn tier_label(tier: LinkTier) -> &'static str {
    match tier {
        LinkTier::Confirmed => "confirmed",
        LinkTier::AutoLinked => "auto-linked",
    }
}

/// The CSS custom property a tier badge draws from. See the module doc for why the pair is neutral.
pub fn tier_token(tier: LinkTier) -> &'static str {
    match tier {
        LinkTier::Confirmed => "var(--tier-confirmed)",
        LinkTier::AutoLinked => "var(--tier-auto)",
    }
}

/// The chip's own text: `{source} · {date}`, or `None` for an unattributed fact (whose rendering is
/// the section's business — see the module doc).
///
/// The date is the ISO date of the `graph_events` timestamp, which is the spec's own example
/// (`CH · 2026-07-31`). It deliberately does **not** go through the `Timestamp` component: that
/// renders `{relative} · {absolute}` — "2 months ago · 31 Jul 2026, 00:00" — which cannot sit in a
/// 20px non-interactive chip beside three table columns. The shared ISO-date helper the temporal
/// column already uses is what keeps the two date renderings in one place.
pub fn chip_text(provenance: &Provenance) -> Option<(SourceLabel, String, Option<LinkTier>)> {
    match provenance {
        Provenance::Unattributed { .. } => None,
        Provenance::Attributed {
            source_dataset,
            observed_at,
            tier,
        } => Some((
            source_label(source_dataset),
            iso_date(observed_at).to_string(),
            *tier,
        )),
    }
}

/// What a whole section can say about where its rows came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionProvenance {
    /// Nothing to attribute.
    NoRows,
    /// Every row states its source. The section says nothing; the chips have said it.
    AllSourced,
    /// Some rows do and some do not. The unattributed rows are the exception, so they carry a chip
    /// and the marker counts them.
    PartlySourced {
        unattributed: usize,
        total: usize,
        reasons: Vec<String>,
    },
    /// No row states a source. The marker says it once for all of them and the rows carry no chip.
    NoneSourced { total: usize, reasons: Vec<String> },
    /// The section's rows are not single-source facts at all.
    NotASingleSourceFact { reason: &'static str },
}

/// Why the LPA presence section carries no provenance — `LpaPresenceView`'s own reason, in the
/// words its doc comment uses.
pub const LPA_PRESENCE_REASON: &str =
    "it counts rows from two datasets (Contracts Finder and the planning portals); the rows it \
     counts carry their own provenance in the contracts and planning-application sections";

/// Classifies one section's rows. `reasons` are the read path's refusal strings, **verbatim and
/// deduplicated** — 45-04's instruction was "the three reasons are short, distinct and already
/// written for a human; they are the copy for the section marker, do not paraphrase them".
pub fn section_provenance(rows: &[Option<Provenance>]) -> SectionProvenance {
    if rows.is_empty() {
        return SectionProvenance::NoRows;
    }
    if rows.iter().all(Option::is_none) {
        return SectionProvenance::NotASingleSourceFact {
            reason: LPA_PRESENCE_REASON,
        };
    }
    let mut reasons: Vec<String> = Vec::new();
    let mut unattributed = 0usize;
    for provenance in rows.iter().flatten() {
        if let Provenance::Unattributed { reason } = provenance {
            unattributed += 1;
            if !reasons.contains(reason) {
                reasons.push(reason.clone());
            }
        }
    }
    let total = rows.len();
    match unattributed {
        0 => SectionProvenance::AllSourced,
        n if n == total => SectionProvenance::NoneSourced { total, reasons },
        n => SectionProvenance::PartlySourced {
            unattributed: n,
            total,
            reasons,
        },
    }
}

/// The section marker's sentence, or `None` when the section has nothing to declare.
///
/// Singular forms are written out rather than left to an `s`: the phase 45 UI spec's checker found the
/// spec's own copy misreading at n=1 (`1 Contracts exist but fall outside…`), and a marker that
/// said "1 of 3 rows here have no recorded source" would be the same defect in a new place.
pub fn section_marker(provenance: &SectionProvenance) -> Option<String> {
    match provenance {
        SectionProvenance::NoRows | SectionProvenance::AllSourced => None,
        SectionProvenance::NotASingleSourceFact { reason } => {
            Some(format!("Not attributable to one source: {reason}."))
        }
        SectionProvenance::PartlySourced {
            unattributed,
            total,
            reasons,
        } => {
            // The verb agrees with the count ("1 … has", "2 … have"); the noun agrees with the
            // TOTAL, because it is "1 of 3 rows" that is being counted. Getting these from one
            // number produced "1 of 3 row here has" on the first run of the test below.
            let verb = if *unattributed == 1 { "has" } else { "have" };
            let noun = if *total == 1 { "row" } else { "rows" };
            Some(format!(
                "{PROVENANCE_INCOMPLETE} — {unattributed} of {total} {noun} here {verb} no \
                 recorded source: {}.",
                reasons.join("; ")
            ))
        }
        SectionProvenance::NoneSourced { total, reasons } => {
            let subject = if *total == 1 {
                "this row".to_string()
            } else {
                format!("any of these {total} rows")
            };
            Some(format!(
                "{PROVENANCE_INCOMPLETE} — no source is recorded for {subject}: {}.",
                reasons.join("; ")
            ))
        }
    }
}

/// Whether a row with no source should carry its own `Unattributed` chip. See the module doc: only
/// where it is the exception in its section.
pub fn shows_unattributed_chips(provenance: &SectionProvenance) -> bool {
    matches!(provenance, SectionProvenance::PartlySourced { .. })
}

/// Every fact on the profile, and how many of them state no source.
///
/// Counts the rows **on screen** — the windows the read path returned — rather than each section's
/// unwindowed total, because it is a statement about what the reader is looking at. 45-04's answer
/// to the phase 45 UI spec Open Question 3 was to keep this count ("74.8% sourced, so it is a meaningful
/// statement rather than a shrug") *and* add the section marker, not to replace one with the other.
pub fn profile_fact_counts(profile: &EntityProfileDto) -> (usize, usize) {
    let mut facts = 0usize;
    let mut unattributed = 0usize;
    for section in SECTION_ORDER {
        for row in section_rows(profile, *section) {
            match row.provenance {
                // A row that is not a single-source fact is not counted as an unattributed one:
                // it is a different statement, and the section says it.
                None => {}
                Some(Provenance::Attributed { .. }) => facts += 1,
                Some(Provenance::Unattributed { .. }) => {
                    facts += 1;
                    unattributed += 1;
                }
            }
        }
    }
    (facts, unattributed)
}

/// `Compact`'s one-line provenance summary (the phase 45 UI spec: "{n} facts, all sourced" /
/// "{n} facts, {k} unattributed"), which at 280px replaces the per-row chips entirely.
pub fn compact_provenance_summary(profile: &EntityProfileDto) -> String {
    let (facts, unattributed) = profile_fact_counts(profile);
    let noun = if facts == 1 { "fact" } else { "facts" };
    if unattributed == 0 {
        format!("{facts} {noun}, all sourced")
    } else {
        format!("{facts} {noun}, {unattributed} unattributed")
    }
}

/// One fact's provenance: `{source} · {date}` with the identity-link tier where there is one.
///
/// Not interactive — hence the 20px height exception to the 44px hit target. Provenance is context,
/// not a destination.
#[component]
#[allow(non_snake_case)]
pub fn ProvenanceChip(
    provenance: Provenance,
    /// Whether an unattributed row carries a chip of its own in this section
    /// ([`shows_unattributed_chips`]).
    show_unattributed: bool,
) -> impl IntoView {
    match chip_text(&provenance) {
        None => (show_unattributed)
            .then(|| {
                view! {
                    <span class="text-label" style="color: var(--muted-foreground);">
                        {UNATTRIBUTED}
                    </span>
                }
            })
            .into_any(),
        Some((source, date, tier)) => {
            let source_view = match source {
                SourceLabel::Glossary(token) => view! { <Abbr token=token /> }.into_any(),
                SourceLabel::Plain(text) => view! { <span>{text}</span> }.into_any(),
            };
            view! {
                <span
                    class="text-label"
                    style="display: inline-flex; align-items: center; gap: var(--space-xs); min-height: 20px; color: var(--muted-foreground); white-space: nowrap;"
                >
                    {source_view}
                    <span aria-hidden="true">"·"</span>
                    <span class="font-mono">{date}</span>
                    {tier.map(|tier| view! {
                        <span
                            class="text-label"
                            style=format!("border: 1px solid color-mix(in srgb, {token} 30%, transparent); background: color-mix(in srgb, {token} 15%, transparent); color: {token}; border-radius: 4px; padding: 0 var(--space-xs);", token = tier_token(tier))
                        >
                            {tier_label(tier)}
                        </span>
                    })}
                </span>
            }
            .into_any()
        }
    }
}

/// The section-level provenance marker, or nothing when the section has nothing to declare.
#[component]
#[allow(non_snake_case)]
pub fn SectionProvenanceMarker(section: SectionId, profile: EntityProfileDto) -> impl IntoView {
    let rows: Vec<Option<Provenance>> = section_rows(&profile, section)
        .into_iter()
        .map(|row| row.provenance)
        .collect();
    let marker = section_marker(&section_provenance(&rows));
    view! {
        {marker.map(|text| view! {
            <p class="text-label" style="margin: 0; color: var(--muted-foreground);">{text}</p>
        })}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::test_fixtures::{
        attributed, empty_profile, linked, sample_profile, unattributed, DIRECTOR_NAMESPACE_REASON,
        LOCATION_NAMESPACE_REASON,
    };

    /// Every dataset name the read path can put on the wire resolves through the one lookup, and
    /// the ones that are glossary tokens come back wrapped.
    ///
    /// The list is `crates/schema/src/types/attribution.rs`'s dataset constants. It is repeated
    /// here because `workbench-web` must never depend on `crates/schema` (the wasm boundary guard),
    /// and pinned against that file directly by
    /// `every_source_dataset_the_read_path_can_emit_resolves_through_the_chip_lookup` in
    /// `tests/abbr_reachable.rs`, which reads the real source.
    #[test]
    fn a_glossary_dataset_is_wrapped_and_a_non_glossary_one_is_not() {
        for token in [
            "CH", "CF", "LPA", "FCA", "CC", "EC", "CCOD", "OCOD", "ICIJ", "OC",
        ] {
            assert_eq!(
                source_label(token),
                SourceLabel::Glossary(token.to_string()),
                "{token} is a glossary token and must render through <Abbr>"
            );
        }
        // Non-vacuity in both directions: this project's own producers are deliberately lowercase
        // and deliberately not abbreviations, so an `<abbr>` with no expansion must not be built
        // for them.
        for plain in ["OpenSanctions", "detector", "probe", "planning_identity"] {
            assert_eq!(
                source_label(plain),
                SourceLabel::Plain(plain.to_string()),
                "{plain} is not an abbreviation and must not be wrapped in <abbr>"
            );
        }
        // `OpenSanctions` IS in `docs/abbreviations.md` — with an em dash for its meaning, because
        // there is nothing to expand. It is the case that separates "in the glossary" from "is an
        // abbreviation", and the reason `source_label` asks the narrower question.
        assert!(
            glossary::lookup("OpenSanctions").is_some(),
            "this assertion is what makes the case above non-trivial"
        );
    }

    #[test]
    fn a_chip_states_the_source_the_date_and_the_tier() {
        let (source, date, tier) =
            chip_text(&linked("EC", LinkTier::Confirmed)).expect("an attributed fact has a chip");
        assert_eq!(source, SourceLabel::Glossary("EC".to_string()));
        assert_eq!(
            date, "2026-07-31",
            "the chip carries the date, not the instant"
        );
        assert_eq!(tier, Some(LinkTier::Confirmed));
        assert_eq!(tier_label(LinkTier::Confirmed), "confirmed");
        assert_eq!(tier_label(LinkTier::AutoLinked), "auto-linked");
        // A fact read straight from a dataset is not link-derived, and says so by carrying no tier.
        assert_eq!(chip_text(&attributed("CH")).expect("chip").2, None);
        assert_eq!(
            chip_text(&unattributed("no source event found for this row")),
            None
        );
    }

    /// The tier badge must not borrow a hue that already means something. Both tokens resolve to
    /// the neutral pair, so the badge cannot be mistaken for a verdict or a node kind.
    #[test]
    fn the_tier_tokens_are_the_neutral_pair_and_not_a_verdict_colour() {
        assert_eq!(tier_token(LinkTier::Confirmed), "var(--tier-confirmed)");
        assert_eq!(tier_token(LinkTier::AutoLinked), "var(--tier-auto)");
        let css = include_str!("../../style/tokens.css");
        for (token, value) in [
            ("--tier-confirmed", "var(--foreground)"),
            ("--tier-auto", "var(--muted-foreground)"),
        ] {
            assert!(
                css.contains(&format!("{token}: {value}")),
                "{token} must be declared in tokens.css as {value}"
            );
        }
        for reserved in ["--success", "--warning", "--destructive", "--accent"] {
            assert!(
                !css.contains(&format!("--tier-confirmed: var({reserved})")),
                "a tier badge must not borrow {reserved}"
            );
        }
    }

    /// A section where every row is unattributed states it once, in the read path's own words, and
    /// suppresses the per-row chip — 45-04's "sixty identical chips down an address column is sixty
    /// repetitions of one fact".
    #[test]
    fn a_wholly_unattributed_section_states_it_once_and_drops_the_row_chips() {
        let rows = vec![
            Some(unattributed(LOCATION_NAMESPACE_REASON)),
            Some(unattributed(LOCATION_NAMESPACE_REASON)),
            Some(unattributed(LOCATION_NAMESPACE_REASON)),
        ];
        let provenance = section_provenance(&rows);
        assert_eq!(
            provenance,
            SectionProvenance::NoneSourced {
                total: 3,
                reasons: vec![LOCATION_NAMESPACE_REASON.to_string()],
            },
            "one reason, deduplicated, not three copies of it"
        );
        let marker = section_marker(&provenance).expect("a marker is rendered");
        assert!(marker
            .starts_with("Provenance incomplete — no source is recorded for any of these 3 rows:"));
        assert!(
            marker.contains(LOCATION_NAMESPACE_REASON),
            "the reason is the read path's, verbatim: {marker}"
        );
        assert!(!shows_unattributed_chips(&provenance));
    }

    /// A section where SOME rows are unattributed points at them — the marker counts, and the rows
    /// themselves carry a chip, because there an absent chip would be ambiguous.
    #[test]
    fn a_partly_sourced_section_counts_the_exceptions_and_chips_them() {
        let rows = vec![
            Some(attributed("CH")),
            Some(unattributed(DIRECTOR_NAMESPACE_REASON)),
            Some(attributed("CH")),
        ];
        let provenance = section_provenance(&rows);
        let marker = section_marker(&provenance).expect("a marker is rendered");
        assert_eq!(
            marker,
            format!("Provenance incomplete — 1 of 3 rows here has no recorded source: {DIRECTOR_NAMESPACE_REASON}.")
        );
        assert!(shows_unattributed_chips(&provenance));
        // Non-vacuity: a fully sourced section declares nothing at all, so "a marker is always
        // rendered" cannot be what makes the assertion above pass.
        assert_eq!(
            section_provenance(&[Some(attributed("CH"))]),
            SectionProvenance::AllSourced
        );
        assert_eq!(section_marker(&SectionProvenance::AllSourced), None);
    }

    /// The LPA presence section is not unattributed — it is not a single-source fact, which is a
    /// different statement and gets different words.
    #[test]
    fn a_count_spanning_two_datasets_says_so_rather_than_claiming_to_be_unattributed() {
        let provenance = section_provenance(&[None, None]);
        let marker = section_marker(&provenance).expect("a marker is rendered");
        assert!(marker.starts_with("Not attributable to one source:"));
        assert!(
            !marker.contains(PROVENANCE_INCOMPLETE),
            "an un-attributable count must not read as a provenance failure: {marker}"
        );
        assert!(!shows_unattributed_chips(&provenance));
    }

    #[test]
    fn the_compact_summary_counts_facts_and_the_unsourced_among_them() {
        let profile = sample_profile();
        let (facts, unattributed) = profile_fact_counts(&profile);
        assert!(
            facts > 0,
            "the fixture must have facts or this proves nothing"
        );
        assert!(unattributed > 0, "and some unsourced ones");
        assert_eq!(
            compact_provenance_summary(&profile),
            format!("{facts} facts, {unattributed} unattributed")
        );
        // An LPA presence row is a fact of no single source, and is counted as neither sourced nor
        // unsourced — it is not a claim with a producer, so it is not in the denominator either.
        let rendered_rows: usize = SECTION_ORDER
            .iter()
            .map(|section| section_rows(&profile, *section).len())
            .sum();
        assert!(
            !profile.lpa_presence.is_empty(),
            "the fixture has an LPA row"
        );
        assert_eq!(
            facts,
            rendered_rows - profile.lpa_presence.len(),
            "every rendered row except the un-attributable ones is a counted fact"
        );
        // An "empty" profile still states one fact: its status. `StatusFactView` is not optional —
        // an entity always has a status and that status always carries provenance — so the count
        // an empty profile reports is 1, not 0, and saying 0 would be the screen disagreeing with
        // the row it is rendering.
        let empty = empty_profile();
        assert_eq!(profile_fact_counts(&empty), (1, 0));
        assert_eq!(compact_provenance_summary(&empty), "1 fact, all sourced");
    }
}
