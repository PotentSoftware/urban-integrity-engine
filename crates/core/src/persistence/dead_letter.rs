use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use tracing::error;

use crate::graph::types::GraphEvent;

/// Append a failed GraphEvent to the JSONL dead letter log.
///
/// One JSON object per line, with the failed event, reason, and timestamp.
/// Creates parent directories if they don't exist.
pub fn log_dead_letter(path: &Path, event: &GraphEvent, reason: &str) {
    #[derive(serde::Serialize)]
    struct DeadLetterEntry<'a> {
        event: &'a GraphEvent,
        reason: &'a str,
        logged_at: u64,
    }

    let entry = DeadLetterEntry {
        event,
        reason,
        logged_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };

    let line = match serde_json::to_string(&entry) {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to serialise dead letter entry: {}", e);
            return;
        }
    };

    let parent = path.parent().unwrap_or(Path::new("."));
    if let Err(e) = std::fs::create_dir_all(parent) {
        error!("Failed to create dead letter directory {:?}: {}", parent, e);
        return;
    }

    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut file) => {
            if let Err(e) = writeln!(file, "{}", line) {
                error!("Failed to write dead letter entry: {}", e);
            }
        }
        Err(e) => error!("Failed to open dead letter log {:?}: {}", path, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::types::{GraphEvent, GraphEventKind};
    use std::fs;
    use tempfile::tempdir;

    fn make_event() -> GraphEvent {
        GraphEvent {
            id: uuid::Uuid::new_v4(),
            timestamp: 1_000_000,
            mutation: GraphEventKind::NodeAdded {
                entity_id: "company-test".to_string(),
                weight_json: "{}".to_string(),
            },
        }
    }

    #[test]
    fn test_log_dead_letter_creates_file() {
        let dir = tempdir().expect("tmpdir");
        let path = dir.path().join("dead_letters.jsonl");
        assert!(!path.exists(), "File should not exist before logging");

        let event = make_event();
        log_dead_letter(&path, &event, "SurrealDB timeout");

        assert!(path.exists(), "Dead letter log file should be created");
    }

    #[test]
    fn test_log_dead_letter_writes_valid_jsonl() {
        let dir = tempdir().expect("tmpdir");
        let path = dir.path().join("dead_letters.jsonl");
        let event = make_event();

        log_dead_letter(&path, &event, "connection refused");

        let contents = fs::read_to_string(&path).expect("read file");
        let line = contents.trim();
        assert!(!line.is_empty(), "File should not be empty");
        // Must be parseable as JSON
        let parsed: serde_json::Value =
            serde_json::from_str(line).expect("Each line must be valid JSON");
        assert_eq!(
            parsed["reason"], "connection refused",
            "reason field must match"
        );
        assert!(
            parsed["logged_at"].is_u64(),
            "logged_at must be a u64 timestamp"
        );
    }

    #[test]
    fn test_log_dead_letter_appends_multiple_entries() {
        let dir = tempdir().expect("tmpdir");
        let path = dir.path().join("dead_letters.jsonl");
        let event = make_event();

        log_dead_letter(&path, &event, "error 1");
        log_dead_letter(&path, &event, "error 2");
        log_dead_letter(&path, &event, "error 3");

        let contents = fs::read_to_string(&path).expect("read file");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 3, "Each call should append one JSONL line");
    }

    #[test]
    fn test_log_dead_letter_creates_parent_directories() {
        let dir = tempdir().expect("tmpdir");
        let nested = dir.path().join("deep/nested/path/dead_letters.jsonl");
        assert!(
            !nested.parent().unwrap().exists(),
            "Parent dirs should not exist yet"
        );

        let event = make_event();
        log_dead_letter(&nested, &event, "nested test");

        assert!(nested.exists(), "File should be created with parent dirs");
    }
}
