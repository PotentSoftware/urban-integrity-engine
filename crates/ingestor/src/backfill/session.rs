//! A SurrealDB handle that survives the ~1-hour HTTP session expiry.
//!
//! Every backfill is a long-running, single-connection job: `backfill ch` walks ~20,000
//! Companies House lookups at 2 requests/second, so a full pass is multiple hours. SurrealDB's
//! HTTP session token expires after roughly an hour, at which point *every* subsequent query
//! fails with `401 Unauthorized for url (http://localhost:8000/rpc)`.
//!
//! Measured live 2026-07-28 against `an_anglesey_aebaa785`: a `backfill ch` run died at ~1h with
//! that 401 having linked 354 of 20,660 candidates, and — because the backfill had no resume
//! cursor either — a restart would have redone all 354 lookups. `serve` already handled this
//! (`crates/cli/src/main.rs`, inline re-auth plus a 50-minute periodic refresh); the backfill
//! path did not.
//!
//! [`DbSession`] is the one choke point every backfill query goes through. It re-signs in and
//! retries once when a query is rejected for an auth reason, and can run a periodic refresh so
//! the reactive path is rarely needed at all.

use anyhow::{Context, Result};
use std::sync::Arc;
use surrealdb::{engine::any::Any, opt::auth::Root, Surreal};

/// How often the proactive refresher re-signs in. Comfortably inside the ~60-minute token
/// lifetime, and the same interval `serve` uses.
pub const REAUTH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(50 * 60);

/// Everything needed to re-establish a session on an existing connection.
#[derive(Clone, Debug)]
pub struct SessionCreds {
    pub username: String,
    pub password: String,
    pub namespace: String,
    /// The database the session must be returned to.
    ///
    /// This is the name `--analysis` resolved to, **not** `config.surreal.database`. Signing in
    /// again resets the connection's namespace/database selection, so re-auth has to restore the
    /// same target the run started against — reading the raw config field here would silently
    /// move a mid-flight backfill onto a different analysis, which is exactly the split-brain
    /// defect fixed in `project` (PR #31).
    pub database: String,
}

/// A shared SurrealDB handle plus the credentials to revive its session.
#[derive(Clone)]
pub struct DbSession {
    db: Arc<Surreal<Any>>,
    creds: Option<SessionCreds>,
}

impl DbSession {
    /// A session that can re-authenticate itself.
    pub fn new(db: Arc<Surreal<Any>>, creds: SessionCreds) -> Self {
        Self {
            db,
            creds: Some(creds),
        }
    }

    /// A session with no credentials: auth failures are reported, not retried.
    ///
    /// Used by tests and by any caller running against a SurrealDB with authentication disabled,
    /// where there is nothing to sign back in with.
    pub fn without_reauth(db: Arc<Surreal<Any>>) -> Self {
        Self { db, creds: None }
    }

    /// The underlying handle, for the few call sites that need the raw SDK (tests, `Response`
    /// consumption helpers). Queries issued through it do **not** get re-auth.
    pub fn handle(&self) -> &Surreal<Any> {
        &self.db
    }

    pub fn arc(&self) -> Arc<Surreal<Any>> {
        self.db.clone()
    }

