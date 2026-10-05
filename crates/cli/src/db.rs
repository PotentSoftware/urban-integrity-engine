use anyhow::{Context, Result};
use surrealdb::{engine::any::Any, Surreal};
use urban_integrity_core::config::AppConfig;
use urban_integrity_schema::db::SCHEMA_DEF;

/// Connect to the SurrealDB database for the selected analysis.
///
/// Resolution (the single choke point): `config.analysis` — `None`/`main`
/// uses `config.surreal.database` verbatim (existing behaviour, no registry
/// access). Any other id is resolved via the `_catalog` registry to the
/// analysis's `db_name`; unknown or `retired` ids error.
pub async fn connect_db(config: &AppConfig) -> Result<Surreal<Any>> {
    let db_name = resolve_analysis_db(config).await?;
    connect_to_db(config, &db_name).await
}

/// Resolve the selected analysis to its target database name.
pub async fn resolve_analysis_db(config: &AppConfig) -> Result<String> {
    match config.analysis.as_deref() {
        None | Some("main") => Ok(config.surreal.database.clone()),
        Some(id) => {
            let cat = crate::registry::connect_catalog(config).await?;
            let row = crate::registry::get_analysis(&cat, id).await?.ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown analysis '{id}' — run `urban-integrity analysis list` to see known analyses"
                )
            })?;
            if row.status == "retired" || row.status == "retiring" {
                anyhow::bail!(
                    "analysis '{id}' is {} and cannot be operated on",
                    row.status
                );
            }
            Ok(row.db_name)
        }
    }
}

/// Connect + sign in + select `db_name` under the configured namespace, then
/// apply `SCHEMA_DEF` (idempotent). Shared by `connect_db` and provisioning.
pub async fn connect_to_db(config: &AppConfig, db_name: &str) -> Result<Surreal<Any>> {
    let http_url = config
        .surreal
        .ws_url
        .replace("wss://", "https://")
        .replace("ws://", "http://");

    let db: Surreal<Any> = surrealdb::engine::any::connect(http_url.as_str())
        .await
        .with_context(|| format!("SurrealDB connect failed: {http_url}"))?;

    if let (Some(user), Some(pass)) = (
        config.surreal.username.as_deref(),
        config.surreal.password.as_deref(),
    ) {
        db.signin(surrealdb::opt::auth::Root {
            username: user.to_string(),
            password: pass.to_string(),
        })
        .await
        .context("SurrealDB sign-in failed")?;
    }

    db.use_ns(&config.surreal.namespace)
        .use_db(db_name)
        .await
        .with_context(|| {
            format!(
                "SurrealDB namespace/db selection failed: {}/{}",
                config.surreal.namespace, db_name
            )
        })?;

    db.query(SCHEMA_DEF)
        .await
        .context("SurrealDB schema initialisation failed")?;

    tracing::debug!(
        url = %http_url,
        namespace = %config.surreal.namespace,
        database = %db_name,
        "CLI connected to SurrealDB via HTTP (schema applied)"
    );

    Ok(db)
}

/// Re-authenticate an existing SurrealDB connection using the same credentials.
///
/// Called when WAL writes start returning 401 Unauthorized, which happens when
/// the SurrealDB HTTP session token expires (default ~1 hour). The common case
/// (this is called proactively, before the token has actually expired — e.g.
/// `serve`'s 50-minute refresh against a ~1h token) re-signs in on the SAME
/// `Surreal<Any>` instance, which updates its shared session state so every
/// `Arc` clone (coordinator, writer loop, API layer) sees the fresh token with
/// no further action required. That case returns `Ok(None)`.
///
/// # Why a genuinely expired token needs a NEW connection
///
/// SurrealDB's HTTP transport attaches whatever `Authorization: Bearer <token>`
/// this connection currently holds to **every** subsequent request it sends —
/// including `signin`, `invalidate`, and `authenticate` themselves. The server
/// rejects the underlying HTTP request with 401 as soon as that header carries
/// an expired JWT, *before* it dispatches to the RPC method the request names.
/// That means once the token has genuinely expired there is no request this
/// connection can send — auth-refreshing or not — that the server will accept;
/// clearing the stale credential first does not help, because clearing it is
/// itself one of the blocked requests.
///
/// This was proven directly against the live server (SurrealDB 3.1.0-beta.2):
/// with a token past its `DURATION FOR TOKEN`, `.invalidate()`, `.signin()` and
/// even `.authenticate(fresh_jwt_from_a_separate_connection)` all fail with the
/// identical `401 Unauthorized — the token has expired`, regardless of which
/// RPC method the request names. See
/// `reauth_db_recovers_from_a_genuinely_expired_token` in
/// `crates/cli/src/commands/migrate.rs` for the reproduction.
///
/// So when the in-place `signin` fails, this function falls back to opening a
/// **brand-new** connection via [`connect_to_db`] and returns it as
/// `Ok(Some(new_db))`. The original `db` handle is permanently unusable at
/// that point — every caller MUST switch to the returned connection for
/// further use. Because a fresh connection cannot be transplanted into the
/// `Arc<Surreal<Any>>` other consumers already hold (mutating a shared
/// `Surreal<Any>` in place is exactly the recovery this fallback proves is no
/// longer possible), each call site is responsible for its own switch-over:
/// `serve`'s WAL-writer retries its own write against the returned connection,
/// and its periodic refresh task swaps its own held clone. Other `Arc` clones
/// of the old, now-dead `db` self-heal independently the next time they 401
/// and call `reauth_db` themselves.
///
/// `db_name` must be the database the caller RESOLVED — i.e. what [`resolve_analysis_db`]
/// returned — not `config.surreal.database`. Signing in again resets the connection's
/// namespace/database selection, so re-auth has to restore the same target the process has been
/// writing to all along; reading the raw config field here would quietly move a long-running
/// `serve` onto a different analysis at the one-hour mark, with nothing in the logs to say so.
/// Same defect shape as the `project --analysis` cross-database write fixed in PR #31.
pub async fn reauth_db(
    db: &Surreal<Any>,
    config: &AppConfig,
    db_name: &str,
) -> Result<Option<Surreal<Any>>> {
    let (Some(user), Some(pass)) = (
        config.surreal.username.as_deref(),
        config.surreal.password.as_deref(),
    ) else {
        // No credentials configured — nothing to re-authenticate.
        return Ok(None);
    };

    let signin_result = db
        .signin(surrealdb::opt::auth::Root {
            username: user.to_string(),
            password: pass.to_string(),
        })
        .await;

    match signin_result {
        Ok(_) => {
            db.use_ns(&config.surreal.namespace)
                .use_db(db_name)
                .await
                .context("SurrealDB namespace/db re-selection failed")?;
            Ok(None)
        }
        Err(signin_err) => {
            // The in-place signin was itself rejected — per the doc comment above, this
            // means the existing token has genuinely, fully expired (not merely due for a
            // proactive refresh), and `db` cannot be recovered by any request sent through
            // it. Open a fresh connection instead; the caller must switch to it.
            tracing::warn!(
                error = %signin_err,
                "in-place SurrealDB re-signin was rejected (token has fully expired) — \
                 opening a fresh connection instead"
            );
            let new_db = connect_to_db(config, db_name).await.with_context(|| {
                format!(
                    "SurrealDB re-signin failed and reconnect also failed (namespace={}, db={db_name})",
                    config.surreal.namespace
                )
            })?;
            Ok(Some(new_db))
        }
    }
}
