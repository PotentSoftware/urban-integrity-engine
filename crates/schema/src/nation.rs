//! Which UK nation an analysis, a ward, or a data source belongs to.
//!
//! ONS GSS codes encode the nation in their first character at every geography level —
//! `E…` England, `W…` Wales, `S…` Scotland, `N…` Northern Ireland — so the nation of an
//! analysis is derivable from the `ward_code` values already in `config.toml`, with no
//! new operator input.
//!
//! This matters because the four nations do not share data infrastructure. The
//! deprivation indices are four separate, non-comparable indices (England IMD 2019,
//! Wales WIMD 2019, Scotland SIMD 2020, NI NIMDM 2017), and HM Land Registry — CCOD,
//! OCOD and Price Paid Data alike — covers England and Wales only. Registers of Scotland
//! and Land & Property Services NI are separate registries this engine does not read.
//!
//! See the jurisdiction-gate spec.

use std::fmt;

/// One of the four UK nations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Nation {
    England,
    Wales,
    Scotland,
    NorthernIreland,
}

impl Nation {
    /// Every nation, in a fixed order — for exhaustive checks and operator output.
    pub const ALL: &'static [Nation] = &[
        Nation::England,
        Nation::Wales,
        Nation::Scotland,
        Nation::NorthernIreland,
    ];

    /// The nation an ONS GSS code belongs to, or `None` if the code is not a GSS code.
    ///
    /// A GSS code is one nation letter followed by eight digits (`W05001492`). The
    /// letter carries the nation at every level — ward, LAD, region — so this works for
    /// any of them. `K` (UK-wide) and `U` are deliberately NOT nations and return `None`
    /// rather than being folded into one, because a source or index scoped to "the UK"
    /// answers a different question from one scoped to a nation.
    ///
    /// Malformed input returns `None` rather than a guess: a wrong nation here silently
    /// selects the wrong deprivation index and the wrong property registry.
    pub fn from_gss_code(code: &str) -> Option<Nation> {
        let code = code.trim();
        if code.len() != 9 {
            return None;
        }
        let mut chars = code.chars();
        let letter = chars.next()?.to_ascii_uppercase();
        if !chars.all(|c| c.is_ascii_digit()) {
            return None;
        }
        match letter {
            'E' => Some(Nation::England),
            'W' => Some(Nation::Wales),
            'S' => Some(Nation::Scotland),
            'N' => Some(Nation::NorthernIreland),
            _ => None,
        }
    }

    /// Parse an operator-supplied nation name (`[analysis] nation = "scotland"`).
    ///
    /// Case-insensitive; hyphens, underscores and spaces are interchangeable.
    pub fn from_name(name: &str) -> Option<Nation> {
        let norm: String = name
            .trim()
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        match norm.as_str() {
            "england" => Some(Nation::England),
            "wales" => Some(Nation::Wales),
            "scotland" => Some(Nation::Scotland),
            "northernireland" | "ni" => Some(Nation::NorthernIreland),
            _ => None,
        }
    }
}

impl fmt::Display for Nation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Nation::England => "England",
            Nation::Wales => "Wales",
            Nation::Scotland => "Scotland",
            Nation::NorthernIreland => "Northern Ireland",
        };
        f.write_str(s)
    }
}

/// The set of nations a data source covers.
///
/// Declared per adapter so the engine can tell "this source has nothing to say about
/// this analysis" from "this source ran and found nothing" — the two are indistinguishable
/// in the logs otherwise, and one of them is a finding while the other is a coverage gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Jurisdiction(&'static [Nation]);

impl Jurisdiction {
    /// Covers all four nations — Companies House, The Gazette (all editions), the
    /// Electoral Commission.
    pub const UK_WIDE: Jurisdiction = Jurisdiction(Nation::ALL);

    /// HM Land Registry (CCOD, OCOD, Price Paid Data) and the Charity Commission for
    /// England and Wales. Scotland and NI have separate registries this engine does not
    /// read.
    pub const ENGLAND_AND_WALES: Jurisdiction = Jurisdiction(&[Nation::England, Nation::Wales]);

