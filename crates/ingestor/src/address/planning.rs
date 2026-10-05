//! Planning-portal free-text address parser (D-07).
//!
//! Turns a planning application's free-text `address` field into the same structured shape
//! PPD stores: `(postcode, saon, paon, street, locality, town)`. This replaces
//! `land_flip.rs`'s `normalise_app_address`, which flattens the whole address to one string
//! and regexes the front — a shape that cannot distinguish a SAON from a PAON in principle
//! (D-15).
//!
//! ## Two-stage split (the SPEC's original comma-first sketch is inverted here — D-25)
//!
//! 1. Split on the bilingual `" / "` delimiter first. 95.1% of live planning addresses are a
//!    Welsh/English pair written twice (e.g. `"... CAERGYBI / ... HOLYHEAD, LL65 2TU"`).
//!    Splitting on commas first shreds both halves into one token soup on 95% of the corpus, so
//!    the `" / "` split must come first; the English, postcode-bearing half is the one parsed.
//! 2. Only then split the chosen half on `,`, dropping empty fields (D-27, 38.2% of the corpus)
//!    and classifying the remaining tokens (SAON vs PAON vs street, D-15/D-28).
//!
//! A leading number followed by a comma then a building name (e.g. `"1, Deban, Crigyll Road"`)
//! is a SAON on that building, never a PAON — `extract_house_number`'s "first digit-leading
//! token" heuristic is exactly the wrong tool here (it stays correct, and unmodified, for
//! PPD/CH/CF's already-clean strings, which is why it is not reused in this module).
//!
//! Every field degrades to `None`/empty on malformed input; this function never panics (V5).

use regex::Regex;

use crate::planning::mapper::extract_postcode;

use super::normalise_postcode;

/// The structured shape of a parsed planning address, mirroring PPD's column layout.
///
/// `street` is non-optional but may be empty, matching `land_flip.rs`'s existing convention
/// where an empty string means "component absent, disables gate-1" rather than `Option<String>`
/// — so downstream code that currently checks `.is_empty()` needs no restructuring when 29-08
/// wires this parser in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedPlanningAddress {
    pub postcode: Option<String>,
    pub saon: Option<String>,
    pub paon: Option<String>,
    pub street: String,
    pub locality: Option<String>,
    pub town: Option<String>,
}