    /// Run a query, re-authenticating and retrying **once** if the session has expired.
    ///
    /// Bindings are passed as JSON values rather than applied by the caller so that the retry can
    /// rebuild the query from scratch — a `Query` builder is consumed by `.await`, so a retry has
    /// to have the ingredients, not the half-built statement.
    pub async fn query(
        &self,
        sql: &str,
        binds: &[(&'static str, serde_json::Value)],
    ) -> Result<surrealdb::IndexedResults> {
        match self.attempt(sql, binds).await {
            Ok(response) => Ok(response),
            Err(e) if is_auth_error(&e) => {
                tracing::warn!(
                    error = %e,
                    "SurrealDB rejected a query for an auth reason — re-authenticating and retrying once"
                );
                self.reauth().await?;
                self.attempt(sql, binds)
                    .await
                    .context("SurrealDB query failed again after re-authentication")
            }
            Err(e) => Err(anyhow::Error::new(e).context("SurrealDB query failed")),
        }
    }

    async fn attempt(
        &self,
        sql: &str,
        binds: &[(&'static str, serde_json::Value)],
    ) -> surrealdb::Result<surrealdb::IndexedResults> {
        let mut query = self.db.query(sql.to_owned());
        for (key, value) in binds {
            query = query.bind((*key, value.clone()));
        }
        // `.check()` surfaces per-statement errors here rather than at the caller's `.take()`.
        // The expiry can present either way depending on where the server rejects the request,
        // and both need to reach the retry above.
        query.await?.check()
    }

    /// Sign in again and reselect the namespace/database.
    ///
    /// Errors if the session was built without credentials — there is nothing to retry with, and
    /// silently continuing would leave the caller looping on 401s.
    pub async fn reauth(&self) -> Result<()> {
        let creds = self.creds.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "SurrealDB session expired and no credentials were supplied to re-authenticate \
                 — set surreal.username/surreal.password (or URBAN__SURREAL__USERNAME / \
                 URBAN__SURREAL__PASSWORD) and re-run"
            )
        })?;

        self.db
            .signin(Root {
                username: creds.username.clone(),
                password: creds.password.clone(),
            })
            .await
            .context("SurrealDB re-signin failed")?;

        self.db
            .use_ns(&creds.namespace)
            .use_db(&creds.database)
            .await
            .with_context(|| {
                format!(
                    "SurrealDB namespace/db re-selection failed: {}/{}",
                    creds.namespace, creds.database
                )
            })?;

        tracing::info!(
            namespace = %creds.namespace,
            database = %creds.database,
            "SurrealDB session re-authenticated"
        );
        Ok(())
    }

    /// Whether this session can re-authenticate at all.
    pub fn can_reauth(&self) -> bool {
        self.creds.is_some()
    }

    /// Refresh the session every [`REAUTH_INTERVAL`] until the returned guard is dropped.
    ///
    /// Proactive, so the reactive path in [`DbSession::query`] stays a backstop rather than the
    /// normal course of events: a token that is never allowed to expire cannot fail a query at an
    /// awkward moment (mid-write, between the two events a planning insert has to emit as a pair).
    pub fn spawn_periodic_reauth(&self) -> ReauthGuard {
        if !self.can_reauth() {
            tracing::debug!("no SurrealDB credentials — periodic re-auth not started");
            return ReauthGuard { handle: None };
        }
        let session = self.clone();
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(REAUTH_INTERVAL);
            interval.tick().await; // the first tick fires immediately; the session is fresh
            loop {
                interval.tick().await;
                if let Err(e) = session.reauth().await {
                    // Not fatal: the next query's inline retry gets another attempt.
                    tracing::warn!(error = %e, "SurrealDB periodic re-auth failed");
                }
            }
        });
        ReauthGuard {
            handle: Some(handle),
        }
    }
}

