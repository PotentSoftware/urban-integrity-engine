use anyhow::{Context as _, Result};
use clap::Args;
use std::sync::Arc;
use tracing::{info, warn};
use urban_integrity_core::config::AppConfig;
use urban_integrity_ingestor::projector::{Projector, RawSqlConfig};

use crate::db::{connect_db, reauth_db, resolve_analysis_db};

/// Migration DDL: removes and redefines all 12 TYPE RELATION tables with IN/OUT constraints.
///
/// Rules enforced here (verified by test_migration_ddl_properties):
/// - No IF NOT EXISTS on DEFINE TABLE lines (SCH-02: REMOVE first, then redefine cleanly)
/// - No ENFORCED keyword (D-14)
/// - All 11 tables removed and redefined (D-11)
const MIGRATION_DDL: &str = "
REMOVE TABLE IF EXISTS directs;
DEFINE TABLE directs TYPE RELATION FROM entity | officer TO entity | officer SCHEMAFULL;
DEFINE FIELD meta.created_at    ON directs TYPE datetime;
DEFINE FIELD meta.effective_from ON directs TYPE option<datetime>;
DEFINE FIELD meta.effective_to   ON directs TYPE option<datetime>;
DEFINE FIELD data.details        ON directs TYPE string;
DEFINE FIELD data.officer_role   ON directs TYPE option<string>;

REMOVE TABLE IF EXISTS located_at;
DEFINE TABLE located_at TYPE RELATION FROM entity | contract TO location SCHEMAFULL;
DEFINE FIELD meta.created_at    ON located_at TYPE datetime;
DEFINE FIELD meta.effective_from ON located_at TYPE option<datetime>;
DEFINE FIELD meta.effective_to   ON located_at TYPE option<datetime>;
DEFINE FIELD data.details        ON located_at TYPE string;

REMOVE TABLE IF EXISTS awarded_to;
DEFINE TABLE awarded_to TYPE RELATION FROM entity TO contract SCHEMAFULL;
DEFINE FIELD meta.created_at    ON awarded_to TYPE datetime;
DEFINE FIELD meta.effective_from ON awarded_to TYPE option<datetime>;
DEFINE FIELD meta.effective_to   ON awarded_to TYPE option<datetime>;
DEFINE FIELD data.details        ON awarded_to TYPE string;

REMOVE TABLE IF EXISTS issued_by;
DEFINE TABLE issued_by TYPE RELATION FROM entity TO contract SCHEMAFULL;
DEFINE FIELD meta.created_at    ON issued_by TYPE datetime;
DEFINE FIELD meta.effective_from ON issued_by TYPE option<datetime>;
DEFINE FIELD meta.effective_to   ON issued_by TYPE option<datetime>;
DEFINE FIELD data.details        ON issued_by TYPE string;

REMOVE TABLE IF EXISTS represents;
DEFINE TABLE represents TYPE RELATION FROM ward TO contract SCHEMAFULL;
DEFINE FIELD meta.created_at    ON represents TYPE datetime;
DEFINE FIELD meta.effective_from ON represents TYPE option<datetime>;
DEFINE FIELD meta.effective_to   ON represents TYPE option<datetime>;
DEFINE FIELD data.details        ON represents TYPE string;

REMOVE TABLE IF EXISTS controls;
DEFINE TABLE controls TYPE RELATION FROM psc TO entity SCHEMAFULL;
DEFINE FIELD meta.created_at    ON controls TYPE datetime;
DEFINE FIELD meta.effective_from ON controls TYPE option<datetime>;
DEFINE FIELD meta.effective_to   ON controls TYPE option<datetime>;

REMOVE TABLE IF EXISTS trustee_of;
DEFINE TABLE trustee_of TYPE RELATION FROM entity TO charity SCHEMAFULL;
DEFINE FIELD meta.created_at    ON trustee_of TYPE datetime;
DEFINE FIELD meta.effective_from ON trustee_of TYPE option<datetime>;
DEFINE FIELD meta.effective_to   ON trustee_of TYPE option<datetime>;

REMOVE TABLE IF EXISTS owns_property;
DEFINE TABLE owns_property TYPE RELATION FROM entity | offshore_entity TO property SCHEMAFULL;
DEFINE FIELD meta.created_at    ON owns_property TYPE datetime;
DEFINE FIELD meta.effective_from ON owns_property TYPE option<datetime>;
DEFINE FIELD meta.effective_to   ON owns_property TYPE option<datetime>;
DEFINE FIELD data.price_paid     ON owns_property TYPE option<float>;
DEFINE FIELD data.tenure         ON owns_property TYPE option<string>;

