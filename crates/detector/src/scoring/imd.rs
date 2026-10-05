use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ImdRow {
    #[serde(rename = "Ward Code")]
    ward_code: String,
    #[serde(rename = "IMD Rank")]
    imd_rank: u32,
    /// The size of the index this rank was drawn from — 1,909 for WIMD 2019's Welsh
    /// LSOAs, 32,844 for English IMD 2019's. Optional so an existing whole-index file
    /// still loads, but see [`ImdIndex::load`] for why a subset needs it.
    #[serde(rename = "IMD Denominator")]
    denominator: Option<u32>,
}

pub struct ImdIndex {
    /// The denominator `m_env` is normalised against.
    pub max_rank: u32,
    pub ranks: HashMap<String, u32>,
    /// True when the file declared `IMD Denominator`. When false, `max_rank` was
    /// inferred from the largest rank present, which is only correct if the file is
    /// the WHOLE index — see [`ImdIndex::load`].
    pub denominator_declared: bool,
}

impl Default for ImdIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl ImdIndex {
    pub fn new() -> Self {
        Self {
            max_rank: 1, // Will be updated on load
            ranks: HashMap::new(),
            denominator_declared: false,
        }
    }

    /// Load ward ranks from CSV.
    ///
    /// **The denominator is the load-bearing part.** `m_env` is
    /// `1 + (max_rank - rank) / max_rank`, so `max_rank` sets the entire scale. It used
    /// to be `max()` of whatever rows the file happened to contain, which is correct
    /// only for a file holding the whole index. Any regional extract silently rescaled
    /// itself: an index of the 14 Anglesey wards, whose ranks live in Wales' 1..1909
    /// space, would normalise against its own worst ward and give the least-deprived
    /// ward `m_env = 1.0` exactly — asserting it is the least deprived place in Wales.
    ///
    /// So an `IMD Denominator` column, when present, wins. Two guards come with it:
    /// a rank above the denominator is rejected (it would make the numerator negative
    /// and DE-weight the alert below 1.0), and rows disagreeing about the denominator
    /// are rejected (a file splicing England's 32,844-LSOA ranks onto Wales' 1,909 has
    /// no common scale, and averaging across them is meaningless).
    ///
    /// Without the column the old inference is kept, because a whole-index file is the
    /// normal case — but `denominator_declared` records that it was inferred so the
    /// caller can say so rather than presenting a subset-relative scale as deprivation
    /// weighting.
    pub fn load<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Box<dyn std::error::Error>> {
        let file = File::open(path)?;
        let mut rdr = csv::Reader::from_reader(file);

        let mut max_rank = 1;
        let mut declared: Option<u32> = None;

        for result in rdr.deserialize() {
            let row: ImdRow = result?;

            if let Some(d) = row.denominator {
                match declared {
                    Some(prev) if prev != d => {
                        return Err(format!(
                            "IMD Denominator disagrees between rows ({prev} then {d}) — the file                              splices two different indices, whose ranks share no scale"
                        )
                        .into());
                    }
                    _ => declared = Some(d),
                }
                if row.imd_rank > d {
                    return Err(format!(
                        "ward {} has rank {} above the declared denominator {d} — the two columns                          describe different indices, and the resulting m_env would fall below 1.0",
                        row.ward_code, row.imd_rank
                    )
                    .into());
                }
            }

            if row.imd_rank > max_rank {
                max_rank = row.imd_rank;
            }
            self.ranks.insert(row.ward_code, row.imd_rank);
        }

        self.denominator_declared = declared.is_some();
        self.max_rank = declared.unwrap_or(max_rank);
        Ok(())
    }

