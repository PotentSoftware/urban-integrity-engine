//! `edge_palette.rs` — the edge-type categorical palette: maps an `EdgeWeight` type string (as
//! carried in the population Arrow payload's `edge_type` column) to its canvas colour token and
//! canvas-legend family. The twelve `EdgeWeight` variants collapse into 6 canvas swatches, for the
//! same reason `node_palette.rs` collapses eleven kinds into seven: twelve distinct hues on
//! one-pixel-wide WebGL lines would be indistinguishable, and an edge carries less colour area
//! than a point does.
//!
//! Cosmograph cannot render text on edges — the vendored bundle has no `linkLabel*` config key of
//! any kind — so colour plus this legend IS the edge labelling. The grouping is therefore by what
//! a relationship MEANS to an analyst, and the legend names each family's member types verbatim
//! so a swatch can always be traced back to the tool rail's own edge-type checkboxes.
//!
//! This module owns ONLY the edge-type -> token mapping. The six CSS custom properties live
//! in `style/tokens.css`; the legend's rendering lives in `tool_rail.rs::canvas_legend`.

/// The 6 canvas swatches: (family label, CSS custom property name, member `EdgeWeight` type
/// names). Ordered by how central the family is to a procurement-corruption read: control first,
/// then the award chain, then the risk signals, then place, money and property.
pub const EDGE_TYPE_PALETTE: [(&str, &str, &[&str]); 6] = [
    (
        "Control",
        "--edge-control",
        &["Directs", "Controls", "TrusteeOf"],
    ),
    ("Award", "--edge-award", &["AwardedTo", "IssuedBy"]),
    ("Risk", "--edge-risk", &["OffshoreLink", "DisqualifiedFrom"]),
    ("Place", "--edge-place", &["LocatedAt", "Represents"]),
    ("Money", "--edge-money", &["DonatesTo", "DeclaresInterest"]),
    ("Property", "--edge-property", &["OwnsProperty"]),
];

/// An `EdgeWeight` type string this module does not recognise falls back to this colour token —
/// never a panic on a future 13th `EdgeWeight` variant. Same rationale, and the same token, as
/// `node_palette.rs`'s unknown-kind fallback: an unrecognised type should read as
/// "uncategorised", not as a new, undocumented colour.
const UNKNOWN_EDGE_COLOR: &str = "--muted-foreground";

/// Maps an `EdgeWeight` type string (`csr_view.rs::edge_type_name`'s output, as written into the
/// links stream's `edge_type` column by `arrow_export.rs`) to its canvas colour token.
pub fn edge_type_color(edge_type: &str) -> &'static str {
    match edge_type {
        "Directs" | "Controls" | "TrusteeOf" => "--edge-control",
        "AwardedTo" | "IssuedBy" => "--edge-award",
        "OffshoreLink" | "DisqualifiedFrom" => "--edge-risk",
        "LocatedAt" | "Represents" => "--edge-place",
        "DonatesTo" | "DeclaresInterest" => "--edge-money",
        "OwnsProperty" => "--edge-property",
        _ => UNKNOWN_EDGE_COLOR,
    }
}

/// Every `EdgeWeight` type name `csr_view.rs::edge_type_name`'s exhaustive 12-variant match can
/// emit. Kept as one list so [`edge_color_entries`] and this module's tests both derive from it
/// rather than re-typing the set.
pub const ALL_EDGE_TYPES: [&str; 12] = [
    "Directs",
    "LocatedAt",
    "AwardedTo",
    "IssuedBy",
    "Represents",
    "Controls",
    "TrusteeOf",
    "OwnsProperty",
    "OffshoreLink",
    "DonatesTo",
    "DisqualifiedFrom",
    "DeclaresInterest",
];

/// The flat `[(edge_type, --edge-* token)]` list `graph_page.rs`'s `CosmographHandle::mount` call
/// site passes through to the shim, which resolves each token to a concrete colour at mount time
/// and wires it into `linkColorBy: "edge_type"` + a `linkColorByFn` lookup. Covers all twelve
/// types, so this module stays the SINGLE source of truth: the canvas legend and the live mount
/// both consume it from here, never a second, independently-maintained edge-type list.
pub fn edge_color_entries() -> Vec<(&'static str, &'static str)> {
    ALL_EDGE_TYPES
        .iter()
        .map(|&edge_type| (edge_type, edge_type_color(edge_type)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_type_palette_has_six_swatches() {
        assert_eq!(EDGE_TYPE_PALETTE.len(), 6);
    }

    /// Every one of the twelve types must be claimed by exactly one palette family, and the
    /// colour `edge_type_color` hands the canvas must be that family's own token. A type added to
    /// `edge_type_color` but forgotten in `EDGE_TYPE_PALETTE`'s member lists (or vice versa)
    /// fails here — that drift is exactly what would put a colour on the canvas with no swatch in
    /// the legend explaining it.
    #[test]
    fn every_edge_type_belongs_to_exactly_one_palette_family() {
        for edge_type in ALL_EDGE_TYPES {
            let claiming: Vec<&str> = EDGE_TYPE_PALETTE
                .iter()
                .filter(|(_, _, members)| members.contains(&edge_type))
                .map(|(label, _, _)| *label)
                .collect();
            assert_eq!(
                claiming.len(),
                1,
                "{edge_type} must be claimed by exactly one family, got {claiming:?}"
            );
            assert_eq!(
                edge_type_color(edge_type),
                EDGE_TYPE_PALETTE
                    .iter()
                    .find(|(label, _, _)| *label == claiming[0])
                    .map(|(_, token, _)| *token)
                    .unwrap(),
                "{edge_type}'s colour must be the token of the family claiming it"
            );
        }
    }

    /// PROVE THIS CAN FAIL: give two families the same token and this fails — six families that
    /// render as five colours would make the legend claim a distinction the canvas does not draw.
    #[test]
    fn each_family_maps_to_its_own_distinct_token() {
        let tokens: std::collections::HashSet<&str> = EDGE_TYPE_PALETTE
            .iter()
            .map(|(_, token, _)| *token)
            .collect();
        assert_eq!(
            tokens.len(),
            6,
            "the six families must map to six distinct tokens"
        );
    }

    #[test]
    fn members_of_a_family_share_its_token() {
        for (label, token, members) in EDGE_TYPE_PALETTE {
            for member in members {
                assert_eq!(
                    edge_type_color(member),
                    token,
                    "{member} is listed under {label} so it must carry {token}"
                );
            }
        }
    }

    #[test]
    fn unrecognised_edge_type_falls_back_without_panicking() {
        assert_eq!(
            edge_type_color("SomeFutureThirteenthEdge"),
            "--muted-foreground"
        );
    }

    /// PROVE THIS CAN FAIL: drop an entry from [`ALL_EDGE_TYPES`] and the count assertion fails;
    /// mis-map one and the per-type assertion fails.
    #[test]
    fn edge_color_entries_covers_every_type() {
        let entries = edge_color_entries();
        assert_eq!(
            entries.len(),
            12,
            "must match edge_type_name's 12-variant exhaustive match"
        );
        let by_type: std::collections::HashMap<&str, &str> = entries.into_iter().collect();
        for edge_type in ALL_EDGE_TYPES {
            assert_eq!(
                by_type.get(edge_type).copied(),
                Some(edge_type_color(edge_type)),
                "{edge_type}'s entry must match edge_type_color exactly"
            );
        }
        assert!(
            !by_type.values().any(|t| *t == UNKNOWN_EDGE_COLOR),
            "no real edge type may fall through to the unknown-token fallback"
        );
    }
}
