use crate::backfill::cursor::{resume_index, CursorState, ResumeCursor};
use crate::backfill::session::DbSession;
use crate::backfill::{extract_id_str, BackfillStats};
use anyhow::Result;

// Re-export normalise_name from core so existing call sites in this module compile unchanged.
pub use urban_integrity_core::stream::normalise_name;

/// Cursor job name — `backfill_cursors:ch`.
const CH_JOB: &str = "ch";

pub struct ChBackfiller {
    session: DbSession,
    api_key: String,
    rate_limit: u32,
    pub dry_run: bool,
    /// Ignore (and clear) any saved cursor, walking the whole candidate list again.
    pub restart: bool,
}

impl ChBackfiller {
    pub fn new(session: DbSession, api_key: String, rate_limit: u32, dry_run: bool) -> Self {
        Self {
            session,
            api_key,
            rate_limit,
            dry_run,
            restart: false,
        }
    }

    /// Discard any saved position and process every candidate from the top.
    pub fn restart(mut self, restart: bool) -> Self {
        self.restart = restart;
        self
    }

    pub async fn run(&self) -> Result<BackfillStats> {
        use crate::backfill::update_node_event_by_record;
        use urban_integrity_schema::types::node::{EntityData, NodeWeight};

        let rest = ChRestClient::new_with_base(
            self.api_key.clone(),
            self.rate_limit,
            "https://api.company-information.service.gov.uk".to_string(),
            std::time::Duration::from_secs(60),
        );

        let mut stats = BackfillStats::default();

        // Fetch all Entity NodeAdded events. Paginated and kind-filtered Rust-side: the former
        // single-shot `kind = 'NodeAdded' AND … CONTAINS … LIMIT 50000` read 6,033 of 33,046 rows
        // and never tripped its own guard. See `scan_events_by_type`.
        let rows =
            crate::backfill::scan_events_by_type(&self.session, "NodeAdded", "\"type\":\"Entity\"")
                .await?;

        // Collect (record_id, entity_id, EntityData) for director: entities missing
        // company_number. The SurrealDB **record id** is carried, not the `event_id`, so the
        // write below addresses the row by primary key — see `update_node_event_by_record`.
        let targets: Vec<(String, String, EntityData)> = rows
            .into_iter()
            .filter_map(|row| {
                let record_id = extract_id_str(&row["id"])?;
                let entity_id_val = &row["payload"]["entity_id"];
                let entity_id = extract_id_str(entity_id_val)?;
                if !entity_id.starts_with("director:") {
                    return None;
                }
                let wj = row["payload"]["weight_json"].as_str()?;
                let weight: NodeWeight = serde_json::from_str(wj).ok()?;
                match weight {
                    NodeWeight::Entity(e) if e.company_number.is_none() => {
                        Some((record_id, entity_id, e))
                    }
                    _ => None,
                }
            })
            .collect();

        // ── resume ───────────────────────────────────────────────────────────────────────────
        //
        // Every verdict except `Exact` leaves the entity untouched, so a restart re-issues every
        // negative lookup it already paid for: live 2026-07-28, 20,306 of 20,660 candidates ended
        // `NoMatch`/`Ambiguous`, roughly three hours of API budget at the 2 req/s ceiling. The
        // cursor makes an interrupted run resumable instead. A dry run deliberately neither reads
        // nor writes it — it changes nothing, so it must not claim progress either.
        let cursor = ResumeCursor::new(self.session.clone(), CH_JOB);
        let saved = if self.dry_run {
            None
        } else if self.restart {
            cursor.clear().await?;
            tracing::info!("CH backfill: --restart given \u{2014} saved cursor discarded");
            None
        } else {
            cursor.load().await?
        };

        let start_at = match &saved {
            Some(state) => {
                resume_index(&targets, &state.last_record_id, |(rid, _, _)| rid.as_str())
            }
            None => 0,
        };

        // Totals carry across resumes so the final numbers describe the whole job, not the last
        // leg of it. `scanned` counts candidates considered overall: those a previous leg got
        // through, plus those still ahead.
        let prior = saved.clone().unwrap_or_default();
        stats.matched = prior.matched;
        stats.skipped = prior.skipped;
        stats.errors = prior.errors;
        stats.scanned = prior.processed + targets.len().saturating_sub(start_at);

        if let Some(state) = &saved {
            tracing::info!(
                resumed_after = %state.last_record_id,
                already_processed = state.processed,
                remaining = targets.len().saturating_sub(start_at),
                "CH backfill: resuming from saved cursor"
            );
        } else {
            tracing::info!(count = targets.len(), "CH backfill: scanning entities");
        }

        let mut position = CursorState {
            processed: prior.processed,
            ..prior
        };

        for (record_id, entity_id, data) in targets.iter().skip(start_at) {
            let results = match rest.search(&data.name).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(?e, entity = %entity_id, "CH search error");
                    stats.errors += 1;
                    // Deliberately NOT advancing the cursor: an error means this candidate was
                    // never resolved, and a resumed run should retry it rather than inherit a
                    // transient failure as a permanent verdict.
                    continue;
                }
            };

