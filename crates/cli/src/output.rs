use comfy_table::{Cell, Color, Table};
use serde::Serialize;
use urban_integrity_core::{
    AddressRow, AlertRecord, BeneficialOwnerRow, ContractRow, DirectorRow, DisqualificationRow,
    EntityProfile, EntityProfileSummary, LpaPresence, PlanningApplicationRow, RelatedEntity,
};
use urban_integrity_schema::types::node::EntityStatus;

#[derive(Clone, Debug, clap::ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
    #[value(name = "md")]
    Markdown,
    Pdf,
}

impl OutputFormat {
    /// Auto-select format: JSON when stdout is not a terminal (piped), table otherwise.
    pub fn auto() -> Self {
        if console::Term::stdout().is_term() {
            OutputFormat::Table
        } else {
            OutputFormat::Json
        }
    }
}

pub fn print_json<T: Serialize>(data: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(data)?);
    Ok(())
}

pub fn severity_color(score: f64) -> Color {
    if score >= 0.85 {
        Color::Red
    } else if score >= 0.65 {
        Color::Yellow
    } else {
        Color::Green
    }
}

pub fn build_alert_table(alerts: &[crate::commands::alerts::AlertRow]) -> Table {
    let mut table = Table::new();
    table.set_header(vec!["ID", "Motif", "Score", "Ward", "Detected", "Entities"]);
    for a in alerts {
        let color = severity_color(a.score);
        table.add_row(vec![
            Cell::new(&a.id),
            Cell::new(&a.motif_type),
            Cell::new(format!("{:.3}", a.score)).fg(color),
            Cell::new(a.ward.as_deref().unwrap_or("\u{2014}")),
            Cell::new(&a.detected_at),
            Cell::new(a.entities.as_deref().unwrap_or("\u{2014}")),
        ]);
    }
    table
}

// ── Entity status helper ─────────────────────────────────────────────────────

fn format_entity_status(s: &EntityStatus) -> String {
    match s {
        EntityStatus::Active => "Active".to_owned(),
        EntityStatus::Dissolved(dt) => format!("Dissolved {}", dt.format("%Y-%m-%d")),
        EntityStatus::InLiquidation(dt) => format!("In Liquidation {}", dt.format("%Y-%m-%d")),
    }
}

// ── Entity profile renderers ─────────────────────────────────────────────────

/// Build the multi-line header block for an entity profile.
///
/// AC-DET-1: if an incorporation date is present and any contract was published
/// within 12 months of it, appends a flag line showing months-since-incorporation
/// for the earliest contract.
pub fn build_entity_header_block(p: &EntityProfile) -> String {
    let em = "\u{2014}";
    let ch = p.ch_number.as_deref().unwrap_or(em);
    let status = format_entity_status(&p.status);
    let inc = p
        .incorporation_date
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| em.to_owned());

    let mut block = format!(
        "{}\nCH Number:    {}\nStatus:       {}\nIncorporated: {}\n",
        p.name, ch, status, inc
    );

    // AC-DET-1: show "first contract N months after incorporation" when relevant
    if let Some(inc_date) = p.incorporation_date {
        // Find the earliest published date across all awarded work.
        //
        // D-03 split planning applications out of `contracts`, and this signal deliberately spans
        // BOTH sections. It is a detection heuristic — "this company started winning work
        // immediately after it was incorporated" — not a contracts figure, so narrowing it to the
        // contracts section would silently retire the flag for 86.9% of the `AwardedTo`
        // destinations on the live corpus. On today's corpus no subject holds both kinds (every
        // `planning:` destination is reached from a `director:` applicant slug and every `CF:` one
        // from a `GB-COH:`/`CF-PARTY:` supplier, measured 2026-09-15), so this union changes
        // nothing observable today — but that is a property of the corpus, not of this code, and
        // it stops being true the moment identity resolution joins an applicant to a company.
        let earliest: Option<chrono::NaiveDate> = p
            .contracts
            .iter()
            .filter_map(|c| c.published.as_deref())
            .chain(
                p.planning_applications
                    .iter()
                    .filter_map(|a| a.published.as_deref()),
            )
            .filter_map(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            .min();

        if let Some(pub_date) = earliest {
            // Calendar-accurate month arithmetic avoids the 390-day (13×30) false-positive
            // that integer division by 30 produces at the 12-month boundary.
            use chrono::Datelike as _;
            let months_since = (pub_date.year() - inc_date.year()) * 12
                + (pub_date.month() as i32 - inc_date.month() as i32);
            if (0..=12).contains(&months_since) {
                block.push_str(&format!(
                    "  \u{2691} First contract: {} months after incorporation\n",
                    months_since
                ));
            }
        }
    }

    block
}

