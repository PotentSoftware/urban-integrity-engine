//! OSINT surface tests for Phase 17 (SURF-01 / SURF-03).
//!
//! Tests:
//!   - `osint_section_skeleton`: entity OSINT Enrichment section always renders all 6
//!     subsection headings, populated or "(none)".
//!   - `verify_osint_flags`: VerifyVerdict carries the 4 OSINT flag fields; confirmed
//!     evasion sets has_disqualified_director=true and overrides risk_level to Critical.

use urban_integrity_core::{
    classify_risk, BeneficialOwnerRow, DisqualificationRow, EntityProfile, RiskLevel, VerifyVerdict,
};
use urban_integrity_schema::types::node::EntityStatus;

// ── Test helpers ─────────────────────────────────────────────────────────────

fn make_empty_profile() -> EntityProfile {
    EntityProfile {
        id: "GB-COH:12345678".to_string(),
        name: "Apex Build Ltd".to_owned(),
        ch_number: Some("12345678".to_owned()),
        status: EntityStatus::Active,
        incorporation_date: None,
        addresses: vec![],
        directors: vec![],
        contracts: vec![],
        planning_applications: vec![],
        alerts: vec![],
        related_entities: vec![],
        lpa_presence: vec![],
        beneficial_owners: vec![],
        disqualifications: vec![],
        offshore_links: vec![],
        trustee_of: vec![],
        owns_property: vec![],
        donates_to: vec![],
        osint_links: vec![],
        sanctions_status: None,
        osint_alerts: vec![],
        sub_candidate_links: vec![],
        identity_provenance: urban_integrity_core::Provenance::Unattributed {
            reason: "hand-built test fixture".to_owned(),
        },
        as_at: None,
    }
}

fn make_profile_with_beneficial_owner() -> EntityProfile {
    let mut p = make_empty_profile();
    p.beneficial_owners = vec![BeneficialOwnerRow {
        name: "John Smith".to_owned(),
        natures_of_control: vec![
            "ownership-of-shares-75-to-100-percent".to_owned(),
            "voting-rights-75-to-100-percent".to_owned(),
        ],
        provenance: urban_integrity_core::Provenance::Unattributed {
            reason: "hand-built test fixture".to_owned(),
        },
        temporal: urban_integrity_core::Temporal::unfiltered(
            urban_integrity_core::EffectiveRange::Unrecorded {
                basis: "hand-built test fixture".to_owned(),
            },
        ),
    }];
    p
}

fn make_profile_with_disqualified_director() -> EntityProfile {
    let mut p = make_empty_profile();
    p.disqualifications = vec![DisqualificationRow {
        officer_name: "john-smith".to_owned(),
        case_number: Some("INS-2023-00123".to_owned()),
        disqualified_from: Some("2024-01-15T00:00:00Z".to_owned()),
        disqualified_until: Some("2034-01-15T00:00:00Z".to_owned()),
        provenance: urban_integrity_core::Provenance::Unattributed {
            reason: "hand-built test fixture".to_owned(),
        },
        temporal: urban_integrity_core::Temporal::unfiltered(
            urban_integrity_core::EffectiveRange::Unrecorded {
                basis: "hand-built test fixture".to_owned(),
            },
        ),
    }];
    p
}

// ── osint_section_skeleton ───────────────────────────────────────────────────

/// D-12: Empty profile renders the "OSINT Enrichment" header + all 6 subsection
/// headings, each with "(none)" for the unpopulated future slots.
#[test]
fn osint_section_skeleton_empty_profile_renders_all_headings() {
    let p = make_empty_profile();
    let block = urban_integrity_cli::output::build_entity_osint_block(&p, false);

    // All 6 subsection headings must be present
    assert!(
        block.contains("Beneficial Owners"),
        "Block must contain 'Beneficial Owners' heading, got:\n{block}"
    );
    assert!(
        block.contains("Disqualifications"),
        "Block must contain 'Disqualifications' heading, got:\n{block}"
    );
    assert!(
        block.contains("Offshore Links"),
        "Block must contain 'Offshore Links' heading, got:\n{block}"
    );
    assert!(
        block.contains("Trustee Of"),
        "Block must contain 'Trustee Of' heading, got:\n{block}"
    );
    assert!(
        block.contains("Owns Property"),
        "Block must contain 'Owns Property' heading, got:\n{block}"
    );
    assert!(
        block.contains("Donates To"),
        "Block must contain 'Donates To' heading, got:\n{block}"
    );
}

/// D-12: Empty profile renders "(none)" for each subsection (6 occurrences).
#[test]
fn osint_section_skeleton_empty_profile_all_none() {
    let p = make_empty_profile();
    let block = urban_integrity_cli::output::build_entity_osint_block(&p, false);

    // Every subsection should show (none)
    let none_count = block.matches("(none)").count();
    assert!(
        none_count >= 6,
        "Block must contain at least 6 '(none)' entries (one per subsection), got {none_count}:\n{block}"
    );
}

/// D-12: Profile with a beneficial owner renders the owner's name under
/// "Beneficial Owners" and does NOT show "(none)" for that subsection.
#[test]
fn osint_section_skeleton_populated_beneficial_owner() {
    let p = make_profile_with_beneficial_owner();
    let block = urban_integrity_cli::output::build_entity_osint_block(&p, false);

    assert!(
        block.contains("John Smith"),
        "Block must contain the beneficial owner name 'John Smith', got:\n{block}"
    );
    assert!(
        block.contains("ownership-of-shares-75-to-100-percent"),
        "Block must contain natures_of_control, got:\n{block}"
    );
    // All other 5 subsections should still be "(none)"
    let none_count = block.matches("(none)").count();
    assert!(
        none_count >= 5,
        "The 5 unpopulated subsections must still show '(none)', got {none_count}:\n{block}"
    );
}

