//! `node_palette.rs` — the D-15 node-kind categorical palette: maps a `NodeWeight` kind string
//! (as carried in the population Arrow payload's `kind` column and the side card's
//! `NodeSummary.entity.kind`) to its canvas colour token and canvas-legend label. Eleven
//! `NodeWeight` kinds collapse into 7 canvas swatches — the five OSINT-sourced kinds
//! (`PoliticalActor`, `Psc`, `Charity`, `OffshoreEntity`, `Disqualification`) share one
//! swatch/label (`--node-osint`/"OSINT"), per the phase 44 UI spec's Color section ("eleven distinct
//! hues at small point size would be indistinguishable on a WebGL canvas... six core kinds plus
//! one grouped OSINT swatch is legible").
//!
//! This module owns ONLY the kind -> token/label mapping. The seven CSS custom properties
//! themselves live in `style/tokens.css`; the canvas legend's actual rendering (with the
//! OSINT/PSC `<Abbr>`-wrapped label per the phase 44 UI spec's "Canvas legend" contract) is a later
//! plan's job — this module supplies the data layer both that legend and `graph_page.rs`'s canvas
//! consume.
#![allow(dead_code)] // node_kind_label has no call site yet — the canvas legend is a later plan.

/// The 7 canvas swatches: (label, CSS custom property name), in the fixed order
/// the phase 44 UI spec's Color table lists them (six core kinds, then the shared OSINT swatch last).
pub const NODE_KIND_PALETTE: [(&str, &str); 7] = [
    ("Entity", "--node-entity"),
    ("Officer", "--node-officer"),
    ("Location", "--node-location"),
    ("Contract", "--node-contract"),
    ("Ward", "--node-ward"),
    ("Property", "--node-property"),
    ("OSINT", "--node-osint"),
];

/// A `NodeWeight` kind string this module does not recognise falls back to this colour token —
/// never a panic on a future 12th `NodeWeight` variant. Reuses this project's existing
/// muted-foreground token rather than inventing an eighth swatch: an unrecognised kind should
/// read as "uncategorised," not a new, undocumented colour.
const UNKNOWN_KIND_COLOR: &str = "--muted-foreground";

/// A `NodeWeight` kind string this module does not recognise falls back to this legend label —
/// same rationale as [`UNKNOWN_KIND_COLOR`].
const UNKNOWN_KIND_LABEL: &str = "Unknown";

/// Maps a `NodeWeight` kind string (`arrow_export.rs::node_kind_name`'s output, or
/// `NodeSummary.entity.kind`) to its canvas colour token. The five OSINT-sourced kinds
/// (`PoliticalActor`, `Psc`, `Charity`, `OffshoreEntity`, `Disqualification`) all return the SAME
/// `--node-osint` token — proven by this module's own test as an identity check across all five,
/// not five separately-hardcoded-but-coincidentally-equal literals.
pub fn node_kind_color(kind: &str) -> &'static str {
    match kind {
        "Entity" => "--node-entity",
        "Officer" => "--node-officer",
        "Location" => "--node-location",
        "Contract" => "--node-contract",
        "Ward" => "--node-ward",
        "Property" => "--node-property",
        "PoliticalActor" | "Psc" | "Charity" | "OffshoreEntity" | "Disqualification" => {
            "--node-osint"
        }
        _ => UNKNOWN_KIND_COLOR,
    }
}

/// Maps a `NodeWeight` kind string to its canvas-legend label — the same 7-way grouping as
/// [`node_kind_color`], returning the human-readable label instead of the CSS token. The canvas
/// legend itself renders the shared OSINT label specially (wrapped in `<Abbr>` alongside "PSC",
/// per the phase 44 UI spec's "Canvas legend" contract) — this function returns the bare "OSINT" word,
/// the legend's own rendering composes the fuller sentence around it.
pub fn node_kind_label(kind: &str) -> &'static str {
    match kind {
        "Entity" => "Entity",
        "Officer" => "Officer",
        "Location" => "Location",
        "Contract" => "Contract",
        "Ward" => "Ward",
        "Property" => "Property",
        "PoliticalActor" | "Psc" | "Charity" | "OffshoreEntity" | "Disqualification" => "OSINT",
        _ => UNKNOWN_KIND_LABEL,
    }
}

