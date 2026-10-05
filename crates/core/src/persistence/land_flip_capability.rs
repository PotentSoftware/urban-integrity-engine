//! `has_gate3_corroboration` — the duplicated gate-3 corroboration predicate (Phase 34, D-05,
//! Pattern 5).
//!
//! A deliberate DUPLICATE, not a shared import, of
//! `crates/enricher/src/land_flip.rs::resolve_corroboration` — `crates/core` cannot import
//! `crates/enricher` (dependency direction: enricher -> core, never the reverse), so the probe
//! subcommand's capability check needs its own copy of exactly the same SurrealQL body,
//! including the Rust-side belt-and-braces re-check, so the probe's notion of "capable" cannot
//! drift from what gate-3 itself actually evaluates.
//!
//! `query_shape_guard.rs`'s `broken_rule` exempts this multi-statement `LET ... SELECT` shape
//! entirely — it does not start with a bare `select` after trim/lowercase — a documented,
//! accepted blind spot per ROADMAP's Phase-31 callout, not something this module newly solves.

use surrealdb::{engine::any::Any, Surreal};

/// Does at least one `property:*`-sourced `identity_links` row with `confidence >= 0.70` and
/// `status IN ['auto_linked', 'confirmed']` target `applicant_id` or any entity that
/// `directs`/`controls` it (the `$net` expansion)?
///
/// Identical predicate to `resolve_corroboration` (`crates/enricher/src/land_flip.rs:772-822`) —
/// same multi-statement query, same `LIMIT 10`, same Rust-side re-check of `status`/`confidence`
/// on each candidate row before accepting it, so a row that merely LOOKS qualifying under a
/// looser read of the SurrealQL result cannot flip this to `true`.
pub async fn has_gate3_corroboration(
    db: &Surreal<Any>,
    applicant_id: &str,
) -> surrealdb::Result<bool> {
    let q = "\
        LET $directs = SELECT VALUE in.entity_id FROM directs \
            WHERE out.entity_id = $applicant_id \
              AND (meta.effective_to = NONE OR meta.effective_to > time::now()); \
        LET $controls = SELECT VALUE in.entity_id FROM controls \
            WHERE out.entity_id = $applicant_id \
              AND (meta.effective_to = NONE OR meta.effective_to > time::now()); \
        LET $net = array::union([$applicant_id], array::union($directs, $controls)); \
        SELECT id, source_node_id, target_node_id, confidence, status FROM identity_links \
            WHERE target_node_id IN $net \
              AND string::starts_with(source_node_id, 'property:') \
              AND confidence >= 0.70 \
              AND status IN ['auto_linked', 'confirmed'] \
            LIMIT 10;";
    let mut resp = db
        .query(q)
        .bind(("applicant_id", applicant_id.to_string()))
        .await?;
    // Statement index 3 is the final SELECT (0..2 are LET statements).
    let rows: Vec<serde_json::Value> = resp.take(3)?;

    for row in &rows {
        // Rust-side belt-and-braces re-check (mirrors resolve_corroboration).
        let status = row["status"].as_str().unwrap_or("");
        if status != "auto_linked" && status != "confirmed" {
            continue;
        }
        let confidence = row["confidence"].as_f64().unwrap_or(0.0);
        if confidence < 0.70 {
            continue;
        }
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_tests_enabled() -> bool {
        urban_integrity_test_support::db_tests_enabled()
    }

    async fn seed_identity_link(
        db: &Surreal<Any>,
        key: &str,
        source_node_id: &str,
        target_node_id: &str,
        confidence: f64,
        status: &str,
    ) {
        db.query(
            "CREATE type::record('identity_links', $key) SET \
             source_node_id = $src, target_node_id = $tgt, resolution_type = 'exact_ch_number', \
             confidence = $conf, source = 'ccod', resolved_at = $resolved_at, \
             evidence_json = '{}', status = $status",
        )
        .bind(("key", key.to_string()))
        .bind(("src", source_node_id.to_string()))
        .bind(("tgt", target_node_id.to_string()))
        .bind(("conf", confidence))
        .bind(("status", status.to_string()))
        .bind(("resolved_at", chrono::Utc::now()))
        .await
        .expect("seed identity_links row")
        .check()
        .expect("seed identity_links row check");
    }

    #[tokio::test]
    async fn db_has_gate3_corroboration_true_for_direct_ccod_shaped_link() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_land_flip_cap",
            "direct_ccod",
        )
        .await;

        let applicant = "GB-COH:00000001";
        seed_identity_link(
            &db,
            "direct_link",
            "property:x",
            applicant,
            1.0,
            "auto_linked",
        )
        .await;

        assert!(
            has_gate3_corroboration(&db, applicant)
                .await
                .expect("query"),
            "a direct CCOD-shaped link must return true"
        );
    }

    #[tokio::test]
    async fn db_has_gate3_corroboration_true_via_directs_net_expansion() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_land_flip_cap",
            "net_expansion",
        )
        .await;

        let applicant = "GB-COH:00000002";
        let director = "director:jane-smith-net";

        // Typed entity/officer node rows -- `directs` is `FROM entity | officer TO entity |
        // officer SCHEMAFULL` (crates/schema/src/db.rs), so RELATE endpoints must be real
        // records in those tables, and the predicate reads their `entity_id` field, not the
        // record key.
        db.query(
            "UPSERT type::record('entity', 'cap-net-applicant') SET \
             name = 'Net Expansion Applicant Ltd', entity_id = $applicant",
        )
        .bind(("applicant", applicant))
        .await
        .expect("seed entity")
        .check()
        .expect("seed entity check");
        db.query(
            "UPSERT type::record('officer', 'cap-net-director') SET \
             name = 'Jane Smith', entity_id = $director",
        )
        .bind(("director", director))
        .await
        .expect("seed officer")
        .check()
        .expect("seed officer check");

        db.query(
            "LET $d = type::record('officer', 'cap-net-director'); \
             LET $a = type::record('entity', 'cap-net-applicant'); \
             RELATE $d->directs->$a SET meta.created_at = time::now(), \
                 data.details = 'test-fixture'",
        )
        .await
        .expect("relate")
        .check()
        .expect("relate check");

        seed_identity_link(
            &db,
            "director_link",
            "property:y",
            director,
            0.90,
            "confirmed",
        )
        .await;

        assert!(
            has_gate3_corroboration(&db, applicant)
                .await
                .expect("query"),
            "a link targeting a director who directs the applicant must return true via net \
             expansion"
        );
    }

    #[tokio::test]
    async fn db_has_gate3_corroboration_false_for_ocod_shaped_link() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_land_flip_cap",
            "ocod_shaped",
        )
        .await;

        let applicant = "GB-COH:00000003";
        seed_identity_link(
            &db,
            "ocod_link",
            "property:z",
            applicant,
            0.65,
            "pending_review",
        )
        .await;

        assert!(
            !has_gate3_corroboration(&db, applicant)
                .await
                .expect("query"),
            "an OCOD-shaped link (confidence 0.65, pending_review) must return false"
        );
    }

    #[tokio::test]
    async fn db_has_gate3_corroboration_false_when_no_link_exists() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let db = urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_land_flip_cap",
            "no_link",
        )
        .await;

        assert!(
            !has_gate3_corroboration(&db, "GB-COH:00000004")
                .await
                .expect("query"),
            "an empty TestNs with no identity_links row must return false"
        );
    }
}