REMOVE TABLE IF EXISTS offshore_link;
DEFINE TABLE offshore_link TYPE RELATION FROM entity TO offshore_entity SCHEMAFULL;
DEFINE FIELD meta.created_at    ON offshore_link TYPE datetime;
DEFINE FIELD meta.effective_from ON offshore_link TYPE option<datetime>;
DEFINE FIELD meta.effective_to   ON offshore_link TYPE option<datetime>;

REMOVE TABLE IF EXISTS donates_to;
DEFINE TABLE donates_to TYPE RELATION FROM entity TO political_actor SCHEMAFULL;
DEFINE FIELD meta.created_at    ON donates_to TYPE datetime;
DEFINE FIELD meta.effective_from ON donates_to TYPE option<datetime>;
DEFINE FIELD meta.effective_to   ON donates_to TYPE option<datetime>;

REMOVE TABLE IF EXISTS disqualified_from;
DEFINE TABLE disqualified_from TYPE RELATION FROM officer TO disqualification SCHEMAFULL;
DEFINE FIELD meta.created_at        ON disqualified_from TYPE datetime;
DEFINE FIELD meta.effective_from     ON disqualified_from TYPE option<datetime>;
DEFINE FIELD meta.effective_to       ON disqualified_from TYPE option<datetime>;
DEFINE FIELD data.disqualified_from  ON disqualified_from TYPE option<datetime>;
DEFINE FIELD data.disqualified_until ON disqualified_from TYPE option<datetime>;
DEFINE FIELD data.case_number        ON disqualified_from TYPE option<string>;

REMOVE TABLE IF EXISTS declares_interest;
DEFINE TABLE declares_interest TYPE RELATION FROM political_actor TO entity SCHEMAFULL;
DEFINE FIELD meta.created_at     ON declares_interest TYPE datetime;
DEFINE FIELD meta.effective_from ON declares_interest TYPE option<datetime>;
DEFINE FIELD meta.effective_to   ON declares_interest TYPE option<datetime>;
DEFINE FIELD data.council        ON declares_interest TYPE option<string>;
DEFINE FIELD data.category       ON declares_interest TYPE option<string>;
";

/// How many relation tables [`MIGRATION_DDL`] actually covers, derived from the DDL rather than
/// written down beside it.
///
/// The three operator-facing lines below used to say "11" while the DDL removed and redefined 12
/// and `test_migration_ddl_properties` asserted 12 twice. Nothing was broken -- the migration
/// always covered all 12 -- but the command told the operator it was doing less than it was, and
/// four hand-maintained copies of one number is what let them disagree. Deriving it leaves one
/// source of truth, so adding a relation table cannot desynchronise the message from the work.
fn relation_table_count() -> usize {
    MIGRATION_DDL.matches("REMOVE TABLE").count()
}

