//! D-04's typed evidence rendering — `pivot_command` is the single point deriving a CLI command
//! string from a `RecordRef`'s `kind`/`id`, so a probe cannot pair an id with the wrong command
//! and "which findings are pivotable" is answerable by type rather than by grep.
//!
//! `pivot_command` is a pure match with no dependency on any other section. It emits **three**
//! command forms from **five** `RecordKind` variants: D-06 widened the enum from "which command
//! opens this" to "what class of thing is this", and `Entity`/`Contract`/`Location` are all
//! opened by `entity <id>`. The collapse lives here and nowhere else.
//!
//! **A caveat no probe hits today, and a renderer will.** `urban-integrity-cli entity <id>`
//! resolves `Entity` and `OffshoreEntity` node weights only (`query_entity_by_id`), so the
//! command this emits for a `Contract` or `Location` ref names a real node that the command
//! cannot yet open. Nothing renders one: every probe in the battery emits `RecordKind::Entity`,
//! and the two new kinds are constructed only by the workbench profile mapper, which does not go
//! through here. The subject lookup is what has to widen before either kind is put in front of an
//! analyst.
//! `render_evidence` is RPT-04's per-finding pivotability guarantee (D-14): a finding naming
//! specific records renders their ids plus the pivot command that opens them; a finding
//! measuring only an aggregate states how its number is reproduced.

use urban_integrity_schema::types::verdict::{Finding, RecordKind, RecordRef};

/// D-04's single point deriving a CLI command string from a `RecordRef`'s `kind`/`id`.
///
/// Three forms, five kinds — see this module's doc comment for why, and for the caveat about
/// `entity <id>` on a `Contract`/`Location` id.
pub fn pivot_command(r: &RecordRef) -> String {
    match r.kind {
        // One arm, not three: a node id is opened by `entity <id>` whatever class of node it
        // names. Splitting them here would invent command forms the CLI does not have.
        RecordKind::Entity | RecordKind::Contract | RecordKind::Location => {
            format!("urban-integrity-cli entity {}", r.id)
        }
        RecordKind::CompanyNumber => {
            format!("urban-integrity-cli verify --company-number {}", r.id)
        }
        RecordKind::IdentityLink => format!("urban-integrity-cli links show {}", r.id),
    }
}

/// Renders every `EvidenceRow` in `finding.evidence`: the row's `text`, then one line per
/// `RecordRef` in `refs` showing `pivot_command(r)`. If `finding.evidence` is genuinely empty
/// (`vec![]`, no rows at all — a probe that split nothing into evidence, which should not happen
/// after Wave 2 but is defended against here), appends a generic fallback reproduction sentence.
/// A finding whose evidence rows exist but carry no `refs` (e.g. `land-flip-candidates`'s
/// aggregate-only shape, plan 35-07) does NOT get the fallback appended — its own row text is
/// already the probe's specific reproduction note, and appending the generic line on top would
/// be redundant/misleading.
pub fn render_evidence(finding: &Finding) -> String {
    let mut out = String::new();
    for row in &finding.evidence {
        out.push_str(&row.text);
        out.push('\n');
        for r in &row.refs {
            out.push_str(&format!("  -> pivot: {}\n", pivot_command(r)));
        }
    }
    if finding.evidence.is_empty() {
        out.push_str(
            "Aggregate measurement — no individual records available to pivot to; re-run \
             `urban-integrity-cli probe` against the same database state to reproduce this \
             figure.\n",
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use urban_integrity_schema::types::verdict::{EvidenceRow, Severity, Verdict};

    fn finding_with_evidence(evidence: Vec<EvidenceRow>) -> Finding {
        Finding {
            probe: "test-probe".to_string(),
            verdict: Verdict::classify(1, 1, 1, 1, ""),
            summary: "s".to_string(),
            cause: None,
            severity: Severity::Info,
            evidence,
            attributions: vec![],
            remediation_hint: None,
        }
    }

    #[test]
    fn pivot_command_entity_contains_command_and_id() {
        let r = RecordRef {
            kind: RecordKind::Entity,
            id: "director:jane-smith".to_string(),
        };
        let cmd = pivot_command(&r);
        assert!(cmd.contains("entity"));
        assert!(cmd.contains("director:jane-smith"));
    }

    #[test]
    fn pivot_command_company_number_contains_verify_flag_and_id() {
        let r = RecordRef {
            kind: RecordKind::CompanyNumber,
            id: "12345678".to_string(),
        };
        let cmd = pivot_command(&r);
        assert!(cmd.contains("verify"));
        assert!(cmd.contains("--company-number"));
        assert!(cmd.contains("12345678"));
    }

    #[test]
    fn pivot_command_identity_link_contains_links_show_and_id() {
        let r = RecordRef {
            kind: RecordKind::IdentityLink,
            id: "identity_links:abc123".to_string(),
        };
        let cmd = pivot_command(&r);
        assert!(cmd.contains("links show"));
        assert!(cmd.contains("identity_links:abc123"));
    }

    #[test]
    fn render_evidence_renders_every_ref_not_just_the_first() {
        let refs = vec![
            RecordRef {
                kind: RecordKind::Entity,
                id: "director:a".to_string(),
            },
            RecordRef {
                kind: RecordKind::Entity,
                id: "director:b".to_string(),
            },
        ];
        let finding = finding_with_evidence(vec![EvidenceRow {
            text: "two directors".to_string(),
            refs,
        }]);
        let rendered = render_evidence(&finding);
        assert!(
            rendered.contains("director:a"),
            "first ref's pivot command missing, got:\n{rendered}"
        );
        assert!(
            rendered.contains("director:b"),
            "second ref's pivot command missing (not just the first), got:\n{rendered}"
        );
    }

    #[test]
    fn render_evidence_falls_back_when_evidence_is_genuinely_empty() {
        let finding = finding_with_evidence(vec![]);
        let rendered = render_evidence(&finding);
        assert!(
            rendered.contains("Aggregate measurement"),
            "genuinely empty evidence must render the generic fallback sentence, got:\n{rendered}"
        );
    }

    #[test]
    fn render_evidence_does_not_append_fallback_when_rows_exist_with_no_refs() {
        let finding = finding_with_evidence(vec![EvidenceRow {
            text: "aggregate-only reproduction note".to_string(),
            refs: vec![],
        }]);
        let rendered = render_evidence(&finding);
        assert!(
            !rendered.contains("Aggregate measurement — no individual records"),
            "a probe that already supplied its own specific reproduction note must not also get \
             the generic fallback appended, got:\n{rendered}"
        );
        assert!(rendered.contains("aggregate-only reproduction note"));
    }
}