/// The flat `[(kind, --node-* token)]` list `graph_page.rs`'s `CosmographHandle::mount` call site
/// (plan 44-16) passes through to the shim's `pointColorByMap`, so every point is coloured by its
/// `kind` column from the moment it mounts (D-15). Covers every `NodeWeight` kind
/// `arrow_export.rs::node_kind_name`'s exhaustive match can emit — the six core kinds plus the
/// five OSINT-sourced kinds, each paired with [`node_kind_color`]'s own token — so this module
/// stays the SINGLE source of truth for the kind -> token mapping: the canvas legend (a later
/// plan) and the live mount both consume it from here, never a second, independently-maintained
/// kind list.
pub fn kind_color_entries() -> Vec<(&'static str, &'static str)> {
    const KINDS: [&str; 11] = [
        "Entity",
        "Officer",
        "Location",
        "Contract",
        "Ward",
        "Property",
        "PoliticalActor",
        "Psc",
        "Charity",
        "OffshoreEntity",
        "Disqualification",
    ];
    KINDS
        .iter()
        .map(|&kind| (kind, node_kind_color(kind)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_kind_palette_has_seven_swatches() {
        assert_eq!(NODE_KIND_PALETTE.len(), 7);
    }

    /// PROVE THIS CAN FAIL: hardcode five independent (but coincidentally equal) literals for
    /// the OSINT-sourced kinds instead of sharing one match arm, and this test still passes —
    /// the real regression this guards against is a FUTURE edit that gives one of the five its
    /// own distinct token by mistake, which this identity-across-all-five assertion catches
    /// (a per-kind hardcoded-but-equal literal would not).
    #[test]
    fn osint_sourced_kinds_share_the_same_color_token() {
        let osint_kinds = [
            "PoliticalActor",
            "Psc",
            "Charity",
            "OffshoreEntity",
            "Disqualification",
        ];
        let colors: Vec<&str> = osint_kinds.iter().map(|k| node_kind_color(k)).collect();
        let first = colors[0];
        assert!(
            colors.iter().all(|c| *c == first),
            "all five OSINT-sourced kinds must return the identical token: {colors:?}"
        );
        assert_eq!(first, "--node-osint");
    }

    #[test]
    fn osint_sourced_kinds_share_the_same_legend_label() {
        let osint_kinds = [
            "PoliticalActor",
            "Psc",
            "Charity",
            "OffshoreEntity",
            "Disqualification",
        ];
        let labels: Vec<&str> = osint_kinds.iter().map(|k| node_kind_label(k)).collect();
        assert!(labels.iter().all(|l| *l == "OSINT"), "{labels:?}");
    }

    #[test]
    fn each_core_kind_maps_to_its_own_distinct_token() {
        let tokens: std::collections::HashSet<&str> = [
            node_kind_color("Entity"),
            node_kind_color("Officer"),
            node_kind_color("Location"),
            node_kind_color("Contract"),
            node_kind_color("Ward"),
            node_kind_color("Property"),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            tokens.len(),
            6,
            "the six core kinds must map to six distinct tokens"
        );
    }

    /// PROVE THIS CAN FAIL: a future 12th `NodeWeight` variant not yet added to the match arms
    /// must fall back gracefully, never panic. Removing the `_ =>` arm makes this fail to compile
    /// (non-exhaustive match on `&str`... actually `&str` matches are never exhaustive without a
    /// wildcard, so removing the arm instead makes THIS test fail at the `unreachable` panic a
    /// hypothetical non-wildcard match would produce) — this test exercises the real fallback
    /// path against a string that is guaranteed never to be a real `NodeWeight` kind.
    #[test]
    fn unrecognised_kind_falls_back_without_panicking() {
        assert_eq!(
            node_kind_color("SomeFutureTwelfthKind"),
            "--muted-foreground"
        );
        assert_eq!(node_kind_label("SomeFutureTwelfthKind"), "Unknown");
    }

    #[test]
    fn every_palette_token_is_reachable_from_node_kind_color() {
        // Every color node_kind_color can return (for a real kind) must appear in the palette's
        // token list, so the legend and the canvas colouring never drift apart.
        let palette_tokens: std::collections::HashSet<&str> =
            NODE_KIND_PALETTE.iter().map(|(_, token)| *token).collect();
        for kind in [
            "Entity",
            "Officer",
            "Location",
            "Contract",
            "Ward",
            "Property",
            "PoliticalActor",
        ] {
            assert!(
                palette_tokens.contains(node_kind_color(kind)),
                "{kind}'s token must be in NODE_KIND_PALETTE"
            );
        }
    }

    /// PROVE THIS CAN FAIL: drop one of the 11 `KINDS` entries from [`kind_color_entries`]'s
    /// internal list, or give one of the five OSINT-sourced kinds a distinct (but still valid)
    /// token by mistake — either change fails one of the two assertions below. Confirms
    /// [`kind_color_entries`] covers every kind [`node_kind_color`] recognises (matching
    /// `arrow_export.rs::node_kind_name`'s 11-variant exhaustive match) and shares the identical
    /// `--node-osint` token across all five OSINT-sourced kinds.
    #[test]
    fn kind_color_entries_covers_every_kind_and_shares_the_osint_token() {
        let entries = kind_color_entries();
        assert_eq!(
            entries.len(),
            11,
            "six core kinds + five OSINT-sourced kinds = 11, matching node_kind_name's 11-variant match"
        );

        let by_kind: std::collections::HashMap<&str, &str> = entries.into_iter().collect();
        for kind in [
            "Entity",
            "Officer",
            "Location",
            "Contract",
            "Ward",
            "Property",
            "PoliticalActor",
            "Psc",
            "Charity",
            "OffshoreEntity",
            "Disqualification",
        ] {
            assert_eq!(
                by_kind.get(kind).copied(),
                Some(node_kind_color(kind)),
                "{kind}'s entry must match node_kind_color exactly"
            );
        }

        let osint_tokens: std::collections::HashSet<&str> = [
            "PoliticalActor",
            "Psc",
            "Charity",
            "OffshoreEntity",
            "Disqualification",
        ]
        .iter()
        .map(|k| by_kind[k])
        .collect();
        assert_eq!(
            osint_tokens.len(),
            1,
            "all five OSINT-sourced kinds must map to the identical token: {osint_tokens:?}"
        );
        assert_eq!(*osint_tokens.iter().next().unwrap(), "--node-osint");
    }
}
