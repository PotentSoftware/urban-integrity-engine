//! Integration test stubs for Markdown report export.
//!
//! Requirements covered:
//! - REP-01: Full-section Markdown report (executive summary, alerts, entities, citations)
//! - REP-02: Output file written to disk with correct default filename format
//! - REP-04: Citations section with CH hyperlink, tender_ref, application_ref
//! - REP-05: Entity-scoped report resolved by company name
//!
//! Tests that require pure rendering are now active.
//! Tests that require SurrealDB or full CLI invocation remain `#[ignore]` and are
//! documented as manual-only per the phase 12 validation notes.

#![allow(dead_code, unused_imports)]

use urban_integrity_cli::commands::report::{
    render_citations_md, render_markdown_report, IntegrityReport, ReportFilters,
};
use urban_integrity_core::{ContractRow, EntityProfile};
use urban_integrity_schema::types::node::EntityStatus;

fn make_empty_profile(name: &str, ch_number: Option<&str>) -> EntityProfile {
    EntityProfile {
        id: name.to_string(),
        name: name.to_owned(),
        ch_number: ch_number.map(str::to_owned),
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

fn make_report_empty() -> IntegrityReport {
    IntegrityReport {
        generated_at: "2026-05-23T10:00:00Z".to_owned(),
        filters_applied: ReportFilters {
            ward: Some("W05001492".to_owned()),
            min_score: 0.5,
            motif: None,
            from: None,
            to: None,
        },
        total_alerts: 0,
        high_risk_count: 0,
        alerts: vec![],
    }
}

/// REP-01: Markdown report renders all required sections.
#[test]
fn rep_01_render_markdown_full_sections() {
    let report = make_report_empty();
    let profiles: Vec<EntityProfile> = vec![];
    let md = render_markdown_report(&report, &profiles, None);

    assert!(
        md.contains("# UrbanIntegrity Report"),
        "Section 1 header missing:\n{md}"
    );
    assert!(
        md.contains("## Summary"),
        "Section 3 summary missing:\n{md}"
    );
    assert!(md.contains("## Alerts"), "Section 4 alerts missing:\n{md}");
    assert!(
        md.contains("## Top Entities"),
        "Section 5 top entities missing:\n{md}"
    );
    assert!(
        md.contains("## Source Data"),
        "Section 6 source data missing:\n{md}"
    );
    assert!(
        !md.contains("## Summary Narrative"),
        "Summary Narrative must NOT appear when summary_narrative is None:\n{md}"
    );
}

/// REP-02: --output-file flag writes the Markdown report to the specified path.
#[test]
#[ignore = "Requires filesystem write — covered by Task 3 human-verify checkpoint smoke test"]
fn rep_02_output_file_writes_to_disk() {
    // TODO: implement as CLI subprocess test if assert_cmd is added in future
    // Manual verification: ./target/release/urban-integrity report --format md --ward W05001492
    // See the phase 12 validation notes for full smoke-test protocol.
}

/// REP-02: Default filename format matches `integrity-report-<slug>-<YYYY-MM-DD>.md`.
/// This is tested in-module in report.rs (default_filename_* tests).
#[test]
#[ignore = "Covered by in-module unit tests in commands/report.rs"]
fn rep_02_default_filename_format() {
    // Covered by report::tests::default_filename_ward_scope,
    // default_filename_entity_scope, default_filename_all_scope
}

/// REP-04: Citations section renders a Companies House hyperlink for each contract.
#[test]
fn rep_04_citations_section_ch_hyperlink() {
    let profile = make_empty_profile("Apex Build Ltd", Some("12345678"));
    let result = render_citations_md(&[profile]);
    assert!(
        result.contains(
            "[Companies House](https://find-and-update.company-information.service.gov.uk/company/12345678)"
        ),
        "CH hyperlink missing in citations, got:\n{result}"
    );
}

/// REP-04: Citations section includes tender_ref when present.
#[test]
fn rep_04_citations_tender_ref_present() {
    let mut profile = make_empty_profile("CF Company Ltd", Some("55555555"));
    profile.contracts.push(ContractRow {
        id: "CF:CF-TEST-001".to_owned(),
        name: "Test Contract".to_owned(),
        value_gbp: None,
        status: None,
        published: None,
        ward_code: None,
        tender_ref: Some("CF-TEST-001".to_owned()),
        application_ref: None,
        provenance: urban_integrity_core::Provenance::Unattributed {
            reason: "hand-built test fixture".to_owned(),
        },
        temporal: urban_integrity_core::Temporal::unfiltered(
            urban_integrity_core::EffectiveRange::Unrecorded {
                basis: "hand-built test fixture".to_owned(),
            },
        ),
    });
    let result = render_citations_md(&[profile]);
    assert!(
        result.contains("Contract ref: CF-TEST-001"),
        "tender_ref not found in citations, got:\n{result}"
    );
}

/// REP-04: Citations section includes application_ref when present.
#[test]
fn rep_04_citations_application_ref_present() {
    let mut profile = make_empty_profile("Planning Applicant Ltd", Some("66666666"));
    profile.contracts.push(ContractRow {
        id: "planning:FUL/2024/001234".to_owned(),
        name: "Planning App".to_owned(),
        value_gbp: None,
        status: None,
        published: None,
        ward_code: None,
        tender_ref: None,
        application_ref: Some("FUL/2024/001234".to_owned()),
        provenance: urban_integrity_core::Provenance::Unattributed {
            reason: "hand-built test fixture".to_owned(),
        },
        temporal: urban_integrity_core::Temporal::unfiltered(
            urban_integrity_core::EffectiveRange::Unrecorded {
                basis: "hand-built test fixture".to_owned(),
            },
        ),
    });
    let result = render_citations_md(&[profile]);
    assert!(
        result.contains("Planning application: FUL/2024/001234"),
        "application_ref not found in citations, got:\n{result}"
    );
}

/// REP-05: Entity-scoped report resolves company by name and assembles across all wards.
#[test]
#[ignore = "Requires SurrealDB — covered by Task 3 human-verify checkpoint smoke test"]
fn rep_05_entity_scoped_report_resolves_by_name() {
    // TODO: run against a live DB or use assert_cmd for full CLI invocation
    // Manual verification: ./target/release/urban-integrity report --format md --entity "Apex Build Ltd" --output-file /tmp/test-entity.md
    // See the phase 12 validation notes for full smoke-test protocol.
}
