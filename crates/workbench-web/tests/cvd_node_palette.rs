//! D-08's carried-forward CVD validation pass (Phase 50, plan 08, UI-SPEC's "Action item carried
//! from the phase 50 design notes"): a colour-vision-deficiency simulation over the seven `--node-*`
//! categorical tokens, in both themes, host-side (`#[test]`, no wasm, no DOM — this workspace has
//! no headless-browser runner wired in, phase 42 research Pitfall 10).
//!
//! UI-SPEC names one pair as the thing to measure, not assume: `--node-location` (slate) vs.
//! `--node-entity` (indigo) — "the pair the star motif puts closest together" (the star's pinned
//! centre is always `--node-location`, its leaves are always `--node-entity`). This test checks
//! that pair specifically, in both themes, under all three common dichromacies.
//!
//! ## Simulation method
//!
//! sRGB hex -> linear RGB via the standard IEC 61966-2-1 transfer function, then a fixed
//! 3x3 matrix per deficiency type applied directly in linear RGB space. The matrices below are
//! the widely-used "HCIRN" (Vischeck-derived) colour-blindness simulation coefficients — the same
//! ones underlying Coblis (color-blindness.com) and the `colorblind`/`daltonize`-style JS
//! simulators — not a novel derivation. Distance is measured as the Euclidean distance between
//! the two simulated linear-RGB colours, never converted back to sRGB (monotonic under the
//! chosen matrices, so comparing in linear space is equivalent to comparing after a round trip).
//!
//! ## Threshold
//!
//! The maximum possible distance between two linear-RGB colours (pure black vs. pure white) is
//! `sqrt(3) ≈ 1.732`. This test's minimum-distance threshold is `0.15` — chosen as a conservative
//! fraction (~8.7%) of that maximum, well below every one of the six measured
//! `--node-location`/`--node-entity` distances (all ≥0.299, measured 2026-09-17; see the
//! `dark_and_light_theme_distances_are_printed_for_the_record` test's `eprintln!` output), so the
//! threshold is not reverse-fit to the data — there is roughly a 2x safety margin. This is a
//! coarser proxy than a perceptual metric like CIE76 `dE`, which cites ~2.3 as its own "just
//! noticeably different" threshold in Lab space; the linear-RGB distance here is deliberately set
//! low (permissive) rather than high, to avoid a false "collision" verdict from the proxy's own
//! imprecision while still catching a genuine near-duplicate colour pair (a `dist` near 0).
//!
//! If a real collision is ever found (a measured distance below 0.15), per UI-SPEC's own
//! instruction this test must NOT be loosened to pass — the finding belongs in a new
//! `50-CVD-FINDING.md`, and this test stays red until a follow-up decision is made.

const MIN_DISTANCE: f64 = 0.15;

/// A linear-RGB -> linear-RGB CVD simulation transform.
type SimulationFn = fn((f64, f64, f64)) -> (f64, f64, f64);

/// A `ThemeTokens` accessor for one `--node-*` kind's hex literal.
type ThemeTokenAccessor = fn(&ThemeTokens) -> &'static str;

