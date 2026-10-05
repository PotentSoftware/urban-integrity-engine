//! Shared SurrealDB test harness.
//!
//! Every DB-gated test runs in its own throwaway namespace so tests never see
//! each other's rows or production data. Before this crate existed each test
//! file rolled its own `format!("test_{uuid}")` and nothing ever removed it —
//! a full `URBAN_DB_TESTS=1 cargo test --workspace` leaked a few hundred
//! namespaces per run, and 2,889 had accumulated in the local store by
//! 2026-07-31.
//!
//! [`NsGuard`] closes that hole: it owns the namespace name and issues
//! `REMOVE NAMESPACE` on drop. [`TestNs`] bundles a connected, schema-applied
//! handle with that guard and derefs to `Surreal<Any>`, so a test that holds
//! the `TestNs` keeps its namespace alive for exactly as long as it needs it.
//!
//! ```ignore
//! let db = TestNs::connect("integrity").await;   // namespace created
//! db.query("SELECT * FROM entity").await?;       // via Deref
//! // namespace removed here, when `db` drops
//! ```
//!
//! Cleanup runs on normal return and on unwinding panics (including failed
//! assertions). It cannot run if the process is killed or aborts — use
//! `scripts/surreal-drop-test-ns.sh` to sweep up after those.

pub mod events;
pub mod planted;

use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use surrealdb::engine::any::Any;
use surrealdb::opt::auth::Root;
use surrealdb::Surreal;

/// How long a single teardown may block the dropping thread.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// SurrealDB endpoint used by tests. Override with `URBAN_TEST_DB_URL`.
pub fn endpoint() -> String {
    std::env::var("URBAN_TEST_DB_URL").unwrap_or_else(|_| "http://localhost:8000".to_string())
}

/// Root credentials, matching the local dev / CI instance.
pub fn credentials() -> (String, String) {
    (
        std::env::var("URBAN__SURREAL__USERNAME").unwrap_or_else(|_| "root".to_string()),
        std::env::var("URBAN__SURREAL__PASSWORD").unwrap_or_else(|_| "root".to_string()),
    )
}

/// `true` when `URBAN_DB_TESTS=1` **and** SurrealDB is actually listening.
///
/// The TCP pre-flight means a missing DB skips the test rather than panicking
/// halfway through it.
pub fn db_tests_enabled() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        return false;
    }
    std::net::TcpStream::connect("localhost:8000").is_ok()
}

/// Owns a throwaway namespace name and removes the namespace when dropped.
///
/// Use this directly when the test drives SurrealDB through something other
/// than a `Surreal<Any>` handle — a spawned CLI, a raw HTTP config — and so
/// only needs the name. Otherwise prefer [`TestNs`].
#[derive(Debug)]
pub struct NsGuard {
    ns: String,
}

impl NsGuard {
    /// Reserve a unique namespace named `test_{uuid}`.
    ///
    /// Reserving is name-only; the namespace springs into existence when
    /// something first selects it. Dropping an unused guard is harmless
    /// (`REMOVE NAMESPACE IF EXISTS`).
    pub fn new() -> Self {
        Self::with_prefix("test")
    }

    /// As [`NsGuard::new`], with a custom prefix — useful when scanning the
    /// live DB and wanting to tell which suite left something behind.
    ///
    /// The prefix must start with `test` so the sweeper script and every
    /// "is this debris?" check stay a simple prefix match.
    pub fn with_prefix(prefix: &str) -> Self {
        assert!(
            prefix.starts_with("test"),
            "test namespace prefix must start with `test`, got {prefix:?}"
        );
        Self {
            ns: format!("{prefix}_{}", uuid::Uuid::new_v4().simple()),
        }
    }

    /// The namespace name.
    pub fn name(&self) -> &str {
        &self.ns
    }
}

impl Default for NsGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for NsGuard {
    fn drop(&mut self) {
        let ns = std::mem::take(&mut self.ns);
        if ns.is_empty() {
            return;
        }

        // Drop is synchronous and usually runs on a Tokio worker thread, where
        // `Runtime::block_on` panics. Teardown therefore gets its own OS thread
        // with its own current-thread runtime.
        let worker = std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("[test-support] LEAKED namespace {ns}: runtime build failed: {e}");
                    return;
                }
            };
            rt.block_on(async move {
                match tokio::time::timeout(TEARDOWN_TIMEOUT, remove_namespace(&ns)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => eprintln!("[test-support] LEAKED namespace {ns}: {e}"),
                    Err(_) => eprintln!(
                        "[test-support] LEAKED namespace {ns}: teardown timed out after {}s",
                        TEARDOWN_TIMEOUT.as_secs()
                    ),
                }
            });
        });

        // Join so teardown finishes before the test binary can exit out from
        // under it. Only a panicking worker can fail this, and it already logged.
        let _ = worker.join();
    }
}

