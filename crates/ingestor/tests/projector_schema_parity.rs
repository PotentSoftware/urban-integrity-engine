//! Parity guard: `SCHEMA_DEF`'s node tables vs `projector::node_data`'s `json!` literals.
//!
//! `node_data` builds each projected node row as a `serde_json::json!` literal, and the node
//! tables are `SCHEMAFULL`. That combination fails silently in both directions:
//!
//! * A field defined in `SCHEMA_DEF` but absent from the literal is simply never written. No
//!   error, no warning — the column just stays empty forever. This is what happened to
//!   `contract.application_received_date`: 30,836 `graph_events` carried the value while the
//!   projected `contract` table held 0, and it cost two false root causes in Phase 29 before
//!   anyone looked at the mapper.
//! * A key in the literal with no matching `DEFINE FIELD` is dropped by `SCHEMAFULL` on write.
//!   The mapper believes it wrote the field; nothing downstream ever sees it.
//!
//! Neither direction produces a failure anyone can observe at runtime, so this test makes them
//! observable at CI time instead. `node_data`'s own `Officer` arm already carries a comment
//! warning about exactly this hazard — a comment is not a mechanism.
//!
//! Scope: the 11 node tables reachable from `NodeWeight`. Relation (edge) tables go through a
//! different raw-SQL path and are not covered here.
//!
//! Generalises `projector.rs`'s `every_schema_defined_contract_field_is_projected`, which was
//! added in response to the `application_received_date` loss but guards only the `contract`
//! table and only the schema-defined-but-unmapped direction. That test is kept: it asserts
//! against a fully-populated `ContractData`, where this one uses serde defaults.
//!
//! NOT covered: a field that exists on the weight struct but in neither `SCHEMA_DEF` nor the
//! mapper — e.g. `PropertyData::country_incorporated`, which no `property` column models at all.
//! That is a schema-completeness question, not a parity one; parity holds trivially when both
//! sides are equally empty.

use std::collections::{BTreeMap, BTreeSet};

use urban_integrity_ingestor::projector::{node_data, table_for_weight};
use urban_integrity_schema::db::SCHEMA_DEF;
use urban_integrity_schema::types::node::NodeWeight;

/// Every `NodeWeight` variant, by name. Kept as data so a variant that gains no sample is a
/// test failure rather than a silent gap in coverage.
const ALL_VARIANTS: &[&str] = &[
    "Entity",
    "Officer",
    "Location",
    "Contract",
    "Ward",
    "PoliticalActor",
    "Psc",
    "Charity",
    "Property",
    "OffshoreEntity",
    "Disqualification",
];

/// Exhaustive match with no wildcard arm: adding a variant to `NodeWeight` fails to COMPILE
/// here until it is named, which in turn forces it into `ALL_VARIANTS` and `sample_weights`.
fn variant_name(weight: &NodeWeight) -> &'static str {
    match weight {
        NodeWeight::Entity(_) => "Entity",
        NodeWeight::Officer(_) => "Officer",
        NodeWeight::Location(_) => "Location",
        NodeWeight::Contract(_) => "Contract",
        NodeWeight::Ward(_) => "Ward",
        NodeWeight::PoliticalActor(_) => "PoliticalActor",
        NodeWeight::Psc(_) => "Psc",
        NodeWeight::Charity(_) => "Charity",
        NodeWeight::Property(_) => "Property",
        NodeWeight::OffshoreEntity(_) => "OffshoreEntity",
        NodeWeight::Disqualification(_) => "Disqualification",
    }
}

/// One sample per variant, built through the same internally-tagged serde path the projector
/// reads `weight_json` with. Only `name` is required on the variants that have it; every other
/// field is `#[serde(default)]`.
///
/// Field VALUES are irrelevant to this test: `json!` emits a key for every entry regardless,
/// so the emitted key set does not depend on whether an `Option` is `Some` or `None`.
fn sample_weights() -> Vec<NodeWeight> {
    let named = |ty: &str| {
        serde_json::from_value::<NodeWeight>(serde_json::json!({ "type": ty, "name": "sample" }))
            .unwrap_or_else(|e| panic!("could not build a {ty} sample: {e}"))
    };
    let bare = |ty: &str| {
        serde_json::from_value::<NodeWeight>(serde_json::json!({ "type": ty }))
            .unwrap_or_else(|e| panic!("could not build a {ty} sample: {e}"))
    };

    vec![
        named("Entity"),
        named("Officer"),
        named("Location"),
        named("Contract"),
        named("Ward"),
        named("PoliticalActor"),
        named("Psc"),
        named("Charity"),
        bare("Property"),
        named("OffshoreEntity"),
        bare("Disqualification"),
    ]
}