            match match_company(&data.name, &results) {
                MatchVerdict::Exact(ch_num) => {
                    tracing::info!(entity = %entity_id, ch_num = %ch_num, "CH exact match");
                    if !self.dry_run {
                        let updated = NodeWeight::Entity(EntityData {
                            company_number: Some(ch_num),
                            ..data.clone()
                        });
                        update_node_event_by_record(&self.session, record_id, &updated).await?;
                    }
                    stats.matched += 1;
                }
                MatchVerdict::Ambiguous => {
                    tracing::debug!(entity = %entity_id, "CH ambiguous \u{2014} skipping");
                    stats.skipped += 1;
                }
                MatchVerdict::NoMatch => {
                    tracing::debug!(entity = %entity_id, "CH no match \u{2014} skipping");
                    stats.skipped += 1;
                }
            }

            if !self.dry_run {
                position = CursorState {
                    last_record_id: record_id.clone(),
                    processed: position.processed + 1,
                    matched: stats.matched,
                    skipped: stats.skipped,
                    errors: stats.errors,
                };
                // A cursor write that fails is not worth aborting a run over, but it must be
                // visible: silently losing it turns the next restart into a full re-pass.
                if let Err(e) = cursor.save(&position).await {
                    tracing::warn!(error = %e, "CH backfill: failed to save resume cursor");
                }
            }
        }

        // Clean completion — the next run starts fresh rather than resuming past the end.
        if !self.dry_run {
            cursor.clear().await?;
        }

        tracing::info!(
            scanned = stats.scanned,
            matched = stats.matched,
            skipped = stats.skipped,
            errors = stats.errors,
            resumed_from = start_at,
            "CH backfill complete"
        );
        Ok(stats)
    }
}

// ── name-matching ─────────────────────────────────────────────────────────────
// Exact-match-only policy (2026-06-13): the former edit-distance (<= 2) `FuzzyWarn`
// path assigned a CH company_number to an entity node on a *near* name match. That is
// name-only fuzzy matching — the false-positive class purged from the enricher adapters
// — so it was removed. A company_number is now assigned ONLY on an exact normalised-name
// match against the CH search results; near misses are treated as NoMatch.
#[derive(Debug, PartialEq)]
pub enum MatchVerdict {
    Exact(String),
    Ambiguous,
    NoMatch,
}
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ChSearchItem {
    pub title: String,
    pub company_number: String,
    pub company_status: Option<String>,
}
pub fn match_company(query: &str, results: &[ChSearchItem]) -> MatchVerdict {
    if results.is_empty() {
        return MatchVerdict::NoMatch;
    }
    let q_norm = normalise_name(query);
    if q_norm.is_empty() {
        return MatchVerdict::NoMatch;
    }
    let exact: Vec<&ChSearchItem> = results
        .iter()
        .filter(|r| normalise_name(&r.title) == q_norm)
        .collect();
    match exact.len() {
        1 => MatchVerdict::Exact(exact[0].company_number.clone()),
        n if n > 1 => MatchVerdict::Ambiguous,
        // Exact-match-only policy: no near-miss fallback. A non-exact name is NoMatch.
        _ => MatchVerdict::NoMatch,
    }
}