    /// Build a coverage set from a static list of nations.
    pub const fn new(nations: &'static [Nation]) -> Self {
        Jurisdiction(nations)
    }

    /// True when this source has data for `nation`.
    pub fn covers(&self, nation: Nation) -> bool {
        self.0.contains(&nation)
    }

    /// The nations covered, for operator output.
    pub fn nations(&self) -> &'static [Nation] {
        self.0
    }
}

impl fmt::Display for Jurisdiction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.len() == Nation::ALL.len() {
            return f.write_str("UK-wide");
        }
        let names: Vec<String> = self.0.iter().map(|n| n.to_string()).collect();
        f.write_str(&names.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ward_codes_map_to_their_nation() {
        assert_eq!(Nation::from_gss_code("E05001234"), Some(Nation::England));
        assert_eq!(Nation::from_gss_code("W05001492"), Some(Nation::Wales));
        assert_eq!(Nation::from_gss_code("S13002522"), Some(Nation::Scotland));
        assert_eq!(
            Nation::from_gss_code("N08000012"),
            Some(Nation::NorthernIreland)
        );
    }

    #[test]
    fn every_gss_level_carries_the_same_nation_letter() {
        // Local authority districts, not just wards — the rule is the first character.
        assert_eq!(Nation::from_gss_code("W06000001"), Some(Nation::Wales));
        assert_eq!(Nation::from_gss_code("S12000036"), Some(Nation::Scotland));
        assert_eq!(Nation::from_gss_code("E09000001"), Some(Nation::England));
    }

    #[test]
    fn malformed_codes_are_rejected_rather_than_guessed() {
        assert_eq!(Nation::from_gss_code(""), None);
        assert_eq!(Nation::from_gss_code("05001234"), None, "no nation letter");
        assert_eq!(Nation::from_gss_code("X05001234"), None, "unknown nation");
        assert_eq!(Nation::from_gss_code("W0500149"), None, "too short");
        assert_eq!(Nation::from_gss_code("W0500149A"), None, "not all digits");
        assert_eq!(
            Nation::from_gss_code("K05001234"),
            None,
            "K = UK-wide, not a nation"
        );
    }

    #[test]
    fn case_and_surrounding_whitespace_do_not_change_the_answer() {
        assert_eq!(Nation::from_gss_code(" w05001492 "), Some(Nation::Wales));
    }

    #[test]
    fn jurisdiction_covers_only_the_nations_it_declares() {
        assert!(Jurisdiction::ENGLAND_AND_WALES.covers(Nation::England));
        assert!(Jurisdiction::ENGLAND_AND_WALES.covers(Nation::Wales));
        assert!(!Jurisdiction::ENGLAND_AND_WALES.covers(Nation::Scotland));
        assert!(!Jurisdiction::ENGLAND_AND_WALES.covers(Nation::NorthernIreland));

        for n in Nation::ALL {
            assert!(Jurisdiction::UK_WIDE.covers(*n), "UK_WIDE must cover {n}");
        }
    }

    #[test]
    fn jurisdiction_describes_itself_for_the_operator() {
        assert_eq!(
            Jurisdiction::ENGLAND_AND_WALES.to_string(),
            "England, Wales"
        );
        assert_eq!(Jurisdiction::UK_WIDE.to_string(), "UK-wide");
        assert_eq!(Nation::Wales.to_string(), "Wales");
        assert_eq!(Nation::NorthernIreland.to_string(), "Northern Ireland");
    }

    #[test]
    fn nation_parses_from_an_operator_supplied_name() {
        assert_eq!(Nation::from_name("scotland"), Some(Nation::Scotland));
        assert_eq!(
            Nation::from_name("Northern Ireland"),
            Some(Nation::NorthernIreland)
        );
        assert_eq!(
            Nation::from_name("northern-ireland"),
            Some(Nation::NorthernIreland)
        );
        assert_eq!(Nation::from_name("Cymru"), None);
    }
}
