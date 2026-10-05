//! Resume cursors for backfills whose cost is external, not internal.
//!
//! Most backfill passes are naturally re-entrant: the ward pass only selects Contracts with no
//! `ward_code`, so a stamped Contract drops out of the candidate set and a re-run does not redo
//! it. `backfill ch` is not, and that asymmetry is the whole reason this module exists — its
//! candidate set is "director entities with no `company_number`", and the two *negative* verdicts
//! (`NoMatch`, `Ambiguous`) leave the entity exactly as it was. Live 2026-07-28, 20,306 of 20,660
//! candidates ended in one of those, so a restart re-issues 20,306 Companies House lookups at
//! 2 requests/second — roughly three hours of API budget spent re-learning nothing.
//!
//! The cursor records how far the ordered candidate list was walked. Combined with
//! [`super::session::DbSession`]'s re-auth, a run that dies mid-flight — the ~1-hour session
//! expiry, a lost network, a Ctrl-C — resumes rather than restarts.
//!
//! Deliberately *not* a general checkpoint: it stores a position and running totals, nothing that
//! could be mistaken for graph data. The graph itself remains the source of truth, and deleting a
//! cursor row only ever costs repeated work, never correctness.

use anyhow::{Context, Result};
use serde_json::json;

use super::session::DbSession;

/// The table holding one row per backfill job (`backfill_cursors:ch`).
pub const CURSOR_TABLE: &str = "backfill_cursors";

/// A saved position in an ordered candidate list, plus the totals accumulated so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CursorState {
    /// SurrealDB record id of the last candidate that was fully processed.
    pub last_record_id: String,
    pub processed: usize,
    pub matched: usize,
    pub skipped: usize,
    pub errors: usize,
}

/// Reads and writes one job's cursor row.
pub struct ResumeCursor {
    session: DbSession,
    job: String,
}

impl ResumeCursor {
    pub fn new(session: DbSession, job: impl Into<String>) -> Self {
        Self {
            session,
            job: job.into(),
        }
    }

    pub fn job(&self) -> &str {
        &self.job
    }

    /// The saved position, or `None` if this job has never been interrupted.
    pub async fn load(&self) -> Result<Option<CursorState>> {
        let mut res = self
            .session
            .query(
                "SELECT last_record_id, processed, matched, skipped, errors \
                 FROM type::record($tb, $job)",
                &[("tb", json!(CURSOR_TABLE)), ("job", json!(self.job))],
            )
            .await
            .with_context(|| format!("loading backfill cursor for job '{}'", self.job))?;
        let rows: Vec<serde_json::Value> = res.take(0)?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        // A row with no `last_record_id` is meaningless as a position — treat it as absent
        // rather than resuming from an empty string, which would skip nothing and read as a
        // successful resume.
        let Some(last_record_id) = row["last_record_id"].as_str().filter(|s| !s.is_empty()) else {
            tracing::warn!(
                job = %self.job,
                "backfill cursor row has no last_record_id — starting from the beginning"
            );
            return Ok(None);
        };
        Ok(Some(CursorState {
            last_record_id: last_record_id.to_owned(),
            processed: row["processed"].as_u64().unwrap_or(0) as usize,
            matched: row["matched"].as_u64().unwrap_or(0) as usize,
            skipped: row["skipped"].as_u64().unwrap_or(0) as usize,
            errors: row["errors"].as_u64().unwrap_or(0) as usize,
        }))
    }

