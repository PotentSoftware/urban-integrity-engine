pub fn compute_p_base(s_phoenix: f64, s_proximity: f64, s_clustering: f64) -> f64 {
    (s_phoenix * 0.50) + (s_proximity * 0.30) + (s_clustering * 0.20)
}

/// How much of an alert's remaining headroom deprivation may claim.
///
/// **The form matters more than the constant, and the first two attempts got the form
/// wrong** (planning open item 34).
///
/// The original weighting multiplied the base score by `m_env ∈ [1.0, 2.0]` and clamped at
/// 1.0. That saturates every base score above `1/m_env`, and the motifs that actually fire
/// score far higher than the 0.5 that implies: `AddressCluster` produces 0.75–0.95.
/// Measured live on `an_anglesey_aebaa785`, enabling a real index pushed **all 1,231
/// weighted alerts onto exactly 1.0** while the 548 with no ward data kept 0.9 — inverting
/// the ordering the weighting exists to sharpen.
///
/// Narrowing that band to `[1.0, 1.3]` does **not** fix it. The ceiling then binds above
/// `1/1.3 ≈ 0.77`, and every live base score (0.85, 0.90, 0.95) is above it, so the
/// strongest clusters in the most deprived wards still clamp. Any UPWARD multiplier
/// saturates when base scores already sit near the ceiling; the width only changes how many.
///
/// So deprivation claims a fraction of the HEADROOM instead:
///
/// ```text
/// p_final = p_base + (1 - p_base) * IMD_WEIGHT * deprivation
/// ```
///
/// This cannot exceed 1.0 by construction, so nothing clamps and no distinction is lost. It
/// is still an uplift — deprivation only ever raises a score, matching "environmental
/// pressure adds risk" — and it is a genuine tie-break: at 0.3, an alert scoring 0.85 in the
/// most deprived ward in Wales reaches 0.888, still below an alert scoring 0.95 in the least
/// deprived. Deprivation reorders alerts of similar strength without overturning a
/// materially stronger motif.
///
/// Widening this is a scoring-policy change: it moves every alert's score and therefore the
/// T-Digest percentile thresholds derived from them.
pub const IMD_WEIGHT: f64 = 0.3;

/// Deprivation of an area on `[0.0, 1.0]`, where 1.0 is the most deprived.
///
/// `imd_rank` is 1-based and 1 is the MOST deprived area — the convention all four UK
/// indices share (England IMD 2019, Wales WIMD 2019, Scotland SIMD 2020, NI NIMDM 2017).
/// `max_rank` is the size of that index, which differs per nation and is why a file must
/// declare it: 32,844 / 1,909 / 6,976 / 890 respectively. Ranks from different nations are
/// not comparable and must never share an index file.
pub fn compute_deprivation(imd_rank: u32, max_rank: u32) -> f64 {
    if max_rank == 0 {
        return 0.0;
    }
    let rank = imd_rank.min(max_rank) as f64;
    let max = max_rank as f64;
    ((max - rank) / max).clamp(0.0, 1.0)
}

/// Apply deprivation weighting to a base score. Never exceeds 1.0, so never clamps.
pub fn apply_imd(p_base: f64, deprivation: f64) -> f64 {
    let p_base = p_base.clamp(0.0, 1.0);
    let deprivation = deprivation.clamp(0.0, 1.0);
    p_base + (1.0 - p_base) * IMD_WEIGHT * deprivation
}

/// The effective multiplier deprivation applied, for the alert's `score_breakdown`.
///
/// Reported rather than used: it is `p_final / p_base` after the fact, so the payload says
/// what actually happened to THIS alert instead of a nominal figure that the clamp may have
/// discarded. 1.0 means deprivation changed nothing.
pub fn effective_m_env(p_base: f64, p_final: f64) -> f64 {
    if p_base <= 0.0 {
        return 1.0;
    }
    p_final / p_base
}