    /// Whether this ward has a rank at all.
    ///
    /// [`Self::deprivation`] returns 0.0 for an unknown ward, which is numerically
    /// identical to the least deprived ward in the index. Without this, "no data for
    /// this ward" is indistinguishable from "this ward is the least deprived", and the
    /// second is a claim the data never made.
    /// The UK nation this index covers, from its ward codes.
    ///
    /// `None` when the index is empty, or when its wards span more than one nation —
    /// which is already a broken file, because ranks drawn from two of the four indices
    /// share no scale (see [`ImdIndex::load`]). The caller refuses rather than picking
    /// the majority: a wrong answer here silently weights an analysis against another
    /// country's deprivation.
    pub fn nation(&self) -> Option<urban_integrity_schema::nation::Nation> {
        let mut found: Option<urban_integrity_schema::nation::Nation> = None;
        for ward in self.ranks.keys() {
            let nation = urban_integrity_schema::nation::Nation::from_gss_code(ward)?;
            match found {
                None => found = Some(nation),
                Some(prev) if prev == nation => {}
                Some(_) => return None,
            }
        }
        found
    }

    pub fn covers(&self, ward_code: &str) -> bool {
        self.ranks.contains_key(ward_code)
    }

    /// Deprivation of a ward on `[0.0, 1.0]`, 1.0 being the most deprived in the index.
    ///
    /// 0.0 for an unknown ward, which is the same value as the LEAST deprived ward — so
    /// callers must gate on [`Self::covers`] rather than reading 0.0 as a finding. See
    /// `formula::apply_imd` for what this feeds.
    pub fn deprivation(&self, ward_code: &str) -> f64 {
        match self.ranks.get(ward_code) {
            Some(&rank) => super::formula::compute_deprivation(rank, self.max_rank),
            None => 0.0, // No pressure applied
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;
    use urban_integrity_schema::nation::Nation;

    #[test]
    fn test_imd_load_and_deprivation() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "Ward Code,IMD Rank").unwrap();
        writeln!(file, "W01,1").unwrap(); // Most deprived
        writeln!(file, "W02,50").unwrap();
        writeln!(file, "W03,100").unwrap(); // Least deprived

        let mut index = ImdIndex::new();
        index.load(file.path()).unwrap();

        assert_eq!(index.max_rank, 100);
        assert_eq!(index.deprivation("W03"), 0.0); // Least deprived
        assert!((index.deprivation("W01") - 0.99).abs() < 1e-9); // Most deprived

        // Unknown ward applies no pressure — numerically the same as least deprived, which
        // is why `covers` exists.
        assert_eq!(index.deprivation("UNKNOWN"), 0.0);
    }

    // ── the denominator (planning notes: IMD weighting inert) ──────────────────────

