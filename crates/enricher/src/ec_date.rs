//! Electoral Commission `AcceptedDate` parsing (POL-04).
//!
//! The EC donations CSV publishes `AcceptedDate` as **`DD/MM/YYYY`** (e.g.
//! `20/12/2010`), not ISO. Two sites independently assumed ISO and parsed with
//! `%Y-%m-%d`:
//!
//! - `adapters::electoral_commission` discarded the failure with `.ok()`, so the
//!   typed `accepted_date` was `None` for **every** EC record ever ingested, with
//!   no log line, while the raw `DD/MM/YYYY` string was still written into
//!   `evidence_json`.
//! - `donation_correlation` read that string back, applied `%Y-%m-%d` again, and
//!   skipped the donation — so the detector could never fire on a real row. Its
//!   `warn!` is the only reason the bug was visible at all.
//!
//! Both now route through [`parse_ec_date`], which accepts the real feed format
//! **and** ISO. Accepting both is deliberate rather than tidy-minded: rows
//! written before this fix hold `DD/MM/YYYY` in `evidence_json` while rows
//! written after hold ISO, so the detector must read both or a re-ingest becomes
//! a prerequisite for the fix.

use chrono::NaiveDate;

/// Formats accepted for an EC `AcceptedDate`, in match order.
///
/// `%d/%m/%Y` is first because it is what the live feed publishes; ISO is kept
/// for `evidence_json` written after this fix, and for hand-built fixtures.
const EC_DATE_FORMATS: &[&str] = &["%d/%m/%Y", "%Y-%m-%d"];

/// Parse an EC `AcceptedDate`, accepting `DD/MM/YYYY` or `YYYY-MM-DD`.
///
/// Returns `None` when the input matches neither — callers are expected to log
/// and skip rather than silently drop, which is what let this bug survive.
///
/// Day-first is tried before ISO, and the two are unambiguous against each other
/// (`/` vs `-`), so ordering only matters for inputs that are malformed anyway.
pub fn parse_ec_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    EC_DATE_FORMATS
        .iter()
        .find_map(|fmt| NaiveDate::parse_from_str(s, fmt).ok())
}

/// Normalise an EC `AcceptedDate` to ISO `YYYY-MM-DD` for storage.
///
/// Returns `None` for unparseable input so the caller keeps the decision about
/// what to persist; callers writing `evidence_json` fall back to the raw string
/// so the audit trail never loses what the feed actually said.
pub fn to_iso(s: &str) -> Option<String> {
    parse_ec_date(s).map(|d| d.format("%Y-%m-%d").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: the live EC feed format. Both call sites parsed this with
    /// `%Y-%m-%d` and failed — silently in the adapter, loudly in the detector.
    #[test]
    fn parses_the_day_first_format_the_ec_feed_actually_publishes() {
        // Values taken verbatim from identity_links.evidence_json in the live
        // an_anglesey_aebaa785 database, 2026-08-14.
        for raw in [
            "20/12/2010",
            "01/04/2003",
            "28/03/2006",
            "30/01/2015",
            "12/09/2007",
        ] {
            let parsed =
                parse_ec_date(raw).unwrap_or_else(|| panic!("live EC date {raw} must parse"));
            assert_eq!(parsed.format("%d/%m/%Y").to_string(), raw);
        }
    }

    #[test]
    fn day_first_maps_to_the_right_day_and_month() {
        // 01/04/2003 is 1 April, not 4 January — the failure mode that makes a
        // permissive "try both" parser dangerous if ISO were tried first.
        let d = parse_ec_date("01/04/2003").expect("parses");
        assert_eq!(d.format("%Y-%m-%d").to_string(), "2003-04-01");
    }

    #[test]
    fn still_accepts_iso_for_rows_written_after_this_fix() {
        let d = parse_ec_date("2023-10-15").expect("parses");
        assert_eq!(d.format("%Y-%m-%d").to_string(), "2023-10-15");
    }

    #[test]
    fn tolerates_surrounding_whitespace() {
        assert!(parse_ec_date("  20/12/2010  ").is_some());
    }

    #[test]
    fn rejects_junk_rather_than_guessing() {
        for raw in ["", "not-a-date", "2010", "20-12-2010", "12/2010"] {
            assert!(parse_ec_date(raw).is_none(), "{raw} must not parse");
        }
    }

    #[test]
    fn to_iso_normalises_day_first_and_passes_iso_through() {
        assert_eq!(to_iso("20/12/2010").as_deref(), Some("2010-12-20"));
        assert_eq!(to_iso("2023-10-15").as_deref(), Some("2023-10-15"));
        assert_eq!(to_iso("not-a-date"), None);
    }
}