/// Re-implements `land_registry_ppd::normalise_street`'s exact transform (lowercase, keep only
/// alphanumeric + whitespace, collapse whitespace) as a private local function rather than an
/// import.
///
/// This is duplicated, not imported, because of the workspace's crate dependency direction:
/// `crates/enricher` depends on `crates/ingestor` (see `crates/enricher/Cargo.toml`), so
/// `crates/ingestor` importing from `crates/enricher::adapters::land_registry_ppd` would be a
/// circular crate dependency and does not compile. D-08 only forbids *changing what
/// `normalise_street` does for PPD strings*; it says nothing about a second, independent
/// implementation of the same trivial transform for a different crate's caller — 29-01's
/// regression test continues to pin the original function, unmodified, for PPD inputs.
fn normalise_street_local(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// If `segment` starts with a bare PAON range (`"1-2"`, `"1 -5"`, D-27 — 0.6% of the corpus),
/// return the byte offset where the matched range prefix ends. PPD does not model ranges the
/// same way, so the caller must disable the PAON component entirely rather than guess a side.
fn bare_paon_range_end(segment: &str) -> Option<usize> {
    let re = Regex::new(r"^\d+\s*-\s*\d+[A-Za-z]?\b").ok()?;
    re.find(segment).map(|m| m.end())
}

/// Split a leading digit-run token (optionally with one trailing letter, e.g. `"1A"`) off
/// `segment`. Returns `(number, rest)` where `rest` is `None` if the segment is JUST the number
/// (nothing else — the SAON-on-a-named-building shape, D-15) and `Some(remainder)` if more
/// content follows in the same segment (the PAON shape, D-28's 23.0% case). Returns `None` if
/// `segment` does not start with a digit at all (D-28's dominant 74.2% case).
fn split_leading_number(segment: &str) -> Option<(String, Option<String>)> {
    let re = Regex::new(r"^([0-9]+[A-Za-z]?)(?:\s+(.+))?$").ok()?;
    let caps = re.captures(segment)?;
    let number = caps.get(1)?.as_str().to_string();
    let rest = caps.get(2).map(|m| m.as_str().to_string());
    Some((number, rest))
}

/// Best-effort assignment of any trailing comma segments to `(locality, town)`. Neither field is
/// load-bearing for gate-1 (only `postcode`/`paon`/`street` are, per D-09) — this is Claude's
/// discretion on exact assignment order, documented here: the LAST remaining segment is treated
/// as the town, and (when more than one segment remains) everything before it is joined into
/// `locality`.
fn assign_locality_town(remaining: &[&str]) -> (Option<String>, Option<String>) {
    match remaining.len() {
        0 => (None, None),
        1 => (None, Some(remaining[0].trim().to_string())),
        _ => (
            Some(remaining[..remaining.len() - 1].join(", ")),
            Some(remaining[remaining.len() - 1].trim().to_string()),
        ),
    }
}

/// Choose which half of a bilingual `" / "`-joined address to parse: the one carrying a
/// recognisable UK postcode (D-25 — 95.1% of live planning addresses are a Welsh/English pair
/// written twice; the English, postcode-bearing half is the one PPD's postcode-keyed data can
/// match against).
///
/// Degenerate-input rule (documented per Task 4): if no `" / "` delimiter exists, the whole
/// string is used. If a delimiter exists but no half carries a postcode (e.g. `"A / B / C"`),
/// fall back to the last `" / "`-delimited segment.
fn choose_bilingual_half(raw: &str) -> &str {
    let parts: Vec<&str> = raw.split(" / ").collect();
    if parts.len() < 2 {
        return raw;
    }
    if let Some(part) = parts.iter().find(|p| extract_postcode(p).is_some()) {
        return part;
    }
    parts.last().copied().unwrap_or(raw)
}

/// Parse a planning portal's free-text address into its structured components.
///
/// See the module doc for the two-stage split this function performs. Never panics — malformed
/// or unparseable input degrades every field to `None`/empty (V5).
pub fn parse_planning_address(raw: &str) -> ParsedPlanningAddress {
    let chosen_half = choose_bilingual_half(raw);

    // Comma-segment the chosen half, dropping empty fields (D-27's empty-field guard — 38.2% of
    // the corpus turns a naive comma split into a phantom component otherwise).
    let mut segments: Vec<&str> = chosen_half
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    // Locate and exclude the postcode SEGMENT — not a substring replace (D-14's exact-substring
    // defect is exactly what this avoids: `normalise_app_address`'s `address.replace(pc, " ")`
    // survives a differently-spaced/cased occurrence). Scan from the end: the postcode
    // conventionally trails the address.
    let postcode = segments
        .iter()
        .rposition(|seg| normalise_postcode(seg).is_some())
        .and_then(|idx| {
            let pc = normalise_postcode(segments[idx]);
            segments.remove(idx);
            pc
        });

    if segments.is_empty() {
        return ParsedPlanningAddress {
            postcode,
            saon: None,
            paon: None,
            street: String::new(),
            locality: None,
            town: None,
        };
    }

    let first = segments[0];

    // 1. A bare PAON range disables the PAON component entirely (D-27, 0.6% of the corpus) —
    // never guess a side.
    if let Some(range_end) = bare_paon_range_end(first) {
        let street_candidate = first.get(range_end..).unwrap_or("").trim();
        let (locality, town) = assign_locality_town(&segments[1..]);
        return ParsedPlanningAddress {
            postcode,
            saon: None,
            paon: None,
            street: normalise_street_local(street_candidate),
            locality,
            town,
        };
    }

    // 2. A leading digit run.
    if let Some((number, rest_of_segment)) = split_leading_number(first) {
        match rest_of_segment {
            // "86 Parc Tyddyn Bach" — more words in the SAME segment: the leading number is a
            // PAON (D-28's 23.0% shape).
            Some(rest) => {
                let (locality, town) = assign_locality_town(&segments[1..]);
                ParsedPlanningAddress {
                    postcode,
                    saon: None,
                    paon: Some(number),
                    street: normalise_street_local(&rest),
                    locality,
                    town,
                }
            }
            // "1" alone — nothing else in the same segment, so a comma immediately follows the
            // number before the next real token.
            //
            // Gap-closure (2026-08-07): this was originally read as the SAON-on-a-named-building
            // shape (D-15), with the number folded into `saon` and `paon` left `None`. Measured
            // against 9 CONFIRMED genuine (planning, PPD) pairs from the live, fully re-ingested
            // an_anglesey_aebaa785 corpus (matched on PPD's own paon_norm/street_norm authority),
            // that reading is wrong: an ordinary house number was being placed in the one field
            // gate-1 never reads (`gate1_saon_is_never_part_of_the_equality` pins that gate-1
            // ignores SAON entirely, per D-09), making a full (postcode, paon, street) match
            // structurally impossible regardless of what PPD holds. The hardest confirmed pair,
            // "11,Pen Gerddi,Plas Road,Holyhead", shows the correct reading directly: PPD says
            // paon=11, street='pen gerddi' — the segment IMMEDIATELY after the number is the
            // street, even though it reads like a building name and a naive parse would call the
            // segment after THAT ("Plas Road") the street instead. No positive corpus evidence
            // was found for a genuine SAON-then-street shape distinguishable from this one, so
            // per D-18's exact-match-only discipline (never manufacture a match, but also never
            // withhold a field PPD's own data proves is real) the bare number is treated as the
            // PAON and the very next segment as the street. `saon` is left `None` here — this
            // branch no longer claims to detect a SAON at all, only a PAON with no digits
            // embedded in its street token.
            None => {
                let rest_segs = &segments[1..];
                let street_candidate = rest_segs.first().copied().unwrap_or("");
                let (locality, town) =
                    assign_locality_town(rest_segs.get(1..).unwrap_or(&[] as &[&str]));
                ParsedPlanningAddress {
                    postcode,
                    saon: None,
                    paon: Some(number),
                    street: normalise_street_local(street_candidate),
                    locality,
                    town,
                }
            }
        }
    } else {
        // 3. No leading digit at all (D-28's dominant 74.2% case): the whole segment is
        // street/building-name material, PAON stays None.
        let (locality, town) = assign_locality_town(&segments[1..]);
        ParsedPlanningAddress {
            postcode,
            saon: None,
            paon: None,
            street: normalise_street_local(first),
            locality,
            town,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Task 2 (RED): six table-driven fixtures, verbatim from plan 29-07 <interfaces> /
    // the phase 29 design notes "The four defects the parser must fix" + D-25/D-27. All fail against the
    // Task 1 stub (which returns an all-empty/all-None ParsedPlanningAddress) until Task 3's
    // implementation lands.

    #[test]
    fn parses_paon_and_street_with_no_saon() {
        // "86 Parc Tyddyn Bach, LL65 1NZ" -- leading digit run followed by more words in the
        // same comma segment: the number is the PAON, the remainder is the street (D-28's
        // 23.0% shape).
        let parsed = parse_planning_address("86 Parc Tyddyn Bach, LL65 1NZ");
        assert_eq!(parsed.postcode, Some("LL651NZ".to_string()));
        assert_eq!(parsed.paon, Some("86".to_string()));
        assert_eq!(parsed.street, "parc tyddyn bach");
        assert_eq!(parsed.saon, None);
    }

    #[test]
    fn flat_number_before_building_name_is_paon_with_street_from_next_segment() {
        // "1, Deban, Crigyll Road, Rhosneigr, LL64 5QS" -- a leading number that is its OWN
        // comma segment, immediately followed by a building-name-shaped segment.
        //
        // [Gap-closure 2026-08-07, superseding this test's original name/assertions]: this was
        // originally asserted as a SAON on that building (D-15), with paon=None and
        // street="crigyll road" (the SECOND segment after the number). Measured against 9
        // confirmed genuine (planning, PPD) pairs from the live corpus, that reading is wrong:
        // PPD's own authority (see gap_closure_pen_gerddi_hardest_case_building_name_is_street_not_saon)
        // says the segment IMMEDIATELY after a bare number is the street, not the one after
        // that. The safety property this test originally guarded -- "must not manufacture a
        // false match against PPD paon='23' on street='crigyll road'" -- still holds under the
        // new reading, just via a different mechanism: street is now "deban", not "crigyll
        // road", so it structurally cannot collide with a PPD row keyed on "crigyll road"
        // either way.
        let parsed = parse_planning_address("1, Deban, Crigyll Road, Rhosneigr, LL64 5QS");
        assert_eq!(parsed.postcode, Some("LL645QS".to_string()));
        assert_eq!(parsed.paon, Some("1".to_string()));
        assert_eq!(parsed.street, "deban");
        assert_eq!(parsed.locality, Some("Crigyll Road".to_string()));
        assert_eq!(parsed.town, Some("Rhosneigr".to_string()));
    }

    #[test]
    fn market_hill_address_paon_and_street_from_bare_number_segment() {
        // "1, Market Hill, Holyhead, LL65 1NG" -- same bare-number-segment shape as above.
        //
        // [Gap-closure 2026-08-07, superseding this test's original name/assertions]: originally
        // asserted paon=None (SAON-on-building reading). Corrected: paon=Some("1"),
        // street="market hill" (the segment immediately after the number) -- the street value
        // is unchanged from before because this fixture only has one segment between the number
        // and the town, so both readings happened to agree on `street`; only `paon` changes.
        // The existing empty-PPD-PAON guard at land_flip.rs:139-145 is unaffected by this plan
        // and stays the actual defence against a planning paon of "1" matching a PPD row with
        // paon_norm=''.
        let parsed = parse_planning_address("1, Market Hill, Holyhead, LL65 1NG");
        assert_eq!(parsed.postcode, Some("LL651NG".to_string()));
        assert_eq!(parsed.paon, Some("1".to_string()));
        assert_eq!(parsed.street, "market hill");
    }

    #[test]
    fn bilingual_pair_parses_the_english_postcode_bearing_half() {
        // "1 BRO IARDDUR, BAE TREARDDUR, CAERGYBI / 1 BRO IARDDUR, TREARDDUR BAY, HOLYHEAD,
        // LL65 2TU" -- D-25's dominant shape (95.1% of the live corpus). The `" / "` split
        // must happen BEFORE any comma segmentation; only the RHS (English, postcode-bearing)
        // half is parsed, and the Welsh LHS tokens must not leak into any field.
        let raw = "1 BRO IARDDUR, BAE TREARDDUR, CAERGYBI / 1 BRO IARDDUR, TREARDDUR BAY, HOLYHEAD, LL65 2TU";
        let parsed = parse_planning_address(raw);
        assert_eq!(parsed.postcode, Some("LL652TU".to_string()));
        assert_eq!(parsed.paon, Some("1".to_string()));
        assert_eq!(parsed.street, "bro iarddur");
        // The Welsh LHS was discarded, not merged: its distinctive place name must not appear
        // anywhere in the parsed result. "caergybi" (Welsh for Holyhead) is the load-bearing
        // check here -- it has no English cognate, so its absence proves the LHS was discarded.
        // "trearddur" is deliberately NOT asserted absent: the RHS/English half legitimately
        // contains its own "TREARDDUR BAY" locality segment (this is a genuine bilingual place
        // name shared by both halves, not LHS leakage), so asserting its absence would fail
        // against a correctly-parsed result. [Deviation: plan 29-07's Task 2 <action> example
        // names "trearddur" alongside "caergybi" as tokens that must not leak from the LHS, but
        // that is unsatisfiable for a parser that correctly retains the RHS's own locality
        // (D-27's own worked example separately confirms "Trearddur Bay" is legitimate RHS
        // content). Rule 1 fix applied to this RED-phase test before GREEN implementation.]
        let all_text = format!(
            "{} {} {}",
            parsed.street,
            parsed.locality.clone().unwrap_or_default(),
            parsed.town.clone().unwrap_or_default()
        )
        .to_lowercase();
        assert!(!all_text.contains("caergybi"));
    }

    #[test]
    fn empty_field_is_dropped_not_phantom() {
        // "1 BONT LLWYD, , LLANFACHRAETH, CAERGYBI, LL65 4XX" -- constructed per D-27's
        // documented empty-field shape (38.2% of the corpus). The empty segment between the
        // two commas must be dropped entirely, not turned into a phantom locality/town.
        let raw = "1 BONT LLWYD, , LLANFACHRAETH, CAERGYBI, LL65 4XX";
        let parsed = parse_planning_address(raw);
        assert_eq!(parsed.postcode, Some("LL654XX".to_string()));
        assert_eq!(parsed.paon, Some("1".to_string()));
        assert_eq!(parsed.street, "bont llwyd");
        assert_ne!(parsed.locality, Some(String::new()));
        assert_ne!(parsed.town, Some(String::new()));
    }

    #[test]
    fn paon_range_disables_paon_component() {
        // "1-2 Bron Haul Cottages, LL65 3XX" -- constructed per D-27's documented range shape
        // (0.6% of the corpus). PPD does not model ranges the same way, so the PAON must be
        // disabled (None) rather than guessing a side.
        let raw = "1-2 Bron Haul Cottages, LL65 3XX";
        let parsed = parse_planning_address(raw);
        assert_eq!(parsed.postcode, Some("LL653XX".to_string()));
        assert_eq!(parsed.paon, None);
    }

    // --- Task 4: degenerate-input hardening (V5) -- every field degrades to None/empty on
    // malformed input; parse_planning_address must never panic and never index out of bounds.
    // Mirrors the existing "returns empty/None on invalid input, never panics" style used for
    // normalise_postcode/extract_house_number in address/mod.rs's own test suite.

    #[test]
    fn empty_string_does_not_panic() {
        let parsed = parse_planning_address("");
        assert_eq!(parsed.postcode, None);
        assert_eq!(parsed.paon, None);
        assert_eq!(parsed.saon, None);
        assert_eq!(parsed.street, String::new());
        assert_eq!(parsed.locality, None);
        assert_eq!(parsed.town, None);
    }

    #[test]
    fn no_comma_no_postcode_does_not_panic() {
        // "Rose Cottage" -- no delimiters at all, no recognisable postcode. The whole string
        // becomes street material (D-28's dominant no-leading-digit case); postcode stays None
        // rather than a guessed/partial match.
        let parsed = parse_planning_address("Rose Cottage");
        assert_eq!(parsed.postcode, None);
        assert_eq!(parsed.paon, None);
        assert_eq!(parsed.street, "rose cottage");
    }

    #[test]
    fn postcode_only_input_does_not_panic() {
        // "LL65 1AA" -- nothing but a postcode. The postcode still resolves; every other field
        // degrades to None/empty rather than indexing into a now-empty segment list.
        let parsed = parse_planning_address("LL65 1AA");
        assert_eq!(parsed.postcode, Some("LL651AA".to_string()));
        assert_eq!(parsed.paon, None);
        assert_eq!(parsed.saon, None);
        assert_eq!(parsed.street, String::new());
        assert_eq!(parsed.locality, None);
        assert_eq!(parsed.town, None);
    }

    #[test]
    fn multiple_bilingual_delimiters_does_not_panic() {
        // "A / B / C" -- two " / " occurrences, none of the three parts carries a postcode.
        // Documented degenerate-input rule (choose_bilingual_half): first half containing a
        // valid postcode: none here, so fall back to the LAST " / "-delimited segment ("C").
        let parsed = parse_planning_address("A / B / C");
        assert_eq!(parsed.postcode, None);
        assert_eq!(parsed.paon, None);
        assert_eq!(parsed.street, "c");
    }

    // --- Gap-closure (2026-08-07): 9 CONFIRMED genuine (planning, PPD) pairs from the live,
    // fully re-ingested an_anglesey_aebaa785 corpus, matched on PPD's own paon_norm/street_norm
    // authority at postcode+PAON+street granularity. The bare-number-segment branch (D-15) put
    // an ordinary house number into `saon` and left `paon = None`, which is structurally
    // unmatchable against gate-1's (postcode, paon, street) equality — see
    // the phase 29 plan 10 gap-closure notes (land-flip address matching) for the full defect
    // writeup. These 9 pairs FAIL against the pre-fix bare-number-is-SAON reading and MUST PASS
    // once the branch is corrected to bare-number-is-PAON, next-segment-is-street.

    #[test]
    fn gap_closure_pen_gerddi_hardest_case_building_name_is_street_not_saon() {
        // "11,Pen Gerddi,Plas Road,Holyhead,LL65 2AD" -- the hardest of the 9: a naive
        // building-name/street split would misclassify "Pen Gerddi" as a SAON-on-building and
        // "Plas Road" as the street. PPD's authority says paon=11, street='pen gerddi' -- the
        // segment IMMEDIATELY after a bare number is the street, full stop.
        let parsed = parse_planning_address("11,Pen Gerddi,Plas Road,Holyhead,LL65 2AD");
        assert_eq!(parsed.postcode, Some("LL652AD".to_string()));
        assert_eq!(parsed.paon, Some("11".to_string()));
        assert_eq!(parsed.street, "pen gerddi");
    }

    #[test]
    fn gap_closure_maes_cyttir() {
        let parsed = parse_planning_address("17,Maes Cyttir,Holyhead,LL65 2TH");
        assert_eq!(parsed.postcode, Some("LL652TH".to_string()));
        assert_eq!(parsed.paon, Some("17".to_string()));
        assert_eq!(parsed.street, "maes cyttir");
    }

    #[test]
    fn gap_closure_cleveland_crescent() {
        let parsed = parse_planning_address("21,CLEVELAND CRESCENT,HOLYHEAD,LL652LD");
        assert_eq!(parsed.postcode, Some("LL652LD".to_string()));
        assert_eq!(parsed.paon, Some("21".to_string()));
        assert_eq!(parsed.street, "cleveland crescent");
    }

    #[test]
    fn gap_closure_newry_street_37() {
        // The confirmed defect's own worked example: "37,NEWRY STREET,HOLYHEAD,LL65 1HP" was
        // producing saon=Some("37"), paon=None, making a full-triple match structurally
        // impossible even though PPD holds an exact postcode/paon/street match for this row.
        let parsed = parse_planning_address("37,NEWRY STREET,HOLYHEAD,LL65 1HP");
        assert_eq!(parsed.postcode, Some("LL651HP".to_string()));
        assert_eq!(parsed.paon, Some("37".to_string()));
        assert_eq!(parsed.street, "newry street");
    }

    #[test]
    fn gap_closure_church_terrace() {
        let parsed = parse_planning_address("4,Church Terrace,Holyhead,LL65 2HP");
        assert_eq!(parsed.postcode, Some("LL652HP".to_string()));
        assert_eq!(parsed.paon, Some("4".to_string()));
        assert_eq!(parsed.street, "church terrace");
    }

    #[test]
    fn gap_closure_maes_awel() {
        let parsed = parse_planning_address("4,Maes Awel,Llandegfan,LL59 5TF");
        assert_eq!(parsed.postcode, Some("LL595TF".to_string()));
        assert_eq!(parsed.paon, Some("4".to_string()));
        assert_eq!(parsed.street, "maes awel");
    }

    #[test]
    fn gap_closure_coedwig_terrace() {
        let parsed = parse_planning_address("5,Coedwig Terrace,Penmon,LL58 8SL");
        assert_eq!(parsed.postcode, Some("LL588SL".to_string()));
        assert_eq!(parsed.paon, Some("5".to_string()));
        assert_eq!(parsed.street, "coedwig terrace");
    }

    #[test]
    fn gap_closure_pentre_pella() {
        let parsed = parse_planning_address("7,Pentre Pella,Holyhead,LL65 1YR");
        assert_eq!(parsed.postcode, Some("LL651YR".to_string()));
        assert_eq!(parsed.paon, Some("7".to_string()));
        assert_eq!(parsed.street, "pentre pella");
    }

    #[test]
    fn gap_closure_newry_street_79() {
        let parsed = parse_planning_address("79,Newry Street,Holyhead,LL65 1HR");
        assert_eq!(parsed.postcode, Some("LL651HR".to_string()));
        assert_eq!(parsed.paon, Some("79".to_string()));
        assert_eq!(parsed.street, "newry street");
    }
}