/// sRGB [0,255] channel -> linear-light [0,1] channel (IEC 61966-2-1 / sRGB EOTF).
fn srgb_channel_to_linear(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Parses a `#rrggbb` hex literal into a linear-RGB triple. Panics on malformed input — every
/// caller here passes a hardcoded literal transcribed from `tokens.css`, so a parse failure is a
/// transcription bug in this file, not a runtime condition to handle gracefully.
fn hex_to_linear(hex: &str) -> (f64, f64, f64) {
    let hex = hex.trim_start_matches('#');
    assert_eq!(hex.len(), 6, "expected a 6-digit hex literal, got {hex:?}");
    let r = u8::from_str_radix(&hex[0..2], 16).expect("valid hex red channel");
    let g = u8::from_str_radix(&hex[2..4], 16).expect("valid hex green channel");
    let b = u8::from_str_radix(&hex[4..6], 16).expect("valid hex blue channel");
    (
        srgb_channel_to_linear(r),
        srgb_channel_to_linear(g),
        srgb_channel_to_linear(b),
    )
}

/// One CVD simulation transform: a fixed 3x3 matrix applied to a linear-RGB triple.
fn apply_matrix(m: [[f64; 3]; 3], rgb: (f64, f64, f64)) -> (f64, f64, f64) {
    let (r, g, b) = rgb;
    (
        m[0][0] * r + m[0][1] * g + m[0][2] * b,
        m[1][0] * r + m[1][1] * g + m[1][2] * b,
        m[2][0] * r + m[2][1] * g + m[2][2] * b,
    )
}

/// Protanopia (missing L-cones / red-deficient) simulation matrix, linear RGB.
fn protanopia(rgb: (f64, f64, f64)) -> (f64, f64, f64) {
    apply_matrix(
        [
            [0.567, 0.433, 0.0],
            [0.558, 0.442, 0.0],
            [0.0, 0.242, 0.758],
        ],
        rgb,
    )
}

/// Deuteranopia (missing M-cones / green-deficient, the most common form, ~5% of male readers)
/// simulation matrix, linear RGB.
fn deuteranopia(rgb: (f64, f64, f64)) -> (f64, f64, f64) {
    apply_matrix([[0.625, 0.375, 0.0], [0.7, 0.3, 0.0], [0.0, 0.3, 0.7]], rgb)
}

/// Tritanopia (missing S-cones / blue-deficient, rare) simulation matrix, linear RGB.
fn tritanopia(rgb: (f64, f64, f64)) -> (f64, f64, f64) {
    apply_matrix(
        [[0.95, 0.05, 0.0], [0.0, 0.433, 0.567], [0.0, 0.475, 0.525]],
        rgb,
    )
}

fn euclidean_distance(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
    let (ar, ag, ab) = a;
    let (br, bg, bb) = b;
    ((ar - br).powi(2) + (ag - bg).powi(2) + (ab - bb).powi(2)).sqrt()
}

/// One theme's transcribed literals for all 7 `--node-*` tokens. Transcribed from
/// `crates/workbench-web/style/tokens.css` (grep `--node-`) — a second assertion
/// (`transcribed_hex_literals_appear_verbatim_in_tokens_css`) proves each literal here actually
/// appears in that file, so the two cannot silently drift.
struct ThemeTokens {
    name: &'static str,
    entity: &'static str,
    officer: &'static str,
    location: &'static str,
    contract: &'static str,
    ward: &'static str,
    property: &'static str,
    osint: &'static str,
}

const DARK: ThemeTokens = ThemeTokens {
    name: "dark",
    entity: "#818cf8",
    officer: "#22d3ee",
    location: "#94a3b8",
    contract: "#fb923c",
    ward: "#2dd4bf",
    property: "#f472b6",
    osint: "#a3e635",
};

const LIGHT: ThemeTokens = ThemeTokens {
    name: "light",
    entity: "#4f46e5",
    officer: "#0e7490",
    location: "#64748b",
    contract: "#c2410c",
    ward: "#0f766e",
    property: "#be185d",
    osint: "#4d7c0f",
};

const TOKENS_CSS: &str = include_str!("../style/tokens.css");

#[test]
fn transcribed_hex_literals_appear_verbatim_in_tokens_css() {
    for theme in [&DARK, &LIGHT] {
        for (label, hex) in [
            ("entity", theme.entity),
            ("officer", theme.officer),
            ("location", theme.location),
            ("contract", theme.contract),
            ("ward", theme.ward),
            ("property", theme.property),
            ("osint", theme.osint),
        ] {
            assert!(
                TOKENS_CSS.contains(hex),
                "{} theme's transcribed --node-{label} literal {hex} does not appear in \
                 tokens.css — the transcription has drifted from the source of truth",
                theme.name
            );
        }
    }
}

/// The one pair UI-SPEC names — `--node-location` (the star's pinned centre) vs. `--node-entity`
/// (the star's leaves) — must simulate to a distance at or above [`MIN_DISTANCE`] under all three
/// dichromacies, in both themes.
#[test]
fn node_location_vs_node_entity_survives_all_three_dichromacies_in_both_themes() {
    for theme in [&DARK, &LIGHT] {
        let location = hex_to_linear(theme.location);
        let entity = hex_to_linear(theme.entity);

        let simulations: [(&str, SimulationFn); 3] = [
            ("deuteranopia", deuteranopia),
            ("protanopia", protanopia),
            ("tritanopia", tritanopia),
        ];
        for (sim_name, sim) in simulations {
            let dist = euclidean_distance(sim(location), sim(entity));
            assert!(
                dist >= MIN_DISTANCE,
                "{} theme: --node-location ({}) vs --node-entity ({}) collide under simulated \
                 {sim_name} — distance {dist:.4} is below the minimum {MIN_DISTANCE}. Per \
                 UI-SPEC's own instruction, do NOT loosen this threshold to make the test pass; \
                 write the finding into 50-CVD-FINDING.md instead and leave this test red.",
                theme.name,
                theme.location,
                theme.entity,
            );
        }
    }
}

/// Records the full 7x7-pair x 3-simulation x 2-theme measurement for the record (`--nocapture`
/// to see it) — not a pass/fail assertion beyond "did not panic", since UI-SPEC scopes the
/// collision check to the one named pair above, not every pair in the palette.
#[test]
fn dark_and_light_theme_distances_are_printed_for_the_record() {
    let all_kinds: [(&str, ThemeTokenAccessor); 7] = [
        ("entity", |t| t.entity),
        ("officer", |t| t.officer),
        ("location", |t| t.location),
        ("contract", |t| t.contract),
        ("ward", |t| t.ward),
        ("property", |t| t.property),
        ("osint", |t| t.osint),
    ];

    for theme in [&DARK, &LIGHT] {
        eprintln!("--- {} theme ---", theme.name);
        for i in 0..all_kinds.len() {
            for j in (i + 1)..all_kinds.len() {
                let (name_a, get_a) = all_kinds[i];
                let (name_b, get_b) = all_kinds[j];
                let a = hex_to_linear(get_a(theme));
                let b = hex_to_linear(get_b(theme));
                let d = euclidean_distance(deuteranopia(a), deuteranopia(b));
                let p = euclidean_distance(protanopia(a), protanopia(b));
                let t = euclidean_distance(tritanopia(a), tritanopia(b));
                eprintln!(
                    "{name_a}-{name_b}: deuteranopia={d:.4} protanopia={p:.4} tritanopia={t:.4}"
                );
            }
        }
    }
}

#[cfg(test)]
mod pure_fn_tests {
    use super::*;

    /// PROVE THIS CAN FAIL: identical colours must simulate to distance 0 under every
    /// transform — a bug in `apply_matrix`'s row/column indexing would not necessarily preserve
    /// this (e.g. a transposed matrix still maps identical inputs to identical outputs, so this
    /// alone does not catch every possible bug, but it does catch a broken/non-linear matrix
    /// application).
    #[test]
    fn identical_colours_have_zero_distance_under_every_simulation() {
        let c = hex_to_linear("#818cf8");
        for sim in [
            deuteranopia as fn((f64, f64, f64)) -> (f64, f64, f64),
            protanopia,
            tritanopia,
        ] {
            assert_eq!(euclidean_distance(sim(c), sim(c)), 0.0);
        }
    }

    #[test]
    fn black_and_white_round_trip_through_srgb_to_linear_at_the_extremes() {
        assert_eq!(hex_to_linear("#000000"), (0.0, 0.0, 0.0));
        let (r, g, b) = hex_to_linear("#ffffff");
        assert!((r - 1.0).abs() < 1e-9);
        assert!((g - 1.0).abs() < 1e-9);
        assert!((b - 1.0).abs() < 1e-9);
    }
}