    /// Record the position after a candidate has been fully processed.
    ///
    /// Written after *every* candidate rather than every N: at 2 requests/second the extra
    /// primary-key upsert is noise next to the 500 ms rate-limit sleep, and batching it would
    /// mean a crash loses up to N external lookups — the exact cost this module exists to avoid.
    pub async fn save(&self, state: &CursorState) -> Result<()> {
        self.session
            .query(
                "UPSERT type::record($tb, $job) SET \
                   last_record_id = $last, processed = $processed, matched = $matched, \
                   skipped = $skipped, errors = $errors, updated_at = time::now()",
                &[
                    ("tb", json!(CURSOR_TABLE)),
                    ("job", json!(self.job)),
                    ("last", json!(state.last_record_id)),
                    ("processed", json!(state.processed)),
                    ("matched", json!(state.matched)),
                    ("skipped", json!(state.skipped)),
                    ("errors", json!(state.errors)),
                ],
            )
            .await
            .with_context(|| format!("saving backfill cursor for job '{}'", self.job))?;
        Ok(())
    }

    /// Drop the cursor — on clean completion, or when `--restart` asks for a full pass.
    pub async fn clear(&self) -> Result<()> {
        self.session
            .query(
                "DELETE type::record($tb, $job)",
                &[("tb", json!(CURSOR_TABLE)), ("job", json!(self.job))],
            )
            .await
            .with_context(|| format!("clearing backfill cursor for job '{}'", self.job))?;
        Ok(())
    }
}