/// Stops the periodic re-auth task when dropped, so a finished command does not leave a timer
/// running against a connection nobody is using.
pub struct ReauthGuard {
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for ReauthGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

/// Does this error mean "the session is no longer authenticated" rather than "the query is wrong"?
///
/// Classified by string because the SDK folds the transport 401 into an opaque
/// `Api(Http(..))`/`Api(Query(..))` payload whose only distinguishing content is the message. The
/// live failure reads `401 Unauthorized for url (http://localhost:8000/rpc)`; the server-side
/// rejections read `There was a problem with authentication` and `Not enough permissions`.
///
/// Deliberately narrow: matching too broadly would turn a genuine permission error into an
/// infinite-looking retry, and a false negative merely restores today's behaviour of failing the
/// run.
pub fn is_auth_error(err: &surrealdb::Error) -> bool {
    message_is_auth_failure(&err.to_string())
}

/// The string test behind [`is_auth_error`], split out so it can be tested directly —
/// `surrealdb::Error` has no public constructor for an arbitrary message.
pub(crate) fn message_is_auth_failure(message: &str) -> bool {
    let msg = message.to_ascii_lowercase();
    msg.contains("401")
        || msg.contains("unauthorized")
        || msg.contains("not enough permissions")
        || msg.contains("problem with authentication")
        || msg.contains("invalid credentials")
        || msg.contains("token has expired")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classifier is what decides whether a run recovers or dies, so pin both directions.
    #[test]
    fn classifies_the_live_401_as_an_auth_error() {
        // The exact string the 2026-07-28 run died on.
        assert!(message_is_auth_failure(
            "401 Unauthorized for url (http://localhost:8000/rpc)"
        ));
    }

    #[test]
    fn classifies_server_side_auth_rejections() {
        assert!(message_is_auth_failure(
            "There was a problem with authentication"
        ));
        assert!(message_is_auth_failure(
            "IAM error: Not enough permissions to perform this action"
        ));
        assert!(message_is_auth_failure("The token has expired"));
    }

    #[test]
    fn does_not_classify_ordinary_query_failures_as_auth_errors() {
        // Re-auth must not be attempted for these — retrying a malformed statement or a
        // contended transaction as if the token were stale hides the real fault.
        assert!(!message_is_auth_failure(
            "Transaction conflict: Write conflict, retry the transaction"
        ));
        assert!(!message_is_auth_failure("Parse error: Unexpected token"));
        assert!(!message_is_auth_failure(
            "Found NONE for field `payload.entity_id`, with record `graph_events:abc`"
        ));
        // A 400 is a bad request, not a stale session.
        assert!(!message_is_auth_failure("400 Bad Request"));
    }

    // ── the recovery path, against a real server ─────────────────────────────────────────────
    //
    // The whole point of this module is behaviour no in-memory fake has: a session whose
    // credentials the server has stopped honouring. `invalidate()` puts the connection in exactly
    // that state — the same one the ~1-hour token expiry produces — so this is the closest thing
    // to a reproduction of the 2026-07-28 failure that can run in CI.
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
        let guard = urban_integrity_test_support::NsGuard::with_prefix("test_backfill_session");
        let db: Surreal<Any> = Surreal::init();
        db.connect("http://localhost:8000").await.expect("connect");
        db.signin(Root {
            username: "root".to_string(),
            password: "root".to_string(),
        })
        .await
        .expect("signin");
        let namespace = guard.name().to_string();
        let database = format!("session_{suffix}");
        crate::backfill::use_ns_db_with_retry(&db, &namespace, &database).await;
        db.query("DEFINE TABLE IF NOT EXISTS graph_events SCHEMALESS")
            .await
            .expect("define table");
        let session = DbSession::new(
            Arc::new(db),
            SessionCreds {
                username: "root".to_string(),
                password: "root".to_string(),
                namespace,
                database,
            },
        );
        (session, guard)
    }

    const PROBE: &str = "SELECT id FROM graph_events LIMIT 1";

    #[tokio::test]
    async fn db_query_recovers_from_a_dead_session() {
        if !db_tests_enabled() {
            eprintln!("skipping: URBAN_DB_TESTS != 1 or no SurrealDB on :8000");
            return;
        }
        let (session, _ns) = connect_scratch_session("recovers").await;
        session.query(PROBE, &[]).await.expect("baseline query");

        // Revoke the session the way expiry does.
        session
            .handle()
            .invalidate()
            .await
            .expect("invalidate the session");

        // Establish that the invalidation actually bites — otherwise the assertion below would
        // pass for the wrong reason, which is the failure mode this whole change exists to stop.
        let raw = session
            .handle()
            .query(PROBE)
            .await
            .and_then(surrealdb::IndexedResults::check);
        assert!(
            raw.is_err(),
            "invalidate() left the connection usable — this test proves nothing about re-auth"
        );
        let raw_err = raw.expect_err("checked above").to_string();
        assert!(
            is_auth_error_message(&raw_err),
            "the failure a dead session produces is not classified as an auth error, so \
             DbSession::query would never retry it: {raw_err}"
        );

        // The managed path re-signs in and retries, so the caller never sees the expiry.
        session
            .query(PROBE, &[])
            .await
            .expect("query through DbSession must survive an invalidated session");
    }

    /// Same classifier the retry uses, reachable from the test above.
    fn is_auth_error_message(message: &str) -> bool {
        message_is_auth_failure(message)
    }

    #[tokio::test]
    async fn reauth_without_credentials_is_an_error_not_a_silent_noop() {
        let db: Surreal<Any> = Surreal::init();
        let session = DbSession::without_reauth(Arc::new(db));
        assert!(!session.can_reauth());
        let err = session.reauth().await.unwrap_err();
        assert!(
            err.to_string().contains("no credentials"),
            "unexpected message: {err}"
        );
    }
}
