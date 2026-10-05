//! Shared Ollama integration module.
//!
//! Provides a single `summarize_via_ollama` function used by both `report.rs`
//! and `verify.rs`. Degrades gracefully (returns `None` with a WARN log) when
//! Ollama is unreachable or returns an unexpected response — never panics.
//!
//! Loopback-only: the URL is hardcoded to `localhost:11434`. No user input
//! reaches the network destination (T-14-02-03 accepted risk).

/// Ollama OpenAI-compatible endpoint (loopback only — T-12-03 / T-14-02-03 accepted).
pub const OLLAMA_URL: &str = "http://localhost:11434/v1";

/// Call Ollama with the `gemma4` model to produce a 2-3 sentence summary narrative.
///
/// Graceful degradation: on any failure (network, HTTP error, parse, missing field)
/// emits a `tracing::warn!` and returns `None`. Never panics, never returns `Err`.
///
/// The prompt is contextualised for the verify command ("pre-signature risk verdict").
/// Both `report.rs` and `verify.rs` import this function; the prompt wording is
/// appropriate for either audit context.
pub async fn summarize_via_ollama(text: &str) -> Option<String> {
    let body = serde_json::json!({
        "model": "gemma4",
        "messages": [
            {
                "role": "system",
                "content": "You are a procurement fraud analyst. Be concise."
            },
            {
                "role": "user",
                "content": format!("Summarise this pre-signature risk verdict in 2-3 sentences:\n\n{}", text)
            }
        ],
        "stream": false
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/chat/completions", OLLAMA_URL))
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "Ollama unavailable — skipping --summarize");
            return None;
        }
    };

    if !resp.status().is_success() {
        tracing::warn!(status = %resp.status(), "Ollama returned error status — skipping --summarize");
        return None;
    }

    let json: serde_json::Value = match resp.json().await {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!(error = %e, "Ollama response parse error — skipping --summarize");
            return None;
        }
    };

    let content = json["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_owned);
    if content.is_none() {
        tracing::warn!("Ollama response missing expected 'choices[0].message.content' field — skipping --summarize");
    }
    content
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify the public function signature compiles and the URL constant is correct.
    /// Network behaviour is tested implicitly by calling the real function — if Ollama
    /// is not running the function must return None without panicking.
    #[test]
    fn test_ollama_url_constant() {
        assert_eq!(OLLAMA_URL, "http://localhost:11434/v1");
    }

    /// Verify that the module path resolves and the function is accessible.
    /// The function signature (`pub async fn summarize_via_ollama(text: &str) -> Option<String>`)
    /// is validated at compile time whenever report.rs or verify.rs call it.
    #[test]
    fn test_summarize_via_ollama_is_accessible() {
        // Confirm that `summarize_via_ollama` is in scope and the module compiles.
        // If the function were removed or made private this test would fail to compile.
        let _ = std::any::type_name_of_val(&summarize_via_ollama);
    }
}