/// Issue `REMOVE NAMESPACE` over a fresh connection.
///
/// Every error is propagated: a silently-swallowed write is exactly how the
/// ICIJ adapter wrote zero rows for months while logging success.
async fn remove_namespace(ns: &str) -> Result<(), surrealdb::Error> {
    let (username, password) = credentials();
    let db = surrealdb::engine::any::connect(endpoint()).await?;
    db.signin(Root { username, password }).await?;
    // `ns` is always `test…_{simple uuid}` from `NsGuard`, so it cannot break
    // out of the backticks; they are belt-and-braces against a future prefix.
    db.query(format!("REMOVE NAMESPACE IF EXISTS `{ns}`"))
        .await?
        .check()?;
    Ok(())
}

/// A connected SurrealDB handle bound to its own namespace, with the schema
/// applied, which removes that namespace when dropped.
///
/// Derefs to `Surreal<Any>`, so `db.query(...)` and `&db` work unchanged at
/// call sites that previously held a bare handle.
#[derive(Debug)]
pub struct TestNs {
    db: Arc<Surreal<Any>>,
    guard: NsGuard,
}

impl TestNs {
    /// Connect, select a fresh namespace and `db_name`, and apply `SCHEMA_DEF`.
    ///
    /// The schema apply is unconditional and idempotent (every statement is
    /// `IF NOT EXISTS`) so no test depends on a sibling having run first —
    /// that ordering assumption has broken under changing test parallelism.
    pub async fn connect(db_name: &str) -> Self {
        Self::connect_with_prefix("test", db_name).await
    }

    /// As [`TestNs::connect`], with a custom namespace prefix.
    pub async fn connect_with_prefix(prefix: &str, db_name: &str) -> Self {
        let guard = NsGuard::with_prefix(prefix);
        let (username, password) = credentials();

        let db = surrealdb::engine::any::connect(endpoint())
            .await
            .expect("SurrealDB unreachable — start it and set URBAN_DB_TESTS=1");
        db.signin(Root { username, password })
            .await
            .expect("SurrealDB root sign-in failed");
        db.use_ns(guard.name())
            .use_db(db_name)
            .await
            .expect("SurrealDB namespace/database selection failed");
        db.query(urban_integrity_schema::db::SCHEMA_DEF)
            .await
            .expect("schema init failed")
            .check()
            .expect("schema init returned a statement error");

        Self {
            db: Arc::new(db),
            guard,
        }
    }

    /// The namespace name — for tests that must point a second client
    /// (raw HTTP config, spawned CLI) at the same namespace.
    pub fn ns(&self) -> &str {
        self.guard.name()
    }

    /// A cloned `Arc` of the handle, for APIs taking `Arc<Surreal<Any>>`.
    ///
    /// The clone does **not** extend the namespace's life: teardown still
    /// happens when this `TestNs` drops.
    pub fn arc(&self) -> Arc<Surreal<Any>> {
        Arc::clone(&self.db)
    }
}

impl Deref for TestNs {
    type Target = Surreal<Any>;

    fn deref(&self) -> &Self::Target {
        &self.db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_names_are_unique_and_prefixed() {
        let a = NsGuard::new();
        let b = NsGuard::new();
        assert!(a.name().starts_with("test_"));
        assert_ne!(a.name(), b.name());
    }

    #[test]
    #[should_panic(expected = "must start with `test`")]
    fn guard_rejects_a_prefix_that_would_escape_the_sweeper() {
        let _ = NsGuard::with_prefix("urban");
    }

    /// The guard must actually remove the namespace, not merely forget the name.
    #[tokio::test]
    async fn dropping_a_test_ns_removes_the_namespace() {
        if !db_tests_enabled() {
            return;
        }
        let ns = {
            let db = TestNs::connect("main").await;
            let ns = db.ns().to_string();
            assert!(
                namespace_exists(&ns).await,
                "namespace should exist while held"
            );
            ns
        };
        assert!(
            !namespace_exists(&ns).await,
            "namespace {ns} survived the guard's Drop"
        );
    }

    async fn namespace_exists(ns: &str) -> bool {
        let (username, password) = credentials();
        let db = surrealdb::engine::any::connect(endpoint())
            .await
            .expect("SurrealDB unreachable");
        db.signin(Root { username, password })
            .await
            .expect("sign-in failed");
        let info: Option<serde_json::Value> = db
            .query("INFO FOR ROOT")
            .await
            .expect("INFO FOR ROOT failed")
            .take(0)
            .expect("INFO FOR ROOT decode failed");
        info.and_then(|v| v.get("namespaces").cloned())
            .and_then(|n| n.get(ns).cloned())
            .is_some()
    }
}