/// Directors sub-table. Columns: Name | Role | Appointed | Resigned.
/// Em-dash for None values.
pub fn build_entity_directors_table(directors: &[DirectorRow]) -> Table {
    let em = "\u{2014}";
    let mut table = Table::new();
    table.set_header(vec!["Name", "Role", "Appointed", "Resigned"]);
    for d in directors {
        table.add_row(vec![
            Cell::new(&d.name),
            Cell::new(d.role.as_deref().unwrap_or(em)),
            Cell::new(d.appointed.as_deref().unwrap_or(em)),
            Cell::new(d.resigned.as_deref().unwrap_or(em)),
        ]);
    }
    table
}

/// Contracts sub-table. Columns: Title | Value (GBP) | Status | Published | Ward.
/// Value formatted as "£1,500,000" or em-dash.
pub fn build_entity_contracts_table(contracts: &[ContractRow]) -> Table {
    let em = "\u{2014}";
    let mut table = Table::new();
    table.set_header(vec!["Title", "Value (GBP)", "Status", "Published", "Ward"]);
    for c in contracts {
        let value_str = c
            .value_gbp
            .map(|v| format!("\u{a3}{}", format_gbp(v)))
            .unwrap_or_else(|| em.to_owned());
        table.add_row(vec![
            Cell::new(&c.name),
            Cell::new(value_str),
            Cell::new(c.status.as_deref().unwrap_or(em)),
            Cell::new(c.published.as_deref().unwrap_or(em)),
            Cell::new(c.ward_code.as_deref().unwrap_or(em)),
        ]);
    }
    table
}

/// Planning applications sub-table. Columns: Description | Status | Received | Ward | Ref.
///
/// Its own table rather than more rows in the contracts one (D-03 / PIV-02). It carries no value
/// column because a planning application has no value: of the 102,619 `planning:` `Contract`
/// node-weight events on the live corpus, zero serialise a `value_gbp` (measured 2026-09-15). A
/// "£—" column on every row would read as data this project has not fetched rather than as a
/// figure that does not exist.
pub fn build_entity_planning_table(applications: &[PlanningApplicationRow]) -> Table {
    let em = "\u{2014}";
    let mut table = Table::new();
    table.set_header(vec!["Description", "Status", "Received", "Ward", "Ref"]);
    for a in applications {
        table.add_row(vec![
            Cell::new(&a.name),
            Cell::new(a.status.as_deref().unwrap_or(em)),
            Cell::new(a.published.as_deref().unwrap_or(em)),
            Cell::new(a.ward_code.as_deref().unwrap_or(em)),
            Cell::new(a.application_ref.as_deref().unwrap_or(em)),
        ]);
    }
    table
}

/// Addresses sub-table. Columns: Postcode | Address Line 1 | Locality.
pub fn build_entity_addresses_table(addresses: &[AddressRow]) -> Table {
    let em = "\u{2014}";
    let mut table = Table::new();
    table.set_header(vec!["Postcode", "Address Line 1", "Locality"]);
    for a in addresses {
        table.add_row(vec![
            Cell::new(a.postcode.as_deref().unwrap_or(em)),
            Cell::new(a.address_line_1.as_deref().unwrap_or(em)),
            Cell::new(a.locality.as_deref().unwrap_or(em)),
        ]);
    }
    table
}

/// Active alerts sub-table. Columns: Motif | Score | Ward | Detected.
/// Score cell uses severity_color. ID column omitted (analyst knows which entity).
pub fn build_entity_alerts_table(alerts: &[AlertRecord]) -> Table {
    let em = "\u{2014}";
    let mut table = Table::new();
    table.set_header(vec!["Motif", "Score", "Ward", "Detected"]);
    for a in alerts {
        let color = severity_color(a.score);
        table.add_row(vec![
            Cell::new(&a.motif_type),
            Cell::new(format!("{:.3}", a.score)).fg(color),
            Cell::new(a.ward_code.as_deref().unwrap_or(em)),
            Cell::new(&a.detected_at),
        ]);
    }
    table
}

/// Related entities sub-table. Columns: Company | CH Number | Status | Shared Connection.
/// Shared Connection cell shows "Director: {name}" or "Address: {postcode}".
pub fn build_entity_related_table(rows: &[RelatedEntity]) -> Table {
    let em = "\u{2014}";
    let mut table = Table::new();
    table.set_header(vec!["Company", "CH Number", "Status", "Shared Connection"]);
    for r in rows {
        let shared = match r.shared_connection.kind.as_str() {
            "director" => format!("Director: {}", r.shared_connection.value),
            "address" => format!("Address: {}", r.shared_connection.value),
            other => format!("{}: {}", other, r.shared_connection.value),
        };
        table.add_row(vec![
            Cell::new(&r.name),
            Cell::new(r.ch_number.as_deref().unwrap_or(em)),
            Cell::new(format_entity_status(&r.status)),
            Cell::new(shared),
        ]);
    }
    table
}