#[derive(serde::Deserialize, Debug)]
pub struct ChSearchResponse {
    pub items: Vec<ChSearchItem>,
}

pub struct ChRestClient {
    client: reqwest::Client,
    api_key: String,
    delay: std::time::Duration,
    base_url: String,
    backoff_429: std::time::Duration,
}

impl ChRestClient {
    pub fn new(api_key: String, rate_limit_per_sec: u32) -> Self {
        Self::new_with_base(
            api_key,
            rate_limit_per_sec,
            "https://api.company-information.service.gov.uk".to_string(),
            std::time::Duration::from_secs(60),
        )
    }

    pub fn new_with_base(
        api_key: String,
        rate_limit_per_sec: u32,
        base_url: String,
        backoff_429: std::time::Duration,
    ) -> Self {
        let delay = std::time::Duration::from_millis(1000 / rate_limit_per_sec.max(1) as u64);
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
            api_key,
            delay,
            base_url,
            backoff_429,
        }
    }

    /// A company's officer list. `Ok(None)` means the request was rate-limited and backed off —
    /// the caller should stop and resume rather than hammering the endpoint.
    pub async fn company_officers(
        &self,
        company_number: &str,
    ) -> anyhow::Result<Option<Vec<crate::backfill::officers::CachedAppointment>>> {
        tokio::time::sleep(self.delay).await;
        let url = format!("{}/company/{}/officers", self.base_url, company_number);
        let response = self
            .client
            .get(&url)
            .basic_auth(&self.api_key, Some(""))
            .query(&[("items_per_page", "100")])
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Some(Vec::new()));
        }
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            tracing::warn!(company_number, "CH 429 — backing off");
            tokio::time::sleep(self.backoff_429).await;
            return Ok(None);
        }
        if !response.status().is_success() {
            anyhow::bail!("CH officers {company_number}: HTTP {}", response.status());
        }
        let v: serde_json::Value = response.json().await?;
        let items = v
            .get("items")
            .and_then(|i| i.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|it| serde_json::from_value(it.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();
        Ok(Some(items))
    }

    /// One company profile. `Ok(None)` means Companies House returned 404 — the number is not
    /// known to CH, which is a real answer worth caching, not an error to retry.
    pub async fn company_profile(
        &self,
        company_number: &str,
    ) -> anyhow::Result<Option<crate::backfill::cessation::CachedProfile>> {
        tokio::time::sleep(self.delay).await;
        let url = format!("{}/company/{}", self.base_url, company_number);
        let response = self
            .client
            .get(&url)
            .basic_auth(&self.api_key, Some(""))
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Some(crate::backfill::cessation::CachedProfile {
                company_number: company_number.to_string(),
                company_status: None,
                date_of_cessation: None,
                gone: true,
            }));
        }
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            tracing::warn!(company_number, "CH 429 — backing off");
            tokio::time::sleep(self.backoff_429).await;
            return Ok(None);
        }
        if !response.status().is_success() {
            anyhow::bail!(
                "CH company profile {company_number}: HTTP {}",
                response.status()
            );
        }
        let v: serde_json::Value = response.json().await?;
        Ok(Some(crate::backfill::cessation::CachedProfile {
            company_number: company_number.to_string(),
            company_status: v
                .get("company_status")
                .and_then(|s| s.as_str())
                .map(|s| s.to_ascii_lowercase()),
            date_of_cessation: v
                .get("date_of_cessation")
                .and_then(|s| s.as_str())
                .map(|s| s[..s.len().min(10)].to_string()),
            gone: false,
        }))
    }

    pub async fn search(&self, name: &str) -> anyhow::Result<Vec<ChSearchItem>> {
        tokio::time::sleep(self.delay).await;
        let url = format!("{}/search/companies", self.base_url);
        let response = self
            .client
            .get(&url)
            .query(&[("q", name), ("items_per_page", "5")])
            .basic_auth(&self.api_key, None::<&str>)
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            tracing::warn!(
                "CH API rate limited — backing off {}s",
                self.backoff_429.as_secs()
            );
            tokio::time::sleep(self.backoff_429).await;
            anyhow::bail!("CH API 429 rate limit");
        }
        if !response.status().is_success() {
            tracing::warn!(status = %response.status(), "CH API non-2xx for {:?}", name);
            return Ok(vec![]);
        }
        let body: ChSearchResponse = response.json().await?;
        Ok(body.items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalise_strips_ltd() {
        assert_eq!(normalise_name("ACME Ltd"), "acme");
    }

    #[test]
    fn test_normalise_strips_limited() {
        assert_eq!(normalise_name("Apex Build Limited"), "apex build");
    }

    #[test]
    fn test_normalise_strips_plc() {
        assert_eq!(normalise_name("Big Corp Plc"), "big corp");
    }

    #[test]
    fn test_normalise_strips_punctuation() {
        assert_eq!(
            normalise_name("O'Brien Construction Ltd."),
            "obrien construction"
        );
    }

    #[test]
    fn test_normalise_collapses_whitespace() {
        assert_eq!(normalise_name("  A  B  Ltd  "), "a b");
    }

    fn item(title: &str, number: &str) -> ChSearchItem {
        ChSearchItem {
            title: title.to_string(),
            company_number: number.to_string(),
            company_status: None,
        }
    }

    #[test]
    fn test_match_exact() {
        let results = vec![item("ACME LTD", "12345678")];
        assert!(
            matches!(match_company("ACME Ltd", &results), MatchVerdict::Exact(n) if n == "12345678")
        );
    }

    #[test]
    fn test_match_no_results() {
        assert!(matches!(
            match_company("NoMatch Corp", &[]),
            MatchVerdict::NoMatch
        ));
    }

    #[test]
    fn test_match_ambiguous_two_exact() {
        let results = vec![item("ACME LTD", "11111111"), item("ACME LTD", "22222222")];
        assert!(matches!(
            match_company("ACME Ltd", &results),
            MatchVerdict::Ambiguous
        ));
    }

    #[test]
    fn test_match_near_miss_is_no_match_exact_only() {
        // Exact-match-only policy (2026-06-13): a near-miss name (edit distance 1) that
        // previously produced FuzzyWarn must now be NoMatch — no company_number is assigned
        // on a non-exact name.
        let results = vec![item("ACME BUILDIG LTD", "12345678")];
        assert!(matches!(
            match_company("Acme Building Ltd", &results),
            MatchVerdict::NoMatch
        ));
    }

    #[test]
    fn test_match_completely_different_is_no_match() {
        // "completely different" → no match
        let results = vec![item("COMPLETELY DIFFERENT COMPANY LTD", "12345678")];
        assert!(matches!(
            match_company("ACME Ltd", &results),
            MatchVerdict::NoMatch
        ));
    }

    #[tokio::test]
    async fn test_ch_rest_client_returns_items() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let body = r#"{"items":[{"title":"ACME LTD","company_number":"12345678","company_status":"active"}]}"#;
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                );
            }
        });
        let client = ChRestClient::new_with_base(
            "testkey".to_string(),
            100,
            format!("http://127.0.0.1:{}", port),
            std::time::Duration::from_millis(1),
        );
        let results = client.search("ACME Ltd").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].company_number, "12345678");
    }

    #[tokio::test]
    async fn test_ch_rest_client_handles_429() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        let client = ChRestClient::new_with_base(
            "testkey".to_string(),
            100,
            format!("http://127.0.0.1:{}", port),
            std::time::Duration::from_millis(1),
        );
        // 429 should return Err so the caller increments stats.errors
        let result = client.search("ACME Ltd").await;
        assert!(result.is_err(), "429 should return Err");
    }

    #[test]
    fn test_match_empty_normalised_name() {
        let results = vec![item("LTD", "12345678")];
        assert!(matches!(
            match_company("Ltd", &results),
            MatchVerdict::NoMatch
        ));
    }
}