/// Apply IN/OUT schema constraints to all 12 relation tables and re-project (SCH-01..SCH-04).
///
/// PREREQUISITE: Run 'urban-integrity enrich' before migrate to ensure OSINT nodes are in
/// graph_events (see Pitfall 3 in the phase 21 research notes — missing OSINT nodes cause edges_skipped
/// regression after REMOVE TABLE wipes the relation data).
#[derive(Debug, Args)]
pub struct MigrateArgs {
    /// Print DDL statements without executing (safety preview for analysts).
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(args: MigrateArgs) -> Result<()> {
    if args.dry_run {
        println!("Dry-run: the following DDL would be applied:");
        println!("{}", MIGRATION_DDL);
        return Ok(());
    }

    let config = AppConfig::load()?;
    let mut db = Arc::new(connect_db(&config).await?);
    // Resolved ONCE and handed to every re-auth below, mirroring `serve`'s `serve_db_name`
    // pattern (main.rs) — re-signing in resets the connection's database selection, so a
    // refresh has to restore the analysis this process actually connected to, not whatever
    // `config.toml` names.
    let db_name = resolve_analysis_db(&config).await?;

    // Build the raw HTTP SQL config for Projector (same pattern as project.rs).
    let http_base = config
        .surreal
        .ws_url
        .replace("wss://", "https://")
        .replace("ws://", "http://");
    let raw = RawSqlConfig {
        base_url: http_base,
        namespace: config.surreal.namespace.clone(),
        database: config.surreal.database.clone(),
        auth: match (
            config.surreal.username.as_deref(),
            config.surreal.password.as_deref(),
        ) {
            (Some(u), Some(p)) => Some((u.to_owned(), p.to_owned())),
            _ => None,
        },
    };

    // Step 1: Pre-flight — capture current edges_skipped baseline BEFORE any DDL changes.
    println!(
        "Migrating {} relation tables with IN/OUT constraints (SCH-01..SCH-04).",
        relation_table_count()
    );
    println!("Step 1: Pre-flight projection to capture edges_skipped baseline…");
    let before_projector = Projector::new(db.clone(), raw.clone());
    let before_stats = before_projector.run().await?;
    info!(
        edges_skipped = before_stats.edges_skipped,
        edges_related = before_stats.edges_related,
        nodes_upserted = before_stats.nodes_upserted,
        "Pre-flight: baseline captured"
    );
    println!(
        "Pre-flight: edges_skipped baseline = {}",
        before_stats.edges_skipped
    );

    // Step 2: Apply the migration DDL.
    // All 12 relation tables are removed then redefined with IN/OUT constraints.
    // Critical: no IF NOT EXISTS on DEFINE TABLE (SCH-02) — the table was just removed.
    //
    // Re-auth immediately before the DDL: Step 1's pre-flight projection reads/writes via
    // `Projector`, which mixes this same long-lived `db` handle (SDK session, node upserts)
    // with a separate stateless raw-HTTP/Basic-Auth path (edge relates). On a large corpus the
    // edge phase alone can run for many hours (826m21s / ~13.8h observed against a 428k-edge
    // graph) with `db` untouched the whole time — long enough for its SurrealDB HTTP session
    // token to expire (~1h default, see `reauth_db`'s doc comment). The very next use of `db`
    // is this DDL, so without a refresh here it 401s before a single statement runs, exactly as
    // happened live on 2026-08-05. Same fix shape as the OSINT drain-session re-auth in main.rs.
    println!(
        "Step 2: Applying REMOVE + DEFINE DDL for {} relation tables…",
        relation_table_count()
    );
    // reauth_db returns `Some(new_db)` when the existing session had genuinely, fully
    // expired and had to be replaced outright (in-place re-signin is provably impossible
    // once that happens — see reauth_db's doc comment); swap `db` to the replacement so
    // every subsequent use in this function talks to a connection that actually works.
    if let Some(new_db) = reauth_db(&db, &config, &db_name)
        .await
        .context("session re-authentication before migration DDL failed")?
    {
        db = Arc::new(new_db);
    }
    db.query(MIGRATION_DDL)
        .await
        .context("migrate DDL failed")?;
    info!("Migration DDL applied successfully");

    // Step 3: Re-project to repopulate the relation tables from graph_events.
    // Re-auth again for the same reason: the post-DDL `Projector` reuses `db` for its own node
    // upserts and can itself run long on a large corpus, so the token must be fresh going in
    // rather than assumed valid because Step 2 just succeeded.
    println!("Step 3: Re-projecting graph_events into the new constrained tables…");
    if let Some(new_db) = reauth_db(&db, &config, &db_name)
        .await
        .context("session re-authentication before post-migration projection failed")?
    {
        db = Arc::new(new_db);
    }
    let after_projector = Projector::new(db.clone(), raw);
    let after_stats = after_projector.run().await?;
    info!(
        nodes_upserted = after_stats.nodes_upserted,
        edges_related = after_stats.edges_related,
        nodes_skipped = after_stats.nodes_skipped,
        edges_skipped = after_stats.edges_skipped,
        "Post-migration projection complete"
    );

    // Step 4: Compare before/after and report (D-12).
    let delta = after_stats.edges_skipped as i64 - before_stats.edges_skipped as i64;
    println!(
        "Migration complete: edges_skipped before={}, after={}, delta={}",
        before_stats.edges_skipped, after_stats.edges_skipped, delta
    );

    if delta > 0 {
        warn!(
            delta,
            "WARNING: edges_skipped increased by {} after migration -- OSINT nodes may be \
             missing from graph_events. Run 'urban-integrity enrich' first.",
            delta
        );
        println!(
            "WARNING: edges_skipped increased by {} after migration — OSINT nodes may be \
             missing from graph_events. Run 'urban-integrity enrich' first.",
            delta
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify structural properties of MIGRATION_DDL without requiring a SurrealDB instance.
    /// These assertions correspond directly to the D-11 (all relation tables), SCH-02 (no IF NOT
    /// EXISTS), and D-14 (no ENFORCED) invariants.
    ///
    /// The count moved from 11 to 12 on 2026-08-23 when `declares_interest` was added for POL-02
    /// (see the POL-02 directs-relate-rejected investigation notes). It must stay in step with
    /// `SCHEMA_DEF`'s own relation-count assertion in `crates/schema/src/db.rs` — this DDL is a
    /// second, independent copy of the relation definitions, so a table added to one and not the
    /// other leaves `migrate` unable to constrain it.
    #[test]
    fn test_migration_ddl_properties() {
        // D-11: All 12 relation tables removed
        assert_eq!(
            MIGRATION_DDL.matches("REMOVE TABLE").count(),
            12,
            "MIGRATION_DDL must REMOVE TABLE 12 times (one per relation table)"
        );

        // D-11: All 12 relation tables redefined with IN/OUT constraints
        assert_eq!(
            MIGRATION_DDL.matches("TYPE RELATION FROM").count(),
            12,
            "MIGRATION_DDL must have 12 TYPE RELATION FROM clauses (one per relation table)"
        );

        // The operator-facing message must state the same count the DDL performs. It said 11
        // against a 12-table DDL until 2026-09-03; deriving it made that impossible, and this
        // pins the derivation to the two assertions above rather than to a literal.
        assert_eq!(
            relation_table_count(),
            12,
            "relation_table_count() must agree with the REMOVE TABLE / TYPE RELATION FROM counts \
             asserted above -- if a relation table was added, update all three together"
        );

        // SCH-02: No IF NOT EXISTS in migration DEFINE TABLE lines — tables were just removed;
        // IF NOT EXISTS would be a no-op and silently skip the redefinition.
        assert!(
            !MIGRATION_DDL.contains("IF NOT EXISTS"),
            "MIGRATION_DDL must not contain IF NOT EXISTS (SCH-02: would silently no-op after REMOVE)"
        );

        // D-14: ENFORCED keyword must not appear
        assert!(
            !MIGRATION_DDL.contains("ENFORCED"),
            "MIGRATION_DDL must not contain ENFORCED (D-14)"
        );
    }

    /// Regression for the live migrate failure (2026-08-05, `real 826m21.552s`): Step 1's
    /// pre-flight projection ran long enough for the long-lived `db` handle's SurrealDB HTTP
    /// session to expire, and Step 2's `db.query(MIGRATION_DDL)` — the first use of `db` since —
    /// 401'd before a single statement ran. The fix is a `reauth_db(&db, ...)` call immediately
    /// before each subsequent use of `db`. This is a pure source-text assertion (no SurrealDB
    /// instance required) that the ordering fix is actually present in `run()`'s source, not
    /// just in a comment: every `db.query(...)` call after Step 1 must be preceded, in source
    /// order, by a `reauth_db(` call. A behavioural proof that `reauth_db` actually recovers an
    /// expired session lives in `migrate_ddl_recovers_from_expired_session_via_reauth` below
    /// (DB-gated, `URBAN_DB_TESTS=1`).
    #[test]
    fn reauth_precedes_every_post_preflight_db_query() {
        let src = include_str!("migrate.rs");
        let run_start = src
            .find("pub async fn run(")
            .expect("run() function not found in migrate.rs source");
        // Bound the slice to just `run()`'s own body — the tests module below (this very test,
        // its sibling `migrate_ddl_recovers_from_expired_session_via_reauth`, etc.) also
        // contains the literal text "reauth_db(" and "db.query(MIGRATION_DDL)", which would
        // otherwise be double-counted.
        let run_end = src[run_start..]
            .find("\n#[cfg(test)]")
            .expect("#[cfg(test)] module not found after run() in migrate.rs source");
        let run_body = &src[run_start..run_start + run_end];

        // Only inspect the two `db.query(...)` call sites known to run after Step 1's
        // pre-flight (the DDL in Step 2, and — indirectly, via `Projector::new(db.clone(), ..)`
        // — the re-projection in Step 3). Locate each `reauth_db(` and `db.query(MIGRATION_DDL)`
        // occurrence and assert a `reauth_db(` call appears immediately before it in source
        // order, with nothing but the DDL invocation itself in between.
        let reauth_positions: Vec<usize> = run_body
            .match_indices("reauth_db(")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            reauth_positions.len(),
            2,
            "run() must call reauth_db(...) exactly twice — once before Step 2's DDL and once \
             before Step 3's re-projection — found {} call(s); if Step 1's preflight ever grows \
             a third long-running `db` use, add a matching reauth_db call and update this count",
            reauth_positions.len()
        );

        let ddl_pos = run_body
            .find("db.query(MIGRATION_DDL)")
            .expect("db.query(MIGRATION_DDL) call not found in run()");
        assert!(
            reauth_positions[0] < ddl_pos,
            "reauth_db(...) must be called before db.query(MIGRATION_DDL) in Step 2 — the exact \
             live 401 this test guards against"
        );

        let projector_pos = run_body
            .find("let after_projector = Projector::new(db.clone(), raw);")
            .expect("Step 3's Projector::new(db.clone(), raw) call not found in run()");
        assert!(
            reauth_positions[1] > ddl_pos && reauth_positions[1] < projector_pos,
            "reauth_db(...) must be called after the Step 2 DDL and before Step 3's \
             Projector::new(db.clone(), raw) — the post-DDL re-projection reuses the same `db` \
             handle for node upserts and is equally exposed to session expiry"
        );
    }

    /// Behavioural proof that `reauth_db` recovers when a SurrealDB HTTP session's token has
    /// GENUINELY, FULLY expired — not merely been `.invalidate()`d while still valid.
    ///
    /// This replaces `migrate_ddl_recovers_from_expired_session_via_reauth`, which was an
    /// artifact: it simulated expiry via `db.invalidate()`, but `.invalidate()` on a
    /// still-valid session **is** re-auth's own first step (clearing the stored credential),
    /// so that test measured `reauth_db` succeeding against a connection whose auth state the
    /// test itself had already cleared. It told us nothing about real expiry, and it passed
    /// against the pre-fix `reauth_db` (plain `db.signin()`, no fallback) even though that
    /// code cannot survive a real expiry.
    ///
    /// Real expiry is a different, and worse, failure mode: SurrealDB's HTTP transport
    /// attaches whatever bearer token a connection currently holds to *every* subsequent
    /// request from that connection — including signin, invalidate, and authenticate
    /// themselves — and the server rejects the request with 401 before it even looks at
    /// which RPC method was named. Proven directly against a live SurrealDB 3.1.0-beta.2
    /// instance: once a token is past its `DURATION FOR TOKEN`, `.invalidate()`, `.signin()`,
    /// and even `.authenticate(fresh_jwt_obtained_from_a_separate_connection)` all fail
    /// identically with `401 — the token has expired`. There is no request the SAME
    /// connection can send that recovers it; `reauth_db`'s fix is to detect the failed
    /// in-place `signin` and fall back to a brand-new connection (see its doc comment in
    /// `crates/cli/src/db.rs`).
    ///
    /// This test reproduces that exact condition with `DEFINE USER ... DURATION FOR TOKEN
    /// 1s` — a genuinely short-lived token, not a simulated one — waits past the TTL,
    /// confirms the session is actually dead, and only then exercises `reauth_db`.
    ///
    /// Two details matter for determinism under a loaded `cargo test --workspace` run,
    /// where 11-table DDL and other concurrent tests can make individual statements take
    /// well over a second:
    ///
    /// - The `AppConfig` handed to `reauth_db` carries the ADMIN (normal-TTL) root
    ///   credentials, not the 1s-TTL probe user's. This is also what production actually
    ///   does — `reauth_db` always re-signs in with `config.surreal.username/password`
    ///   (the long-lived root user), never a short-lived credential — so a fresh
    ///   connection it opens is never itself racing a 1-second clock. Only the ORIGINAL
    ///   `db` handle (signed in as the probe user) needs to genuinely expire; the
    ///   recovery path must not inherit that TTL.
    /// - The recovered connection is verified with a cheap `INFO FOR DB` rather than the
    ///   11-table `MIGRATION_DDL`. The contract under test is "the returned connection is
    ///   usable", which a cheap query proves just as well; MIGRATION_DDL's weight only
    ///   adds flakiness here (and is already independently exercised by
    ///   `located_at_accepts_contract_via_schema_def_and_migration_ddl` below).
    #[tokio::test]
    async fn reauth_db_recovers_from_a_genuinely_expired_token() {
        if !urban_integrity_test_support::db_tests_enabled() {
            return;
        }

        // ── Ephemeral ROOT-scoped user with a 1s token lifetime ──
        // reauth_db signs in via `surrealdb::opt::auth::Root`, so the short-lived credential
        // under test must itself be a ROOT user (not a namespace/database-scoped one).
        let username = format!("test_reauth_{}", uuid::Uuid::new_v4().simple());
        let password = "test-reauth-password";

        let (admin_user, admin_pass) = urban_integrity_test_support::credentials();
        let admin = surrealdb::engine::any::connect(urban_integrity_test_support::endpoint())
            .await
            .expect("SurrealDB unreachable");
        admin
            .signin(surrealdb::opt::auth::Root {
                username: admin_user.clone(),
                password: admin_pass.clone(),
            })
            .await
            .expect("admin sign-in failed");
        admin
            .query(format!(
                "DEFINE USER {username} ON ROOT PASSWORD '{password}' ROLES OWNER \
                 DURATION FOR TOKEN 1s"
            ))
            .await
            .expect("DEFINE USER transport failed")
            .check()
            .expect("DEFINE USER must apply cleanly");

        // Guard: always remove the ephemeral ROOT user, even if an assertion below panics.
        // Mirrors `NsGuard`'s pattern (own thread + runtime; Drop cannot be async).
        struct RootUserGuard(String);
        impl Drop for RootUserGuard {
            fn drop(&mut self) {
                let username = std::mem::take(&mut self.0);
                if username.is_empty() {
                    return;
                }
                let worker = std::thread::spawn(move || {
                    let rt = match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(rt) => rt,
                        Err(e) => {
                            eprintln!(
                                "[migrate tests] LEAKED root user {username}: runtime build \
                                 failed: {e}"
                            );
                            return;
                        }
                    };
                    rt.block_on(async move {
                        let (admin_user, admin_pass) = urban_integrity_test_support::credentials();
                        let Ok(db) = surrealdb::engine::any::connect(
                            urban_integrity_test_support::endpoint(),
                        )
                        .await
                        else {
                            eprintln!(
                                "[migrate tests] LEAKED root user {username}: connect failed"
                            );
                            return;
                        };
                        if db
                            .signin(surrealdb::opt::auth::Root {
                                username: admin_user,
                                password: admin_pass,
                            })
                            .await
                            .is_err()
                        {
                            eprintln!(
                                "[migrate tests] LEAKED root user {username}: admin sign-in \
                                 failed"
                            );
                            return;
                        }
                        if let Err(e) = db
                            .query(format!("REMOVE USER IF EXISTS {username} ON ROOT"))
                            .await
                        {
                            eprintln!("[migrate tests] LEAKED root user {username}: {e}");
                        }
                    });
                });
                let _ = worker.join();
            }
        }
        let _root_guard = RootUserGuard(username.clone());

        // ── Namespace/database isolation for the DDL itself ──
        let ns = urban_integrity_test_support::NsGuard::new();
        let db_name = "integrity";

        // Sign in AS the ephemeral short-lived user, matching connect_to_db's flow.
        let db: surrealdb::Surreal<surrealdb::engine::any::Any> =
            surrealdb::engine::any::connect(urban_integrity_test_support::endpoint())
                .await
                .expect("SurrealDB unreachable");
        db.signin(surrealdb::opt::auth::Root {
            username: username.clone(),
            password: password.to_string(),
        })
        .await
        .expect("initial sign-in with the short-lived user failed");
        db.use_ns(ns.name())
            .use_db(db_name)
            .await
            .expect("namespace/database selection failed");

        // Wait past the 1s token TTL.
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        // Confirm the token has genuinely expired — a query now fails outright, not just
        // returns an application-level error.
        let expired = match db.query("INFO FOR DB").await {
            Err(_) => true,
            Ok(resp) => resp.check().is_err(),
        };
        assert!(
            expired,
            "expected a query against a token past its 1s DURATION FOR TOKEN to fail — if \
             this now succeeds, SurrealDB's token expiry enforcement changed and this \
             regression test's premise no longer holds"
        );

        // `reauth_db` only ever reads `config.surreal.*`, and in production it always
        // re-signs in with the long-lived root credentials from config — never a
        // short-lived probe user's. Use the ADMIN credentials here, not `username`/
        // `password` (the 1s-TTL user signed into the original `db` above): the fresh
        // connection `reauth_db` opens must not inherit a 1-second clock of its own, or
        // verifying it below races the same expiry this test is trying to prove recovery
        // from.
        let config = urban_integrity_core::config::AppConfig {
            analysis: None,
            surreal: urban_integrity_core::config::SurrealConfig {
                ws_url: urban_integrity_test_support::endpoint(),
                namespace: ns.name().to_string(),
                database: db_name.to_string(),
                username: Some(admin_user),
                password: Some(admin_pass),
            },
            graph: Default::default(),
            ingest: Default::default(),
            detector: Default::default(),
            enricher: Default::default(),
            jurisdiction: Default::default(),
            workbench: Default::default(),
        };

        // The fix under test: because the in-place signin on `db` is itself rejected (the
        // token is genuinely expired, not merely due for a proactive refresh), reauth_db
        // must fall back to a brand-new connection rather than propagating the 401.
        let recovered = reauth_db(&db, &config, db_name)
            .await
            .expect("reauth_db must recover from a genuinely expired token")
            .expect(
                "reauth_db must return Some(new_db) when in-place re-signin was rejected — \
                 None here would mean it silently believed the poisoned `db` handle was fine",
            );

        // The replacement connection actually works. A cheap statement is enough to prove
        // the contract ("the returned connection is usable") — MIGRATION_DDL's 11-table
        // REMOVE+DEFINE is exercised independently by
        // `located_at_accepts_contract_via_schema_def_and_migration_ddl` below, and using
        // it here only adds runtime without adding coverage of `reauth_db` itself.
        recovered
            .query("INFO FOR DB")
            .await
            .expect("INFO FOR DB transport failed against the recovered connection")
            .check()
            .expect("INFO FOR DB must apply cleanly on the connection reauth_db returned");

        // The original, poisoned `db` handle remains unusable — this is the documented
        // contract (no in-place recovery is possible once a token has genuinely expired),
        // not a bug.
        let original_still_broken = match db.query("INFO FOR DB").await {
            Err(_) => true,
            Ok(resp) => resp.check().is_err(),
        };
        assert!(
            original_still_broken,
            "the original `db` handle should remain unusable after a genuine expiry — \
             reauth_db's contract is that callers must switch to the returned connection"
        );

        // No manual cleanup for `ns`/the ephemeral user: `ns` (NsGuard) removes the
        // throwaway namespace on drop, and `_root_guard` removes the ephemeral ROOT user.
    }

    /// D-03/D-23 (URBAN_DB_TESTS=1): `located_at` must accept a `contract` on its `in` side,
    /// via BOTH the fresh-`SCHEMA_DEF` path and the `MIGRATION_DDL` path, and must still accept
    /// `entity` — the widening is additive, not a replacement.
    #[tokio::test]
    async fn located_at_accepts_contract_via_schema_def_and_migration_ddl() {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return;
        }

        // TestNs::connect applies SCHEMA_DEF, which already carries the widened
        // `located_at TYPE RELATION FROM entity | contract TO location` (Task 1).
        let db = urban_integrity_test_support::TestNs::connect("integrity").await;

        let entity_key = "test-migrate-located-at-entity";
        let contract_key = "test-migrate-located-at-contract";
        let location_key = "test-migrate-located-at-location";

        db.query("UPSERT type::record('entity', $k) CONTENT $r")
            .bind(("k", entity_key))
            .bind((
                "r",
                serde_json::json!({"name": "Test Entity", "entity_id": "entity:test-migrate-located-at-entity"}),
            ))
            .await
            .unwrap()
            .check()
            .unwrap();
        db.query("UPSERT type::record('contract', $k) CONTENT $r")
            .bind(("k", contract_key))
            .bind((
                "r",
                serde_json::json!({"name": "Test Contract", "entity_id": "contract:test-migrate-located-at-contract"}),
            ))
            .await
            .unwrap()
            .check()
            .unwrap();
        db.query("UPSERT type::record('location', $k) CONTENT $r")
            .bind(("k", location_key))
            .bind((
                "r",
                serde_json::json!({"name": "Test Location", "entity_id": "location:test-migrate-located-at-location", "postcode": "AB1 2CD"}),
            ))
            .await
            .unwrap()
            .check()
            .unwrap();

        // ── Test 1: a fresh SCHEMA_DEF-provisioned namespace accepts contract->located_at->location.
        // located_at requires meta.created_at (datetime) and data.details (string) — both
        // non-optional — so a bare RELATE without them fails schema coercion regardless of the
        // `in` type; SET both here, matching the shape the real projector writes.
        db.query(
            "LET $c = type::record('contract', $ck); \
             LET $l = type::record('location', $lk); \
             RELATE $c->located_at->$l SET meta.created_at = time::now(), data.details = 'test-fixture'",
        )
        .bind(("ck", contract_key))
        .bind(("lk", location_key))
        .await
        .unwrap()
        .check()
        .expect("SCHEMA_DEF path: contract must be a legal `in` value for located_at");

        let rows: Vec<serde_json::Value> = db
            .query("SELECT count() FROM located_at WHERE in = type::record('contract', $ck) AND out = type::record('location', $lk) GROUP ALL")
            .bind(("ck", contract_key))
            .bind(("lk", location_key))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let count = rows.first().and_then(|r| r["count"].as_i64()).unwrap_or(0);
        assert_eq!(
            count, 1,
            "SCHEMA_DEF path: expected exactly 1 contract->located_at->location relation"
        );

        // ── Test 2: re-apply MIGRATION_DDL (REMOVE + redefine) in the SAME namespace, proving
        // the MIGRATION_DDL text — not just SCHEMA_DEF — independently accepts `contract`.
        // REMOVE TABLE wipes the row created in Test 1; re-create it after.
        db.query(MIGRATION_DDL)
            .await
            .unwrap()
            .check()
            .expect("MIGRATION_DDL must apply cleanly");

        db.query(
            "LET $c = type::record('contract', $ck); \
             LET $l = type::record('location', $lk); \
             RELATE $c->located_at->$l SET meta.created_at = time::now(), data.details = 'test-fixture'",
        )
        .bind(("ck", contract_key))
        .bind(("lk", location_key))
        .await
        .unwrap()
        .check()
        .expect("MIGRATION_DDL path: contract must be a legal `in` value for located_at");

        let rows: Vec<serde_json::Value> = db
            .query("SELECT count() FROM located_at WHERE in = type::record('contract', $ck) AND out = type::record('location', $lk) GROUP ALL")
            .bind(("ck", contract_key))
            .bind(("lk", location_key))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let count = rows.first().and_then(|r| r["count"].as_i64()).unwrap_or(0);
        assert_eq!(
            count, 1,
            "MIGRATION_DDL path: expected exactly 1 contract->located_at->location relation"
        );

        // ── Test 3 (non-regression): entity->located_at->location must still succeed — the
        // widening is additive (entity | contract), not a replacement.
        db.query(
            "LET $e = type::record('entity', $ek); \
             LET $l = type::record('location', $lk); \
             RELATE $e->located_at->$l SET meta.created_at = time::now(), data.details = 'test-fixture'",
        )
        .bind(("ek", entity_key))
        .bind(("lk", location_key))
        .await
        .unwrap()
        .check()
        .expect("entity must still be a legal `in` value for located_at (non-regression)");

        let rows: Vec<serde_json::Value> = db
            .query("SELECT count() FROM located_at WHERE in = type::record('entity', $ek) AND out = type::record('location', $lk) GROUP ALL")
            .bind(("ek", entity_key))
            .bind(("lk", location_key))
            .await
            .unwrap()
            .take(0)
            .unwrap();
        let count = rows.first().and_then(|r| r["count"].as_i64()).unwrap_or(0);
        assert_eq!(
            count, 1,
            "non-regression: expected exactly 1 entity->located_at->location relation"
        );

        // ── Cleanup ──
        let _ = db
            .query("DELETE located_at WHERE in = type::record('contract', $ck) OR in = type::record('entity', $ek)")
            .bind(("ck", contract_key))
            .bind(("ek", entity_key))
            .await;
        let _ = db
            .query("DELETE type::record('entity', $k)")
            .bind(("k", entity_key))
            .await;
        let _ = db
            .query("DELETE type::record('contract', $k)")
            .bind(("k", contract_key))
            .await;
        let _ = db
            .query("DELETE type::record('location', $k)")
            .bind(("k", location_key))
            .await;
    }
}