/// LPA presence sub-table. Columns: LPA Code | Contracts | Planning Apps.
///
/// Renders the per-LPA activity summary returned by `query_entity_lpa_presence`.
/// Em-dash convention is not needed here — all numeric fields are always present.
pub fn build_entity_lpa_presence_table(rows: &[LpaPresence]) -> Table {
    let mut table = Table::new();
    table.set_header(vec!["LPA Code", "Contracts", "Planning Apps"]);
    for r in rows {
        table.add_row(vec![
            Cell::new(&r.lpa_code),
            Cell::new(r.contract_count),
            Cell::new(r.planning_application_count),
        ]);
    }
    table
}

/// Disambiguation table (D-05). Columns: # | Name | CH Number | Status.
/// Row index is 1-based.
pub fn build_entity_disambiguation_table(rows: &[EntityProfileSummary]) -> Table {
    let em = "\u{2014}";
    let mut table = Table::new();
    table.set_header(vec!["#", "Name", "CH Number", "Status"]);
    for (i, r) in rows.iter().enumerate() {
        table.add_row(vec![
            Cell::new(i + 1),
            Cell::new(&r.name),
            Cell::new(r.ch_number.as_deref().unwrap_or(em)),
            Cell::new(format_entity_status(&r.status)),
        ]);
    }
    table
}

/// OSINT Enrichment block for the `entity` command (SURF-01 / D-12).
///
/// Prints all 6 named subsections in order. In Phase 17, only Beneficial Owners
/// (Controls edges) and Disqualifications (DisqualifiedFrom edges) are populated
/// from live data. The remaining 4 subsections render their heading + "(none)"
/// so future phases can fill them without restructuring the section layout.
///
/// ENR-11: only auto_linked/confirmed links are shown — candidate links are never
/// surfaced to the analyst (enforced at the query seam in queries.rs).
///
/// The `deep` flag controls whether the "Sub-Candidate OSINT Links" section
/// is rendered. D-13: sub_candidate links are NOT shown in standard entity output;
/// only when `--deep` is explicitly passed by the analyst. D-14: these links are
/// read-only and cannot be confirmed via `links` CLI.
pub fn build_entity_osint_block(p: &EntityProfile, deep: bool) -> String {
    let mut out = String::new();

    // ── 1. Beneficial Owners (Controls / Psc nodes) ──────────────────────────
    out.push_str("Beneficial Owners (Controls)\n");
    if p.beneficial_owners.is_empty() {
        out.push_str("(none)\n");
    } else {
        let block = build_beneficial_owners_table(&p.beneficial_owners);
        out.push_str(&block.to_string());
        out.push('\n');
    }

    // ── 2. Disqualifications (DisqualifiedFrom edges) ────────────────────────
    out.push('\n');
    out.push_str("Disqualifications (DisqualifiedFrom)\n");
    if p.disqualifications.is_empty() {
        out.push_str("(none)\n");
    } else {
        let block = build_disqualifications_table(&p.disqualifications);
        out.push_str(&block.to_string());
        out.push('\n');
    }

    // ── 3–6. Link subsections ────────────────────────────────────────────────
    // All four families gained a row type when PIV-03 gave them a provenance (plan 45-04):
    // `PropertyRow` for the property section and `LinkedRecordRow` for the other three. The bullet
    // text is each row's `label`, byte-identical to the `Vec<String>` element it replaced — this
    // terminal view renders no provenance chip, which is the phase 45 UI spec's surface, not the CLI's.
    fn push_link_section(out: &mut String, heading: &str, items: &mut dyn Iterator<Item = &str>) {
        out.push('\n');
        out.push_str(heading);
        out.push('\n');
        let mut any = false;
        for item in items {
            any = true;
            out.push_str("  • ");
            out.push_str(item);
            out.push('\n');
        }
        if !any {
            out.push_str("(none)\n");
        }
    }
    push_link_section(
        &mut out,
        "Offshore Links",
        &mut p.offshore_links.iter().map(|r| r.label.as_str()),
    );
    push_link_section(
        &mut out,
        "Trustee Of",
        &mut p.trustee_of.iter().map(|r| r.label.as_str()),
    );
    push_link_section(
        &mut out,
        "Owns Property",
        &mut p.owns_property.iter().map(|r| r.label.as_str()),
    );
    push_link_section(
        &mut out,
        "Donates To",
        &mut p.donates_to.iter().map(|r| r.label.as_str()),
    );

    // ── 7. Sub-candidate links (SURF-02) — only when --deep is requested ─────
    // D-13: sub_candidate links are NOT shown in standard entity output.
    // D-14: these links are read-only — analysts cannot confirm via `links` CLI.
    if deep {
        out.push('\n');
        out.push_str("Sub-Candidate OSINT Links (0.50–0.69, investigative only)\n");
        out.push_str("Note: these links cannot be confirmed via `links` CLI\n");
        if p.sub_candidate_links.is_empty() {
            out.push_str("(none)\n");
        } else {
            let mut table = Table::new();
            table.set_header(vec![
                "Source Node",
                "Target Node",
                "Adapter",
                "Confidence",
                "Type",
            ]);
            for link in &p.sub_candidate_links {
                table.add_row(vec![
                    Cell::new(&link.source_node_id),
                    Cell::new(&link.target_node_id),
                    Cell::new(&link.source),
                    Cell::new(format!("{:.2}", link.confidence)),
                    Cell::new(&link.resolution_type),
                ]);
            }
            out.push_str(&table.to_string());
            out.push('\n');
        }
    }

    out
}