    /// **The defect this fixes.** `max_rank` was `max()` of whatever rows the file
    /// happened to contain, so a REGIONAL extract silently rescaled every weight to
    /// that region. An index covering the 14 Anglesey wards, whose ranks live in
    /// Wales' 1..1909 space, would have been normalised against its own worst ward —
    /// making Anglesey's least-deprived ward `m_env = 1.0` exactly, as if it were the
    /// least deprived place in Wales. A declared denominator fixes the scale to the
    /// index's own, so a subset weights the same as the whole.
    #[test]
    fn a_declared_denominator_beats_the_max_row() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "Ward Code,IMD Rank,IMD Denominator").unwrap();
        writeln!(file, "W05001497,300,1909").unwrap();
        writeln!(file, "W05001501,1500,1909").unwrap();

        let mut index = ImdIndex::new();
        index.load(file.path()).unwrap();

        assert_eq!(
            index.max_rank, 1909,
            "the file's own denominator, not its max row"
        );
        assert!(index.denominator_declared);
        // (1909 - 1500)/1909, NOT the 0.0 the max-row rule would have produced.
        let d = index.deprivation("W05001501");
        assert!(
            d > 0.21 && d < 0.22,
            "least-deprived ward in the subset still weights: {d}"
        );
    }

    /// Without a denominator the old behaviour is kept — a whole-index file is the
    /// normal case — but the index says so, so a caller can warn instead of
    /// presenting a subset-relative scale as deprivation weighting.
    #[test]
    fn an_undeclared_denominator_falls_back_and_is_flagged() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "Ward Code,IMD Rank").unwrap();
        writeln!(file, "W01,1").unwrap();
        writeln!(file, "W02,100").unwrap();

        let mut index = ImdIndex::new();
        index.load(file.path()).unwrap();

        assert_eq!(index.max_rank, 100);
        assert!(!index.denominator_declared);
    }

    /// A rank above the denominator means the two columns describe different indices
    /// — England's 32,844 LSOAs against Wales' 1,909, say. Loading it would produce a
    /// negative numerator and an `m_env` below 1.0, quietly DE-weighting an alert.
    #[test]
    fn a_rank_beyond_the_denominator_is_rejected() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "Ward Code,IMD Rank,IMD Denominator").unwrap();
        writeln!(file, "W01,5000,1909").unwrap();

        let mut index = ImdIndex::new();
        assert!(index.load(file.path()).is_err());
    }

    /// Two rows disagreeing about the denominator means the file is a splice of two
    /// indices, which cannot be ranked against each other.
    #[test]
    fn an_inconsistent_denominator_is_rejected() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "Ward Code,IMD Rank,IMD Denominator").unwrap();
        writeln!(file, "W01,10,1909").unwrap();
        writeln!(file, "E01,10,32844").unwrap();

        let mut index = ImdIndex::new();
        assert!(index.load(file.path()).is_err());
    }

    /// The generator writes provenance columns (LSOA count, spread) next to the two
    /// the loader needs. Extra columns must not break the parse.
    #[test]
    fn extra_columns_are_ignored() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            "Ward Code,Ward Name,IMD Rank,IMD Denominator,LSOA Count"
        )
        .unwrap();
        writeln!(file, "W05001497,Cefni,300,1909,3").unwrap();

        let mut index = ImdIndex::new();
        index.load(file.path()).unwrap();
        assert_eq!(index.ranks.get("W05001497"), Some(&300));
    }

    /// An unknown ward scores 1.0, which is numerically identical to the least
    /// deprived ward in the index. The two must stay distinguishable to a caller, or
    /// "no data for this ward" reads as "this ward is fine".
    #[test]
    fn an_unknown_ward_is_distinguishable_from_the_least_deprived() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "Ward Code,IMD Rank,IMD Denominator").unwrap();
        writeln!(file, "W01,100,100").unwrap();

        let mut index = ImdIndex::new();
        index.load(file.path()).unwrap();

        assert_eq!(index.deprivation("W01"), 0.0);
        assert_eq!(index.deprivation("UNKNOWN"), 0.0);
        assert!(index.covers("W01"));
        assert!(!index.covers("UNKNOWN"));
    }

    /// The index's own nation, read off its ward codes. The preflight compares this to
    /// the analysis nation: the English IMD file loads perfectly cleanly against Welsh
    /// wards and then matches none of them, which is the defect this exists to catch.
    #[test]
    fn index_reports_the_nation_of_its_wards() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "Ward Code,IMD Rank,IMD Denominator").unwrap();
        writeln!(f, "W05001492,1573,1909").unwrap();
        writeln!(f, "W05001503,295,1909").unwrap();
        let mut idx = ImdIndex::new();
        idx.load(f.path()).unwrap();
        assert_eq!(idx.nation(), Some(Nation::Wales));
    }

    #[test]
    fn an_empty_index_has_no_nation() {
        assert_eq!(ImdIndex::new().nation(), None);
    }

    /// Ranks from two nations share no scale, so a file spanning them has no nation to
    /// report — the preflight refuses rather than picking the majority.
    #[test]
    fn an_index_spanning_nations_reports_no_nation() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "Ward Code,IMD Rank,IMD Denominator").unwrap();
        writeln!(f, "W05001492,1573,1909").unwrap();
        writeln!(f, "E05001234,900,1909").unwrap();
        let mut idx = ImdIndex::new();
        idx.load(f.path()).unwrap();
        assert_eq!(idx.nation(), None);
    }
}