/// `SCHEMA_DEF` parsed into `table -> {field}`.
///
/// Only `DEFINE FIELD` statements are read; `DEFINE INDEX` also contains " ON " and would
/// otherwise be misparsed as a field. The field name is the token immediately before `ON`,
/// which is robust to the optional `IF NOT EXISTS`; the table is the token after `ON`, or
/// after `ON TABLE` where that form is used.
fn schema_field_map() -> BTreeMap<String, BTreeSet<String>> {
    let mut map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for line in SCHEMA_DEF.lines().map(str::trim) {
        if !line.starts_with("DEFINE FIELD") {
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let Some(on) = tokens.iter().position(|t| *t == "ON") else {
            continue;
        };
        if on == 0 || on + 1 >= tokens.len() {
            continue;
        }
        let field = tokens[on - 1];
        let table = if tokens[on + 1] == "TABLE" {
            tokens.get(on + 2).copied().unwrap_or_default()
        } else {
            tokens[on + 1]
        };
        let table = table.trim_end_matches(';');
        if table.is_empty() {
            continue;
        }
        map.entry(table.to_string())
            .or_default()
            .insert(field.to_string());
    }

    map
}

/// Guards the parser itself. Without this, a `schema_field_map` that silently returned nothing
/// would make every parity assertion below pass vacuously — the same shape of false-green this
/// whole file exists to prevent.
#[test]
fn schema_parser_finds_known_fields() {
    let map = schema_field_map();

    let entity = map
        .get("entity")
        .expect("SCHEMA_DEF defines an `entity` table — parser found no fields for it");
    assert!(
        entity.contains("entity_status") && entity.contains("incorporation_date"),
        "parser missed known `entity` fields; got {entity:?}"
    );

    // Proves DEFINE INDEX lines are not being read as fields: `entity` has two indexes, on
    // `company_number` and `entity_id`, and neither index name may appear as a field.
    assert!(
        !entity.iter().any(|f| f.starts_with("idx_")),
        "DEFINE INDEX lines leaked into the field set: {entity:?}"
    );
}

#[test]
fn every_node_weight_variant_has_a_sample() {
    let samples = sample_weights();
    let covered: BTreeSet<&str> = samples.iter().map(variant_name).collect();
    let expected: BTreeSet<&str> = ALL_VARIANTS.iter().copied().collect();

    assert_eq!(
        covered, expected,
        "sample_weights() must cover every NodeWeight variant exactly once"
    );
    assert_eq!(
        samples.len(),
        ALL_VARIANTS.len(),
        "duplicate samples for the same variant"
    );
}

/// The parity assertion itself.
#[test]
fn schema_def_and_node_data_agree_on_every_node_table() {
    let schema = schema_field_map();
    let mut failures: Vec<String> = Vec::new();

    for weight in sample_weights() {
        let table = table_for_weight(&weight);
        let variant = variant_name(&weight);

        let Some(schema_fields) = schema.get(table) else {
            failures.push(format!(
                "{variant}: table `{table}` has no DEFINE FIELD statements in SCHEMA_DEF at all"
            ));
            continue;
        };
        assert!(
            !schema_fields.is_empty(),
            "{variant}: table `{table}` parsed to an empty field set"
        );

        let row = node_data("sample:1", &weight);
        let mapped: BTreeSet<String> = row
            .as_object()
            .unwrap_or_else(|| panic!("{variant}: node_data did not return a JSON object"))
            .keys()
            .cloned()
            .collect();

        // Written by the projector as the row's own identifier; not a SCHEMA_DEF column on
        // every table, so it is excluded from the comparison rather than special-cased below.
        let dropped: Vec<&String> = schema_fields.difference(&mapped).collect();
        let rejected: Vec<&String> = mapped.difference(schema_fields).collect();

        if !dropped.is_empty() {
            failures.push(format!(
                "{variant} -> `{table}`: SCHEMA_DEF defines {dropped:?} but node_data never \
                 emits them — these columns stay empty forever, silently"
            ));
        }
        if !rejected.is_empty() {
            failures.push(format!(
                "{variant} -> `{table}`: node_data emits {rejected:?} with no matching DEFINE \
                 FIELD — SCHEMAFULL discards them on write, silently"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "SCHEMA_DEF / node_data parity broken:\n  {}",
        failures.join("\n  ")
    );
}