/// Beneficial owners table. Columns: Name | Natures of Control.
fn build_beneficial_owners_table(rows: &[BeneficialOwnerRow]) -> Table {
    let mut table = Table::new();
    table.set_header(vec!["Name", "Natures of Control"]);
    for r in rows {
        let noc = if r.natures_of_control.is_empty() {
            "\u{2014}".to_owned()
        } else {
            r.natures_of_control.join(", ")
        };
        table.add_row(vec![Cell::new(&r.name), Cell::new(noc)]);
    }
    table
}

/// Disqualifications table. Columns: Officer | Case Number | From | Until.
fn build_disqualifications_table(rows: &[DisqualificationRow]) -> Table {
    let em = "\u{2014}";
    let mut table = Table::new();
    table.set_header(vec!["Officer", "Case Number", "From", "Until"]);
    for r in rows {
        table.add_row(vec![
            Cell::new(&r.officer_name),
            Cell::new(r.case_number.as_deref().unwrap_or(em)),
            Cell::new(r.disqualified_from.as_deref().unwrap_or(em)),
            Cell::new(r.disqualified_until.as_deref().unwrap_or(em)),
        ]);
    }
    table
}

// ── Private helpers ──────────────────────────────────────────────────────────

/// Format a GBP value with thousands separators (e.g. 1500000.0 → "1,500,000").
fn format_gbp(value: f64) -> String {
    let rounded = value.round() as i64;
    let (sign, abs_str) = if rounded < 0 {
        ("-", (-rounded).to_string())
    } else {
        ("", rounded.to_string())
    };
    let chars: Vec<char> = abs_str.chars().collect();
    let mut result = String::with_capacity(abs_str.len() + abs_str.len() / 3 + 1);
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(*c);
    }
    format!("{}{}", sign, result)
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_core::{
        ContractRow, DirectorRow, RelatedEntity, SharedConnection, SubCandidateLinkRow,
    };
    use urban_integrity_schema::types::node::EntityStatus;

    fn make_profile(ch_number: Option<&str>) -> EntityProfile {
        EntityProfile {
            id: "test".to_string(),
            name: "Apex Build Ltd".to_owned(),
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

    #[test]
    fn test_build_entity_header_block_includes_ch_number_label() {
        let profile = make_profile(Some("12345678"));
        let block = build_entity_header_block(&profile);
        assert!(
            block.contains("CH Number:"),
            "Header block must contain 'CH Number:' label, got: {block}"
        );
        assert!(
            block.contains("12345678"),
            "Header block must contain the actual CH number, got: {block}"
        );
    }

    #[test]
    fn test_build_entity_header_block_ch_number_em_dash_when_none() {
        let profile = make_profile(None);
        let block = build_entity_header_block(&profile);
        assert!(
            block.contains("CH Number:"),
            "Label must still appear when CH number is None"
        );
        assert!(
            block.contains('\u{2014}'),
            "Em-dash must be rendered for missing CH number"
        );
    }

    #[test]
    fn test_build_entity_directors_table_renders_dash_for_missing_role() {
        let directors = vec![DirectorRow {
            name: "Jane Smith".to_owned(),
            role: None,
            appointed: Some("2020-01-01".to_owned()),
            resigned: None,
            provenance: urban_integrity_core::Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
            temporal: urban_integrity_core::Temporal::unfiltered(
                urban_integrity_core::EffectiveRange::Unrecorded {
                    basis: "hand-built test fixture".to_owned(),
                },
            ),
        }];
        let rendered = build_entity_directors_table(&directors).to_string();
        assert!(
            rendered.contains('\u{2014}'),
            "None role must render as em-dash, got: {rendered}"
        );
        assert!(
            rendered.contains("Jane Smith"),
            "Director name must appear in table"
        );
    }

    #[test]
    fn test_build_entity_related_table_includes_shared_connection_column() {
        let rows = vec![RelatedEntity {
            name: "Beta Build Ltd".to_owned(),
            ch_number: Some("87654321".to_owned()),
            status: EntityStatus::Active,
            shared_connection: SharedConnection {
                kind: "director".to_owned(),
                value: "Jane Smith".to_owned(),
            },
            provenance: urban_integrity_core::Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
        }];
        let rendered = build_entity_related_table(&rows).to_string();
        assert!(
            rendered.contains("Director: Jane Smith"),
            "Shared connection must render as 'Director: Jane Smith', got: {rendered}"
        );
        assert!(
            rendered.contains("Shared Connection"),
            "Header must include 'Shared Connection' column"
        );
    }

    #[test]
    fn test_build_entity_contracts_table_formats_value_gbp() {
        let contracts = vec![ContractRow {
            id: "CF:x-fixture-01".to_owned(),
            name: "Road Repair Contract".to_owned(),
            value_gbp: Some(1_500_000.0),
            status: Some("active".to_owned()),
            published: Some("2024-01-01".to_owned()),
            ward_code: None,
            tender_ref: None,
            application_ref: None,
            provenance: urban_integrity_core::Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
            temporal: urban_integrity_core::Temporal::unfiltered(
                urban_integrity_core::EffectiveRange::Unrecorded {
                    basis: "hand-built test fixture".to_owned(),
                },
            ),
        }];
        let rendered = build_entity_contracts_table(&contracts).to_string();
        assert!(
            rendered.contains("1,500,000"),
            "Value must be comma-formatted, got: {rendered}"
        );
        assert!(
            rendered.contains('\u{a3}'),
            "Value must include £ sign, got: {rendered}"
        );
    }

    #[test]
    fn test_build_entity_disambiguation_table_is_one_indexed() {
        let summaries = vec![
            EntityProfileSummary {
                name: "Alpha Ltd".to_owned(),
                ch_number: Some("11111111".to_owned()),
                status: EntityStatus::Active,
            },
            EntityProfileSummary {
                name: "Alpha Build Ltd".to_owned(),
                ch_number: Some("22222222".to_owned()),
                status: EntityStatus::Active,
            },
        ];
        let rendered = build_entity_disambiguation_table(&summaries).to_string();
        assert!(
            rendered.contains('1'),
            "First row index must be 1, got: {rendered}"
        );
        assert!(
            rendered.contains('2'),
            "Second row index must be 2, got: {rendered}"
        );
        assert!(rendered.contains("Alpha Ltd"));
        assert!(rendered.contains("Alpha Build Ltd"));
    }

    #[test]
    fn test_format_gbp_thousands_separator() {
        assert_eq!(format_gbp(1_500_000.0), "1,500,000");
        assert_eq!(format_gbp(1000.0), "1,000");
        assert_eq!(format_gbp(100.0), "100");
        assert_eq!(format_gbp(0.0), "0");
        assert_eq!(format_gbp(-1_500_000.0), "-1,500,000");
        assert_eq!(format_gbp(-1000.0), "-1,000");
        assert_eq!(format_gbp(-100.0), "-100");
    }

    #[test]
    fn test_build_entity_header_block_ac_det_1_flag_within_12_months() {
        use chrono::NaiveDate;
        let profile = EntityProfile {
            id: "test".to_string(),
            name: "NewCo Ltd".to_owned(),
            ch_number: Some("00000001".to_owned()),
            status: EntityStatus::Active,
            incorporation_date: Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
            addresses: vec![],
            directors: vec![],
            contracts: vec![ContractRow {
                id: "CF:x-fixture-02".to_owned(),
                name: "Big Contract".to_owned(),
                value_gbp: Some(5_000_000.0),
                status: None,
                published: Some("2024-06-15".to_owned()), // ~5.5 months after inc
                ward_code: None,
                tender_ref: None,
                application_ref: None,
                provenance: urban_integrity_core::Provenance::Unattributed {
                    reason: "hand-built test fixture".to_owned(),
                },
                temporal: urban_integrity_core::Temporal::unfiltered(
                    urban_integrity_core::EffectiveRange::Unrecorded {
                        basis: "hand-built test fixture".to_owned(),
                    },
                ),
            }],
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
        };
        let block = build_entity_header_block(&profile);
        assert!(
            block.contains("First contract"),
            "AC-DET-1: flag must appear when contract within 12 months of incorporation, got: {block}"
        );
    }

    #[test]
    fn test_build_entity_header_block_no_flag_beyond_12_months() {
        use chrono::NaiveDate;
        let profile = EntityProfile {
            id: "test".to_string(),
            name: "OldCo Ltd".to_owned(),
            ch_number: None,
            status: EntityStatus::Active,
            incorporation_date: Some(NaiveDate::from_ymd_opt(2020, 1, 1).unwrap()),
            addresses: vec![],
            directors: vec![],
            contracts: vec![ContractRow {
                id: "CF:x-fixture-03".to_owned(),
                name: "Contract".to_owned(),
                value_gbp: None,
                status: None,
                published: Some("2022-06-01".to_owned()), // >24 months after inc
                ward_code: None,
                tender_ref: None,
                application_ref: None,
                provenance: urban_integrity_core::Provenance::Unattributed {
                    reason: "hand-built test fixture".to_owned(),
                },
                temporal: urban_integrity_core::Temporal::unfiltered(
                    urban_integrity_core::EffectiveRange::Unrecorded {
                        basis: "hand-built test fixture".to_owned(),
                    },
                ),
            }],
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
        };
        let block = build_entity_header_block(&profile);
        assert!(
            !block.contains("First contract"),
            "AC-DET-1 flag must NOT appear when contract >12 months after incorporation, got: {block}"
        );
    }

    // ── WR-03: calendar month boundary accuracy ────────────────────────────────

    #[test]
    fn test_ac_det_1_at_exactly_13_calendar_months_no_flag() {
        // 13 calendar months = no flag. The old / 30 logic would compute 395/30 = 13 → no flag
        // but the boundary at exactly 390 days (13*30) would have triggered — now it won't.
        use chrono::NaiveDate;
        let profile = EntityProfile {
            id: "test".to_string(),
            name: "BoundaryCo".to_owned(),
            ch_number: None,
            status: EntityStatus::Active,
            incorporation_date: Some(NaiveDate::from_ymd_opt(2023, 1, 1).unwrap()),
            addresses: vec![],
            directors: vec![],
            contracts: vec![ContractRow {
                id: "CF:x-fixture-04".to_owned(),
                name: "Late Contract".to_owned(),
                value_gbp: None,
                status: None,
                published: Some("2024-02-15".to_owned()), // 13.5 calendar months after inc
                ward_code: None,
                tender_ref: None,
                application_ref: None,
                provenance: urban_integrity_core::Provenance::Unattributed {
                    reason: "hand-built test fixture".to_owned(),
                },
                temporal: urban_integrity_core::Temporal::unfiltered(
                    urban_integrity_core::EffectiveRange::Unrecorded {
                        basis: "hand-built test fixture".to_owned(),
                    },
                ),
            }],
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
        };
        let block = build_entity_header_block(&profile);
        assert!(
            !block.contains("First contract"),
            "AC-DET-1 must NOT flag at 13 calendar months, got: {block}"
        );
    }

    #[test]
    fn test_ac_det_1_at_exactly_12_calendar_months_flags() {
        // Exactly 12 calendar months should still trigger
        use chrono::NaiveDate;
        let profile = EntityProfile {
            id: "test".to_string(),
            name: "ExactCo".to_owned(),
            ch_number: None,
            status: EntityStatus::Active,
            incorporation_date: Some(NaiveDate::from_ymd_opt(2023, 3, 1).unwrap()),
            addresses: vec![],
            directors: vec![],
            contracts: vec![ContractRow {
                id: "CF:x-fixture-05".to_owned(),
                name: "On-Boundary Contract".to_owned(),
                value_gbp: None,
                status: None,
                published: Some("2024-03-15".to_owned()), // exactly 12 calendar months + 14 days
                ward_code: None,
                tender_ref: None,
                application_ref: None,
                provenance: urban_integrity_core::Provenance::Unattributed {
                    reason: "hand-built test fixture".to_owned(),
                },
                temporal: urban_integrity_core::Temporal::unfiltered(
                    urban_integrity_core::EffectiveRange::Unrecorded {
                        basis: "hand-built test fixture".to_owned(),
                    },
                ),
            }],
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
        };
        let block = build_entity_header_block(&profile);
        assert!(
            block.contains("First contract"),
            "AC-DET-1 must flag at exactly 12 calendar months, got: {block}"
        );
    }

    // ── format_entity_status ──────────────────────────────────────────────────

    #[test]
    fn test_format_entity_status_active() {
        let s = format_entity_status(&EntityStatus::Active);
        assert_eq!(s, "Active");
    }

    #[test]
    fn test_format_entity_status_dissolved_includes_date() {
        use chrono::TimeZone as _;
        let dt = chrono::Utc.with_ymd_and_hms(2023, 6, 15, 0, 0, 0).unwrap();
        let s = format_entity_status(&EntityStatus::Dissolved(dt));
        assert!(s.starts_with("Dissolved "), "got: {s}");
        assert!(s.contains("2023-06-15"), "got: {s}");
    }

    #[test]
    fn test_format_entity_status_in_liquidation_includes_date() {
        use chrono::TimeZone as _;
        let dt = chrono::Utc.with_ymd_and_hms(2024, 3, 1, 0, 0, 0).unwrap();
        let s = format_entity_status(&EntityStatus::InLiquidation(dt));
        assert!(s.starts_with("In Liquidation "), "got: {s}");
        assert!(s.contains("2024-03-01"), "got: {s}");
    }

    // ── build_entity_addresses_table ──────────────────────────────────────────

    #[test]
    fn test_build_entity_addresses_table_renders_rows() {
        use urban_integrity_core::AddressRow;
        let addresses = vec![AddressRow {
            id: "location:x-fixture-06".to_owned(),
            postcode: Some("EC1A 1BB".into()),
            address_line_1: Some("123 High Street".into()),
            locality: Some("London".into()),
            provenance: urban_integrity_core::Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
            temporal: urban_integrity_core::Temporal::unfiltered(
                urban_integrity_core::EffectiveRange::Unrecorded {
                    basis: "hand-built test fixture".to_owned(),
                },
            ),
        }];
        let rendered = build_entity_addresses_table(&addresses).to_string();
        assert!(rendered.contains("EC1A 1BB"), "got: {rendered}");
        assert!(rendered.contains("123 High Street"), "got: {rendered}");
        assert!(rendered.contains("London"), "got: {rendered}");
    }

    #[test]
    fn test_build_entity_addresses_table_empty_fields_show_em_dash() {
        use urban_integrity_core::AddressRow;
        let addresses = vec![AddressRow {
            id: "location:x-fixture-07".to_owned(),
            postcode: None,
            address_line_1: None,
            locality: None,
            provenance: urban_integrity_core::Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
            temporal: urban_integrity_core::Temporal::unfiltered(
                urban_integrity_core::EffectiveRange::Unrecorded {
                    basis: "hand-built test fixture".to_owned(),
                },
            ),
        }];
        let rendered = build_entity_addresses_table(&addresses).to_string();
        assert!(
            rendered.contains('\u{2014}'),
            "None fields must render as em-dash, got: {rendered}"
        );
    }

    // ── build_entity_alerts_table ─────────────────────────────────────────────

    #[test]
    fn test_build_entity_alerts_table_renders_rows() {
        use urban_integrity_core::AlertRecord;
        let alerts = vec![AlertRecord {
            id: "a-001".into(),
            motif_type: "PhoenixLoop".into(),
            score: 0.91,
            result_json: "{}".into(),
            detected_at: "2026-05-23T10:00:00Z".into(),
            ward_code: Some("E05001234".into()),
            withdrawn_at: None,
            withdrawn_reason: None,
            participants: vec![],
            first_seen_run_id: None,
            last_seen_run_id: None,
            provenance: urban_integrity_core::Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
        }];
        let rendered = build_entity_alerts_table(&alerts).to_string();
        assert!(rendered.contains("PhoenixLoop"), "got: {rendered}");
        assert!(rendered.contains("0.910"), "got: {rendered}");
        assert!(rendered.contains("E05001234"), "got: {rendered}");
    }

    // ── build_entity_related_table — address kind ─────────────────────────────

    #[test]
    fn test_build_entity_related_table_address_kind() {
        let rows = vec![RelatedEntity {
            name: "Gamma Build Ltd".into(),
            ch_number: Some("99999999".into()),
            status: EntityStatus::Active,
            shared_connection: SharedConnection {
                kind: "address".into(),
                value: "EC1A1BB".into(),
            },
            provenance: urban_integrity_core::Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
        }];
        let rendered = build_entity_related_table(&rows).to_string();
        assert!(
            rendered.contains("Address: EC1A1BB"),
            "Address shared connection must render as 'Address: <postcode>', got: {rendered}"
        );
    }

    #[test]
    fn test_build_entity_related_table_unknown_kind_renders_generically() {
        let rows = vec![RelatedEntity {
            name: "Delta Ltd".into(),
            ch_number: None,
            status: EntityStatus::Active,
            shared_connection: SharedConnection {
                kind: "contract".into(),
                value: "CF-001".into(),
            },
            provenance: urban_integrity_core::Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
        }];
        let rendered = build_entity_related_table(&rows).to_string();
        assert!(
            rendered.contains("contract: CF-001"),
            "Unknown kind must render as '<kind>: <value>', got: {rendered}"
        );
    }

    // ── build_entity_disambiguation_table — with non-Active status ─────────────

    #[test]
    fn test_build_entity_disambiguation_table_with_dissolved_status() {
        use chrono::TimeZone as _;
        let dt = chrono::Utc.with_ymd_and_hms(2022, 1, 1, 0, 0, 0).unwrap();
        use urban_integrity_core::EntityProfileSummary;
        let summaries = vec![EntityProfileSummary {
            name: "OldCo Ltd".into(),
            ch_number: Some("55555555".into()),
            status: EntityStatus::Dissolved(dt),
        }];
        let rendered = build_entity_disambiguation_table(&summaries).to_string();
        assert!(
            rendered.contains("Dissolved"),
            "Dissolved status must appear in disambiguation table, got: {rendered}"
        );
        assert!(rendered.contains("2022-01-01"), "got: {rendered}");
    }

    #[test]
    fn test_ac_det_1_390_day_false_positive_is_fixed() {
        // 390 days = 13×30 — the old integer-division formula would flag this as "13 months ≤ 12"
        // (since 390/30 = 13 — wait, that would NOT trigger under old code since 13 > 12).
        // The real false-positive was at days 361-390: 361/30=12, which triggered the ≤12 check.
        // This test verifies day 365 (13 calendar months in a leap year) is NOT flagged.
        use chrono::NaiveDate;
        let profile = EntityProfile {
            id: "test".to_string(),
            name: "OldBugCo".to_owned(),
            ch_number: None,
            status: EntityStatus::Active,
            incorporation_date: Some(NaiveDate::from_ymd_opt(2023, 1, 1).unwrap()),
            addresses: vec![],
            directors: vec![],
            contracts: vec![ContractRow {
                id: "CF:x-fixture-08".to_owned(),
                name: "Day 366 Contract".to_owned(),
                value_gbp: None,
                status: None,
                // day 366 from 2023-01-01 = 2024-01-01 = exactly 12 calendar months: SHOULD flag
                // day 366+31 from 2023-01-01 = 2024-02-01 = 13 calendar months: should NOT flag
                published: Some("2024-02-01".to_owned()),
                ward_code: None,
                tender_ref: None,
                application_ref: None,
                provenance: urban_integrity_core::Provenance::Unattributed {
                    reason: "hand-built test fixture".to_owned(),
                },
                temporal: urban_integrity_core::Temporal::unfiltered(
                    urban_integrity_core::EffectiveRange::Unrecorded {
                        basis: "hand-built test fixture".to_owned(),
                    },
                ),
            }],
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
        };
        let block = build_entity_header_block(&profile);
        assert!(
            !block.contains("First contract"),
            "AC-DET-1 must NOT flag at 13 calendar months (old /30 false-positive regression check), got: {block}"
        );
    }

    // ── build_entity_osint_block deep flag (Phase 19-06, SURF-02) ─────────────

    fn make_profile_with_sub_candidates() -> EntityProfile {
        EntityProfile {
            id: "entity:test-corp".to_string(),
            name: "Test Corp".to_owned(),
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
            sub_candidate_links: vec![SubCandidateLinkRow {
                source_node_id: "entity:test-corp".to_owned(),
                target_node_id: "director:john-smith".to_owned(),
                source: "charity_commission".to_owned(),
                confidence: 0.65,
                resolution_type: "fuzzy_name".to_owned(),
                evidence_json: "{\"score\":0.65}".to_owned(),
            }],
            identity_provenance: urban_integrity_core::Provenance::Unattributed {
                reason: "hand-built test fixture".to_owned(),
            },
            as_at: None,
        }
    }

    // Test 1: deep=true shows sub-candidate section heading
    #[test]
    fn test_build_entity_osint_block_deep_true_shows_candidate_osint_section() {
        let profile = make_profile_with_sub_candidates();
        let block = build_entity_osint_block(&profile, true);
        assert!(
            block.contains("Sub-Candidate OSINT Links (0.50\u{2013}0.69, investigative only)"),
            "deep=true must render 'Sub-Candidate OSINT Links' heading, got:\n{block}"
        );
    }

    // Test 2: deep=false does NOT show sub-candidate section
    #[test]
    fn test_build_entity_osint_block_deep_false_no_candidate_section() {
        let profile = make_profile_with_sub_candidates();
        let block = build_entity_osint_block(&profile, false);
        assert!(
            !block.contains("Sub-Candidate OSINT Links"),
            "deep=false must NOT render 'Sub-Candidate OSINT Links', got:\n{block}"
        );
    }

    // Test 3: deep=true with empty sub_candidate_links shows "(none)"
    #[test]
    fn test_build_entity_osint_block_deep_true_empty_shows_none() {
        let profile = make_profile(None); // empty sub_candidate_links
        let block = build_entity_osint_block(&profile, true);
        assert!(
            block.contains("Sub-Candidate OSINT Links (0.50\u{2013}0.69, investigative only)"),
            "deep=true must render section heading even with no data, got:\n{block}"
        );
        // The section should show "(none)" since sub_candidate_links is empty
        // (check after the section heading appears)
        let after_heading = block
            .find("Sub-Candidate OSINT Links")
            .map(|i| &block[i..])
            .unwrap_or("");
        assert!(
            after_heading.contains("(none)"),
            "deep=true with empty sub_candidate_links must render '(none)', got:\n{after_heading}"
        );
    }

    // Test 4: existing tests call build_entity_osint_block with deep=false (signature change)
    #[test]
    fn test_build_entity_osint_block_existing_deep_false_still_works() {
        let profile = make_profile(Some("12345678"));
        // New signature requires deep parameter; passing false reproduces old behaviour.
        let block = build_entity_osint_block(&profile, false);
        assert!(
            block.contains("Beneficial Owners"),
            "Existing OSINT sections must still render with deep=false, got:\n{block}"
        );
        assert!(
            !block.contains("Sub-Candidate OSINT Links"),
            "Sub-Candidate OSINT section must be absent with deep=false, got:\n{block}"
        );
    }
}