/// Index of the first candidate still to be processed, given a saved position.
///
/// The candidate list arrives in `ORDER BY id` order (see `scan_events_by_type`), so resuming is
/// a matter of finding where `last_record_id` sits in it. Two lookups, in order:
///
/// 1. **Exact position.** The usual case: the cursor row is still a candidate — a `NoMatch` or
///    `Ambiguous` verdict leaves the entity untouched, so it stays eligible.
/// 2. **Order fallback.** If the last candidate processed was a *match*, it now carries a
///    `company_number` and has dropped out of the candidate set entirely, so there is no exact
///    position to find. Count the candidates that sort at or before it instead.
///
/// The fallback relies on the scan's ordering agreeing with Rust's string ordering. It does for
/// these ids — SurrealDB orders string record ids lexicographically and the generated ids are
/// lowercase alphanumeric — and `db_scan_returns_ids_in_rust_string_order` in `backfill::mod`
/// pins that. A disagreement would resume from a slightly wrong offset, costing or skipping some
/// repeated lookups; it cannot corrupt anything, because every write is idempotent.
pub fn resume_index<T>(items: &[T], last_record_id: &str, key: impl Fn(&T) -> &str) -> usize {
    if let Some(pos) = items.iter().position(|item| key(item) == last_record_id) {
        return pos + 1;
    }
    items
        .iter()
        .take_while(|item| key(item) <= last_record_id)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn resumes_after_an_exact_match() {
        let items = ids(&["a1", "b2", "c3", "d4"]);
        assert_eq!(resume_index(&items, "b2", |s| s.as_str()), 2);
    }

    #[test]
    fn resumes_after_a_candidate_that_left_the_set() {
        // "b2" matched on the previous run, so it no longer appears. Everything at or before it
        // is done; "c3" is next.
        let items = ids(&["a1", "c3", "d4"]);
        assert_eq!(resume_index(&items, "b2", |s| s.as_str()), 1);
    }

    #[test]
    fn a_cursor_at_the_last_item_leaves_nothing_to_do() {
        let items = ids(&["a1", "b2", "c3"]);
        assert_eq!(resume_index(&items, "c3", |s| s.as_str()), 3);
    }

    #[test]
    fn a_cursor_beyond_every_candidate_leaves_nothing_to_do() {
        // Every remaining candidate sorts before the cursor — e.g. the tail of the previous run
        // all matched and dropped out.
        let items = ids(&["a1", "b2"]);
        assert_eq!(resume_index(&items, "z9", |s| s.as_str()), 2);
    }

    #[test]
    fn a_cursor_before_every_candidate_starts_from_the_top() {
        let items = ids(&["b2", "c3"]);
        assert_eq!(resume_index(&items, "a1", |s| s.as_str()), 0);
    }

    #[test]
    fn an_empty_candidate_list_is_handled() {
        let items: Vec<String> = Vec::new();
        assert_eq!(resume_index(&items, "a1", |s| s.as_str()), 0);
    }

    // ── round-trip against a real server ─────────────────────────────────────────────────────
    //
    // The SurrealQL above is the part unit tests cannot reach, and this project has shipped
    // adapters whose SurrealQL never once ran against a database (see the ICIJ silent-write
    // failure). A cursor that fails to persist would be invisible: the run completes, and only a
    // later interruption reveals there was nothing to resume from.
    use super::super::session::{DbSession, SessionCreds};
    use surrealdb::{engine::any::Any, opt::auth::Root, Surreal};

    fn db_tests_enabled() -> bool {
        if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
            return false;
        }
        std::net::TcpStream::connect("[::1]:8000").is_ok()
            || std::net::TcpStream::connect("127.0.0.1:8000").is_ok()
    }

    /// Returns the session plus the namespace guard; callers must bind the
    /// guard so the namespace outlives the test.
    async fn connect_scratch_session(
        suffix: &str,
    ) -> (DbSession, urban_integrity_test_support::NsGuard) {
        let guard = urban_integrity_test_support::NsGuard::with_prefix("test_backfill_cursor");
        let db: Surreal<Any> = Surreal::init();
        db.connect("http://localhost:8000").await.expect("connect");
        db.signin(Root {
            username: "root".to_string(),
            password: "root".to_string(),
        })
        .await
        .expect("signin");
        let namespace = guard.name().to_string();
        let database = format!("cursor_{suffix}");
        crate::backfill::use_ns_db_with_retry(&db, &namespace, &database).await;
        db.query(format!(
            "DEFINE TABLE IF NOT EXISTS {CURSOR_TABLE} SCHEMALESS"
        ))
        .await
        .expect("define cursor table");
        let session = DbSession::new(
            std::sync::Arc::new(db),
            SessionCreds {
                username: "root".to_string(),
                password: "root".to_string(),
                namespace,
                database,
            },
        );
        (session, guard)
    }

    #[tokio::test]
    async fn db_cursor_round_trips_and_clears() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let (session, _ns) = connect_scratch_session("round_trip").await;
        let cursor = ResumeCursor::new(session, "roundtrip");
        cursor.clear().await.expect("start from a clean slate");

        assert_eq!(
            cursor.load().await.expect("load"),
            None,
            "a job that has never been interrupted must have no cursor"
        );

        let state = CursorState {
            last_record_id: "graph_events:7hcaah36syb6inbwur0d".to_string(),
            processed: 354,
            matched: 12,
            skipped: 340,
            errors: 2,
        };
        cursor.save(&state).await.expect("save");
        assert_eq!(
            cursor.load().await.expect("load"),
            Some(state.clone()),
            "every field must survive the round trip — the totals are what make a resumed run's \
             final numbers describe the whole job"
        );

        // Saving again must overwrite, not accumulate rows.
        let later = CursorState {
            last_record_id: "graph_events:8zzzzz36syb6inbwur0d".to_string(),
            processed: 355,
            ..state
        };
        cursor.save(&later).await.expect("re-save");
        assert_eq!(cursor.load().await.expect("load"), Some(later));

        cursor.clear().await.expect("clear");
        assert_eq!(
            cursor.load().await.expect("load"),
            None,
            "a completed run must leave no cursor behind, or the next run resumes past the end"
        );
    }

    #[tokio::test]
    async fn db_cursors_for_different_jobs_do_not_collide() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let (session, _ns) = connect_scratch_session("isolation").await;
        let ch = ResumeCursor::new(session.clone(), "ch");
        let other = ResumeCursor::new(session, "ward");
        ch.clear().await.expect("clean");
        other.clear().await.expect("clean");

        ch.save(&CursorState {
            last_record_id: "graph_events:aaa".to_string(),
            processed: 1,
            ..Default::default()
        })
        .await
        .expect("save ch");

        assert!(
            other.load().await.expect("load ward").is_none(),
            "one job's cursor must not be readable as another's"
        );
        assert!(ch.load().await.expect("load ch").is_some());
    }
}