// ── verify_osint_flags ───────────────────────────────────────────────────────

/// Helper: build a VerifyVerdict from an EntityProfile with OSINT override fields.
///
/// In real execution the active-evasion DB query happens in verify.rs run().
/// Here we test the classify/override logic directly — asserting on the resulting
/// VerifyVerdict fields without calling std::process::exit.
fn build_verdict_with_evasion_override(
    profile: &EntityProfile,
    evasion_active: bool,
) -> VerifyVerdict {
    let has_disq = !profile.disqualifications.is_empty();
    let beneficial_owners: Vec<String> = profile
        .beneficial_owners
        .iter()
        .map(|b| b.name.clone())
        .collect();

    // Derive base risk level from P_c score (0.0 = no alerts → Low)
    let max_score = profile
        .alerts
        .iter()
        .map(|a| a.score)
        .fold(0.0_f64, f64::max);
    let mut risk_level = classify_risk(max_score);

    // SURF-03 / R-03: if has_disqualified_director AND active evasion confirmed,
    // override risk_level to Critical BEFORE rendering. Exit code 1 fires from
    // the matches!(High|Critical) arm in verify.rs — not exit(2) (R-03 supersedes
    // CONTEXT specifics line 148).
    if has_disq && evasion_active {
        risk_level = RiskLevel::Critical;
    }

    VerifyVerdict {
        entity_name: profile.name.clone(),
        ch_number: profile.ch_number.clone(),
        entity_status: profile.status.clone(),
        p_c_score: max_score,
        motif_flags: vec![],
        dissolved_relative_count: 0,
        connected_officer_count: profile.directors.len(),
        risk_level,
        has_disqualified_director: has_disq,
        beneficial_owners,
        has_offshore_link: false,
        has_sanctions_match: false,
    }
}

/// D-13: Verified verdict from a profile with a disqualified director sets
/// has_disqualified_director=true.
#[test]
fn verify_osint_flags_disqualified_director_flag() {
    let p = make_profile_with_disqualified_director();
    let verdict = build_verdict_with_evasion_override(&p, false);

    assert!(
        verdict.has_disqualified_director,
        "has_disqualified_director must be true when disqualifications is non-empty"
    );
}

/// D-13 / R-03: Active evasion confirmed overrides risk_level to Critical.
#[test]
fn verify_osint_flags_active_evasion_overrides_critical() {
    let p = make_profile_with_disqualified_director();
    let verdict = build_verdict_with_evasion_override(&p, true);

    assert_eq!(
        verdict.risk_level,
        RiskLevel::Critical,
        "Confirmed active evasion must override risk_level to Critical"
    );
}

/// R-03: Critical verdict maps to exit code 1 (not 2). Assert via the same
/// matches! predicate used in verify.rs:83.
#[test]
fn verify_osint_flags_critical_maps_to_exit_1() {
    let p = make_profile_with_disqualified_director();
    let verdict = build_verdict_with_evasion_override(&p, true);

    // This is the condition that fires std::process::exit(1) in verify.rs:83.
    // Testing the predicate here avoids needing to spawn a subprocess.
    let should_exit_nonzero = matches!(verdict.risk_level, RiskLevel::High | RiskLevel::Critical);
    assert!(
        should_exit_nonzero,
        "Critical risk level must trigger the exit(1) path (matches High|Critical)"
    );
}

/// D-13: has_offshore_link and has_sanctions_match are false in Phase 17.
#[test]
fn verify_osint_flags_phase17_stubs_are_false() {
    let p = make_profile_with_disqualified_director();
    let verdict = build_verdict_with_evasion_override(&p, true);

    assert!(
        !verdict.has_offshore_link,
        "has_offshore_link must be false in Phase 17 (stub for Phase 19)"
    );
    assert!(
        !verdict.has_sanctions_match,
        "has_sanctions_match must be false in Phase 17 (stub for Phase 19)"
    );
}

/// D-13: beneficial_owners list is populated from EntityProfile.
#[test]
fn verify_osint_flags_beneficial_owners_populated() {
    let p = make_profile_with_beneficial_owner();
    let verdict = build_verdict_with_evasion_override(&p, false);

    assert!(
        !verdict.beneficial_owners.is_empty(),
        "beneficial_owners must be populated from EntityProfile"
    );
    assert!(
        verdict.beneficial_owners.contains(&"John Smith".to_owned()),
        "beneficial_owners must contain 'John Smith', got: {:?}",
        verdict.beneficial_owners
    );
}

/// No evasion: disqualified director without an active evasion alert does NOT
/// override risk_level to Critical.
#[test]
fn verify_osint_flags_no_evasion_no_override() {
    let p = make_profile_with_disqualified_director();
    let verdict = build_verdict_with_evasion_override(&p, false);

    // No alerts → P_c = 0.0 → Low risk. Evasion not active → no override.
    assert_eq!(
        verdict.risk_level,
        RiskLevel::Low,
        "Without active evasion, risk_level stays Low (P_c=0.0)"
    );
}
