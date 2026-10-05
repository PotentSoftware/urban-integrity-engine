//! Source-coverage gate: an adapter must not run against a nation its source does not
//! cover (planning-notes Open Item 35; see the jurisdiction-gate spec).
//!
//! No DB needed — coverage is a static property of each adapter and the admission
//! decision is pure.

use std::path::PathBuf;

use urban_integrity_enricher::adapters::charity_commission::CharityCommissionAdapter;
use urban_integrity_enricher::adapters::land_registry_ccod::LandRegistryCcodAdapter;
use urban_integrity_enricher::adapters::land_registry_ocod::LandRegistryOcodAdapter;
use urban_integrity_enricher::adapters::land_registry_ppd::LandRegistryPpdAdapter;
use urban_integrity_enricher::adapters::planning_identity::PlanningIdentityAdapter;
use urban_integrity_enricher::adapters::EnricherAdapter;
use urban_integrity_enricher::engine::adapter_admitted;
use urban_integrity_schema::nation::{Jurisdiction, Nation};

fn ccod() -> LandRegistryCcodAdapter {
    LandRegistryCcodAdapter::new(Some("k".to_string()), PathBuf::from("data/ccod"), 30)
}

/// HM Land Registry covers England and Wales only. CCOD is the dangerous one: its row
/// filter is `company_number` alone, with no geography test, so a Scottish analysis would
/// receive real English property for its Scottish companies and score it as local
/// corroboration.
#[test]
fn land_registry_adapters_declare_england_and_wales() {
    assert_eq!(ccod().coverage(), Jurisdiction::ENGLAND_AND_WALES);
    assert_eq!(
        LandRegistryOcodAdapter::new(Some("k".to_string()), PathBuf::from("data/ocod"), 30)
            .coverage(),
        Jurisdiction::ENGLAND_AND_WALES
    );
    assert_eq!(
        LandRegistryPpdAdapter::new(Default::default(), PathBuf::from("data/ppd")).coverage(),
        Jurisdiction::ENGLAND_AND_WALES
    );
}

/// The Charity Commission adapter's own first line says "for England and Wales". Scotland
/// is OSCR and NI is CCNI; neither is implemented.
#[test]
fn charity_commission_declares_england_and_wales() {
    assert_eq!(
        CharityCommissionAdapter::new(PathBuf::from("data/charity"), 30, true).coverage(),
        Jurisdiction::ENGLAND_AND_WALES
    );
}

/// Adapters that never opted in stay UK-wide, so adding the gate cannot silently narrow
/// a source that was fine.
#[test]
fn adapters_default_to_uk_wide() {
    assert_eq!(
        PlanningIdentityAdapter::new().coverage(),
        Jurisdiction::UK_WIDE
    );
}

#[test]
fn out_of_jurisdiction_adapters_are_refused_admission() {
    for nation in [Nation::Scotland, Nation::NorthernIreland] {
        assert!(
            !adapter_admitted(Jurisdiction::ENGLAND_AND_WALES, Some(nation)),
            "an England-and-Wales source must not run against {nation}"
        );
    }
    for nation in [Nation::England, Nation::Wales] {
        assert!(adapter_admitted(
            Jurisdiction::ENGLAND_AND_WALES,
            Some(nation)
        ));
    }
    for nation in Nation::ALL {
        assert!(adapter_admitted(Jurisdiction::UK_WIDE, Some(*nation)));
    }
}

/// An analysis with no wards configured has no nation to check against. Refusing every
/// regional source there would break existing single-source runs that never had a
/// geography, so an unknown nation admits everything — the preflight is what tells the
/// operator the nation is unknown.
#[test]
fn unknown_analysis_nation_admits_every_adapter() {
    assert!(adapter_admitted(Jurisdiction::ENGLAND_AND_WALES, None));
    assert!(adapter_admitted(Jurisdiction::UK_WIDE, None));
}