pub fn compute_p_final(p_base: f64, m_env: f64) -> f64 {
    (p_base * m_env).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_p_base() {
        assert_eq!(compute_p_base(1.0, 1.0, 1.0), 1.0);
        assert_eq!(compute_p_base(0.0, 0.0, 0.0), 0.0);
        assert_eq!(compute_p_base(1.0, 0.0, 0.0), 0.50);
        assert_eq!(compute_p_base(0.0, 1.0, 0.0), 0.30);
        assert_eq!(compute_p_base(0.0, 0.0, 1.0), 0.20);
    }

    #[test]
    fn test_compute_deprivation() {
        assert_eq!(compute_deprivation(100, 100), 0.0); // Least deprived
        assert!((compute_deprivation(1, 100) - 0.99).abs() < 1e-9); // Most deprived
        assert_eq!(compute_deprivation(50, 100), 0.5); // Median
        assert_eq!(compute_deprivation(0, 0), 0.0); // Degenerate index, no weighting
    }

    /// **The property the whole design exists for** (planning open item 34).
    ///
    /// Deprivation must reorder alerts of similar strength without overturning a materially
    /// stronger motif, and must never saturate — the failure that made the first two
    /// attempts unusable. Pinned against the real Anglesey ward ranks and the real live base
    /// scores, so a future change to `IMD_WEIGHT` has to break this deliberately.
    #[test]
    fn imd_weighting_is_a_tiebreak_not_a_saturator() {
        let most_deprived = compute_deprivation(295, 1909); // Tref Cybi, Holyhead
        let least_deprived = compute_deprivation(1573, 1909); // Aethwy

        // Nothing saturates, at any live base score, in any ward.
        for base in [0.75, 0.85, 0.90, 0.95] {
            let weighted = apply_imd(base, most_deprived);
            assert!(
                weighted < 1.0,
                "base {base} saturated in the most deprived ward: {weighted}"
            );
            assert!(
                weighted > base,
                "deprivation must raise the score, not lower it"
            );
        }

        // A more deprived ward outranks a less deprived one at equal base score …
        assert!(apply_imd(0.85, most_deprived) > apply_imd(0.85, least_deprived));

        // … but never overturns a materially stronger motif. This is the tie-break.
        assert!(
            apply_imd(0.85, most_deprived) < apply_imd(0.95, least_deprived),
            "deprivation must not outrank a materially stronger motif"
        );

        // An alert with no ward data keeps its base score and still sorts on merit, rather
        // than being stranded below a field of saturated alerts.
        assert!(apply_imd(0.95, 0.0) > apply_imd(0.85, most_deprived));

        // Deprivation has to move the score enough to matter at all.
        let spread = apply_imd(0.85, most_deprived) - apply_imd(0.85, least_deprived);
        assert!(
            spread > 0.02,
            "deprivation moved the score by only {spread}"
        );
    }

    /// The two rejected forms, kept as a live comparison so the choice of shape is not
    /// mistaken for an arbitrary constant.
    #[test]
    fn both_multiplicative_bands_would_saturate() {
        // Original: m_env in [1.0, 2.0].
        let wide = 1.0 + compute_deprivation(295, 1909);
        // First proposed narrowing: m_env in [1.0, 1.3]. The ceiling still binds above
        // 1/1.3 = 0.77, and every live base score is above that.
        let narrow = 1.0 + IMD_WEIGHT * compute_deprivation(295, 1909);

        for base in [0.85, 0.90, 0.95] {
            assert_eq!(
                compute_p_final(base, wide),
                1.0,
                "wide band saturates {base}"
            );
            assert_eq!(
                compute_p_final(base, narrow),
                1.0,
                "narrow band also saturates {base}"
            );
        }
        // Whereas the shipped headroom form does not.
        assert!(apply_imd(0.95, compute_deprivation(295, 1909)) < 1.0);
    }

    #[test]
    fn effective_m_env_reports_what_actually_happened() {
        let d = compute_deprivation(295, 1909);
        let p_final = apply_imd(0.85, d);
        let m = effective_m_env(0.85, p_final);
        assert!((0.85 * m - p_final).abs() < 1e-12);
        assert_eq!(
            effective_m_env(0.85, 0.85),
            1.0,
            "no deprivation, no multiplier"
        );
    }

    #[test]
    fn test_compute_p_final() {
        assert_eq!(compute_p_final(0.5, 2.0), 1.0);
        assert_eq!(compute_p_final(0.6, 1.9), 1.0); // bounded to 1.0
        assert_eq!(compute_p_final(0.1, 1.0), 0.1);
        assert_eq!(compute_p_final(0.0, 2.0), 0.0);
    }
}
