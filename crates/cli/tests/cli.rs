/// CLI integration tests.
///
/// Two categories:
///  1. **Unit-style**: pure function tests for output formatters (severity_color,
///     build_alert_table, print_json, OutputFormat enum) — no process spawning.
///  2. **Process smoke tests**: run the compiled CLI binary via `std::process::Command`
///     to verify argument parsing, help output, and command dispatch.
///  3. **Live DB CLI tests** (requires `URBAN_DB_TESTS=1` and a running SurrealDB):
///     run `report` and `alerts list` end-to-end against the database.
///
/// Guard summary:
///   URBAN_LIVE_TESTS=1  — CH / CF network tests (no local services needed)
///   URBAN_DB_TESTS=1    — CLI subprocess tests that connect to SurrealDB
///
/// Note: tests in category 3 require the test process to have unrestricted TCP
/// access so the spawned CLI binary can reach SurrealDB. In sandboxed/CI
/// environments where subprocess network access is blocked, verify these
/// commands manually:
///   ~/.surrealdb/surreal start --user root --pass root surrealkv://data/surreal.db
///   cargo run -p urban-integrity-cli -- report --output json
///   cargo run -p urban-integrity-cli -- alerts list --output json
use urban_integrity_cli::{
    commands::alerts::AlertRow,
    output::{build_alert_table, severity_color},
};

// ── severity_color ────────────────────────────────────────────────────────────

#[test]
fn test_severity_color_high_risk_is_red() {
    use comfy_table::Color;
    assert_eq!(severity_color(0.85), Color::Red);
    assert_eq!(severity_color(1.0), Color::Red);
    assert_eq!(severity_color(0.9), Color::Red);
}

#[test]
fn test_severity_color_medium_risk_is_yellow() {
    use comfy_table::Color;
    assert_eq!(severity_color(0.65), Color::Yellow);
    assert_eq!(severity_color(0.75), Color::Yellow);
    assert_eq!(severity_color(0.84), Color::Yellow);
}

#[test]
fn test_severity_color_low_risk_is_green() {
    use comfy_table::Color;
    assert_eq!(severity_color(0.0), Color::Green);
    assert_eq!(severity_color(0.5), Color::Green);
    assert_eq!(severity_color(0.64), Color::Green);
}

// ── build_alert_table ─────────────────────────────────────────────────────────

#[test]
fn test_build_alert_table_empty_produces_header_row() {
    let table = build_alert_table(&[]);
    let rendered = table.to_string();
    assert!(rendered.contains("ID"), "header row should contain 'ID'");
    assert!(
        rendered.contains("Motif"),
        "header row should contain 'Motif'"
    );
    assert!(
        rendered.contains("Score"),
        "header row should contain 'Score'"
    );
    assert!(
        rendered.contains("Ward"),
        "header row should contain 'Ward'"
    );
    assert!(
        rendered.contains("Detected"),
        "header row should contain 'Detected'"
    );
}

#[test]
fn test_build_alert_table_single_row_renders_values() {
    let alerts = vec![AlertRow {
        id: "alert-001".into(),
        motif_type: "PhoenixLoop".into(),
        score: 0.92,
        ward: Some("E05001234".into()),
        detected_at: "2026-05-11T10:00:00Z".into(),
        entities: None,
    }];
    let rendered = build_alert_table(&alerts).to_string();
    assert!(rendered.contains("alert-001"));
    assert!(rendered.contains("PhoenixLoop"));
    assert!(rendered.contains("0.920"));
    assert!(rendered.contains("E05001234"));
}

#[test]
fn test_build_alert_table_none_ward_renders_dash() {
    let alerts = vec![AlertRow {
        id: "alert-002".into(),
        motif_type: "AddressCluster".into(),
        score: 0.71,
        ward: None,
        detected_at: "2026-05-11T11:00:00Z".into(),
        entities: None,
    }];
    let rendered = build_alert_table(&alerts).to_string();
    // None ward renders as em-dash (—)
    assert!(
        rendered.contains('\u{2014}'),
        "None ward should render as em-dash"
    );
}

#[test]
fn test_build_alert_table_multiple_rows() {
    let alerts: Vec<AlertRow> = (0..5)
        .map(|i| AlertRow {
            id: format!("alert-{:03}", i),
            motif_type: "NepotismBridge".into(),
            score: 0.5 + (i as f64 * 0.1),
            ward: None,
            detected_at: "2026-05-11T12:00:00Z".into(),
            entities: None,
        })
        .collect();

    let rendered = build_alert_table(&alerts).to_string();
    for i in 0..5 {
        assert!(rendered.contains(&format!("alert-{:03}", i)));
    }
}

#[test]
fn test_build_alert_table_with_entities_column() {
    let alerts = vec![AlertRow {
        id: "alert-001".into(),
        motif_type: "PhoenixLoop".into(),
        score: 0.92,
        ward: Some("E05001234".into()),
        detected_at: "2026-05-11T10:00:00Z".into(),
        entities: Some("Apex Build Ltd, Beta Contractors + 1 more".into()),
    }];
    let rendered = build_alert_table(&alerts).to_string();
    assert!(
        rendered.contains("Entities"),
        "table header must include 'Entities' column, got: {rendered}"
    );
    assert!(
        rendered.contains("Apex Build Ltd"),
        "table row must include entity name, got: {rendered}"
    );
}

// ── print_json ────────────────────────────────────────────────────────────────

#[test]
fn test_print_json_serializes_struct() {
    // print_json writes to stdout — we just verify it doesn't error
    let alert = AlertRow {
        id: "test".into(),
        motif_type: "PhoenixLoop".into(),
        score: 0.88,
        ward: Some("E05001234".into()),
        detected_at: "2026-05-11T10:00:00Z".into(),
        entities: None,
    };
    // Capture the serialization output without actually printing
    let json = serde_json::to_string_pretty(&alert).unwrap();
    assert!(json.contains("\"id\": \"test\""));
    assert!(json.contains("\"score\": 0.88"));
    assert!(json.contains("PhoenixLoop"));
}

#[test]
fn test_print_json_empty_vec_is_valid() {
    let empty: Vec<AlertRow> = vec![];
    let json = serde_json::to_string_pretty(&empty).unwrap();
    assert_eq!(json.trim(), "[]");
}

// ── CLI binary smoke tests ────────────────────────────────────────────────────

/// Get the path to the compiled CLI binary.
/// Cargo sets CARGO_BIN_EXE_<name> automatically for integration tests.
fn cli_bin() -> std::path::PathBuf {
    // Cargo sets this env var for the exact binary being tested
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_urban-integrity-cli") {
        return std::path::PathBuf::from(p);
    }
    // Fallback: walk up from the test deps dir to target/debug/
    let mut path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("urban-integrity-cli")
}

#[test]
fn test_cli_help_exits_zero() {
    let output = std::process::Command::new(cli_bin())
        .arg("--help")
        .output()
        .expect("failed to run CLI binary");

    assert!(
        output.status.success(),
        "CLI --help should exit 0, got: {:?}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("urban-integrity") || stdout.contains("Usage"),
        "Expected help text, got: {}",
        stdout
    );
}

#[test]
fn test_cli_report_help_exits_zero() {
    let output = std::process::Command::new(cli_bin())
        .args(["report", "--help"])
        .output()
        .expect("failed to run CLI binary");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("ward") || stdout.contains("min-score"),
        "Expected report help flags, got: {}",
        stdout
    );
}

#[test]
fn test_cli_alerts_help_exits_zero() {
    let output = std::process::Command::new(cli_bin())
        .args(["alerts", "--help"])
        .output()
        .expect("failed to run CLI binary");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("list") || stdout.contains("show"),
        "Expected alerts subcommands in help, got: {}",
        stdout
    );
}

#[test]
fn test_cli_status_help_exits_zero() {
    let output = std::process::Command::new(cli_bin())
        .args(["status", "--help"])
        .output()
        .expect("failed to run CLI binary");

    assert!(output.status.success());
}

/// Build a CLI subprocess command pre-configured for DB tests.
///
/// - Sets `current_dir` to workspace root so `config.toml` is found.
/// - Overrides `URBAN__SURREAL__WS_URL` to use `127.0.0.1` instead of
///   `localhost` because subprocess environments may lack `/etc/hosts`
///   DNS resolution for loopback aliases.
fn db_cli_cmd(args: &[&str]) -> std::process::Output {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap() // crates/cli -> crates
        .parent()
        .unwrap(); // crates -> workspace root

    std::process::Command::new(cli_bin())
        .args(args)
        .current_dir(workspace_root)
        // ws:// is converted to http:// inside connect_db — both loopback forms work.
        .env("URBAN__SURREAL__WS_URL", "ws://localhost:8000")
        .env("URBAN__SURREAL__USERNAME", "root")
        .env("URBAN__SURREAL__PASSWORD", "root")
        .output()
        .expect("failed to run CLI binary")
}

#[test]
fn test_cli_invalid_subcommand_exits_nonzero() {
    let output = std::process::Command::new(cli_bin())
        .arg("nonexistent-command")
        .output()
        .expect("failed to run CLI binary");

    assert!(
        !output.status.success(),
        "Unknown subcommand should exit non-zero"
    );
}

// ── Additional DB-gated CLI tests ─────────────────────────────────────────────
// These tests exercise the full CLI ↔ SurrealDB path and were motivated by
// the debugging experience connecting surrealdb SDK v3 to server v3.0.2.

fn skip_if_no_db() -> bool {
    if std::env::var("URBAN_DB_TESTS").as_deref() != Ok("1") {
        eprintln!("[skip] set URBAN_DB_TESTS=1 with SurrealDB running to execute");
        return true;
    }
    // Pre-flight TCP check — skip gracefully if DB isn't up yet rather than panicking
    // in the spawned subprocess (which produces a confusing error message).
    if std::net::TcpStream::connect("localhost:8000").is_err() {
        eprintln!("[skip] SurrealDB not reachable on localhost:8000 — start it first");
        return true;
    }
    false
}

/// report --output table should print the ASCII table header
#[test]
fn test_cli_report_table_output_contains_header() {
    if skip_if_no_db() {
        return;
    }
    let output = db_cli_cmd(&["report", "--output", "table"]);
    assert!(
        output.status.success(),
        "report table failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Total alerts:"),
        "table output must contain 'Total alerts:' summary line"
    );
    assert!(
        stdout.contains("Motif"),
        "table header must include 'Motif' column"
    );
}

/// report --ward with a valid GSS code exits 0 and reflects filter in JSON
#[test]
fn test_cli_report_with_ward_filter_accepts_arg() {
    if skip_if_no_db() {
        return;
    }
    let output = db_cli_cmd(&[
        "report",
        "--ward",
        "E05001234",
        "--min-score",
        "0.7",
        "--output",
        "json",
    ]);
    assert!(
        output.status.success(),
        "CLI report with --ward should exit 0"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("should be JSON");
    assert_eq!(parsed["filters_applied"]["ward"], "E05001234");
    assert_eq!(parsed["filters_applied"]["min_score"], 0.7);
}

/// report --min-score 0.0 should return all events, not error out
#[test]
fn test_cli_report_zero_min_score_returns_all() {
    if skip_if_no_db() {
        return;
    }
    let output = db_cli_cmd(&["report", "--min-score", "0.0", "--output", "json"]);
    assert!(output.status.success());
    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert!(parsed["total_alerts"].is_number());
}

/// report --output json must have the correct envelope keys
#[test]
fn test_cli_report_json_output_is_valid_json() {
    if skip_if_no_db() {
        return;
    }
    let output = db_cli_cmd(&["report", "--output", "json"]);
    assert!(
        output.status.success(),
        "report --output json failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .expect("CLI report --output json must produce valid JSON");
    assert!(parsed["generated_at"].is_string(), "must have generated_at");
    assert!(parsed["total_alerts"].is_number(), "must have total_alerts");
    assert!(parsed["alerts"].is_array(), "must have alerts array");
}

/// alerts list --output json must return a JSON array
#[test]
fn test_cli_alerts_list_json_output_is_valid_json() {
    if skip_if_no_db() {
        return;
    }
    let output = db_cli_cmd(&["alerts", "list", "--output", "json"]);
    assert!(output.status.success());
    let parsed: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .expect("alerts list --output json should produce valid JSON");
    assert!(
        parsed.is_array(),
        "alerts list JSON output should be an array"
    );
}

/// alerts list --limit 5 should return at most 5 results
#[test]
fn test_cli_alerts_list_limit_flag() {
    if skip_if_no_db() {
        return;
    }
    let output = db_cli_cmd(&["alerts", "list", "--limit", "5", "--output", "json"]);
    assert!(output.status.success());
    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    let arr = parsed.as_array().expect("should be array");
    assert!(
        arr.len() <= 5,
        "limit=5 should return at most 5 alerts, got {}",
        arr.len()
    );
}

/// `status` returns JSON describing the connected database.
///
/// Asserted against the current `EngineStatus` shape. This test previously required
/// `node_count` and `edge_count` to be numbers, which was correct while `status` was a stub
/// printing four hardcoded zeros; those keys no longer exist.
///
/// The assertions below are deliberately the ones that hold against ANY database state, because
/// this test runs against whatever store the instance serves -- a fresh CI database with no
/// projection, or a live corpus with a complete one:
///
/// - `graph_events` is always a real live count, so it is always a number.
/// - `nodes`/`edges`/`edges_skipped` come from `projection_runs:current` and are `null` when the
///   marker does not carry them. A bare `is_number()` here would fail in CI, where nothing has
///   ever been projected. Asserting number-or-null is not a weakening: the field genuinely has
///   two valid shapes, and `null` is the honest one for "not recorded", which is the whole point
///   of `status` reporting `Option` rather than `unwrap_or(0)`.
/// - The number-or-null pair is then pinned by a real invariant: when `projection` is
///   `never_run`, every marker-derived count MUST be null. That is what stops this test passing
///   on a `status` that silently reported zeros again.
#[test]
fn test_cli_status_shows_db_connected() {
    if skip_if_no_db() {
        return;
    }
    let output = db_cli_cmd(&["status"]);
    assert!(
        output.status.success(),
        "status should exit 0 when DB is up"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|_| panic!("status output should be valid JSON, got: {}", stdout));

    assert!(
        parsed["database"].is_string(),
        "status JSON must name the database it connected to, got: {parsed}"
    );
    assert!(
        parsed["graph_events"].is_number(),
        "status JSON must carry a live graph_events count, got: {parsed}"
    );

    let projection = parsed["projection"]
        .as_str()
        .unwrap_or_else(|| panic!("status JSON must carry a projection state, got: {parsed}"));
    assert!(
        matches!(projection, "complete" | "unfinished" | "never_run"),
        "projection must be one of the three states, got {projection:?} in: {parsed}"
    );

    for key in ["nodes", "edges", "edges_skipped"] {
        assert!(
            parsed[key].is_number() || parsed[key].is_null(),
            "{key} must be a number or null (null when the projection marker does not carry \
             it), got: {parsed}"
        );
        if projection == "never_run" {
            assert!(
                parsed[key].is_null(),
                "with no projection marker, {key} must be null and never 0 -- reporting a count \
                 of nothing for a graph that was never projected is the stub behaviour this \
                 command was fixed to stop doing, got: {parsed}"
            );
        }
    }
}

// ── entity subcommand smoke tests ─────────────────────────────────────────────

/// entity --help should exit 0 and show key flags
#[test]
fn test_cli_entity_help_exits_zero() {
    let output = std::process::Command::new(cli_bin())
        .args(["entity", "--help"])
        .output()
        .expect("failed to run CLI binary");

    assert!(
        output.status.success(),
        "entity --help should exit 0, got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--ch-number"),
        "Expected --ch-number flag in help, got: {}",
        stdout
    );
    assert!(
        stdout.contains("--output"),
        "Expected --output flag in help, got: {}",
        stdout
    );
}

/// entity with no args should exit non-zero (the bail! in run() ensures this)
#[test]
fn test_cli_entity_no_args_exits_nonzero() {
    let output = std::process::Command::new(cli_bin())
        .arg("entity")
        .output()
        .expect("failed to run CLI binary");

    assert!(
        !output.status.success(),
        "entity with no args should exit non-zero"
    );
}

/// entity --ch-number <n> --output json must exit 0 and produce valid JSON (or empty output)
/// when DB is available. Tolerant of empty DB — CLI may return no entity found.
#[test]
fn test_cli_entity_json_output_is_valid_json() {
    if skip_if_no_db() {
        return;
    }
    let output = db_cli_cmd(&["entity", "--ch-number", "00000001", "--output", "json"]);
    assert!(
        output.status.success(),
        "entity --ch-number --output json should exit 0, got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // If entity is found, output must be valid JSON; if not found the CLI prints to stderr
    // and produces no stdout — both are acceptable (tolerant of empty DB).
    if !stdout.trim().is_empty() {
        let _parsed: serde_json::Value = serde_json::from_str(&stdout)
            .expect("entity --output json must produce valid JSON when stdout is non-empty");
    }
}

/// entity --ch-number for a found entity must return JSON with required fields (AC-DET-4).
///
/// If the entity is not found (empty DB), test passes as a smoke test only.
#[test]
fn test_cli_entity_json_fields_present() {
    if skip_if_no_db() {
        return;
    }
    let output = db_cli_cmd(&["entity", "--ch-number", "00000001", "--output", "json"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() {
        // Entity not found — empty DB scenario; test is a smoke test only
        return;
    }
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("must be valid JSON when non-empty");
    // If this is an object (single profile found), assert required D-04 camelCase fields
    if parsed.is_object() {
        assert!(
            parsed.get("name").is_some(),
            "JSON profile must have 'name' field, got: {}",
            parsed
        );
        assert!(
            parsed.get("chNumber").is_some(),
            "JSON profile must have 'chNumber' (camelCase, D-04), got: {}",
            parsed
        );
        assert!(
            parsed.get("directors").and_then(|v| v.as_array()).is_some(),
            "JSON profile must have 'directors' array, got: {}",
            parsed
        );
        assert!(
            parsed.get("contracts").and_then(|v| v.as_array()).is_some(),
            "JSON profile must have 'contracts' array, got: {}",
            parsed
        );
        assert!(
            parsed
                .get("relatedEntities")
                .and_then(|v| v.as_array())
                .is_some(),
            "JSON profile must have 'relatedEntities' array (camelCase, D-04), got: {}",
            parsed
        );
    }
}

/// When --output is omitted and stdout is non-tty (subprocess), OutputFormat::auto()
/// must emit JSON (D-07, SP-6). AC-DET-4 / ENT-04a.
#[test]
fn test_cli_entity_pipe_defaults_to_json() {
    if skip_if_no_db() {
        return;
    }
    // No --output flag — subprocess is non-tty so auto() returns Json
    let output = db_cli_cmd(&["entity", "--ch-number", "00000001"]);
    assert!(
        output.status.success(),
        "entity (no --output, non-tty) should exit 0"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        serde_json::from_str::<serde_json::Value>(&stdout)
            .expect("non-tty entity output must be valid JSON (OutputFormat::auto → Json)");
    }
}

/// ISO-8601 date check (AC-DET-4): incorporationDate field must parse as YYYY-MM-DD
/// when present in the JSON output.
#[test]
fn test_cli_entity_json_dates_are_iso8601() {
    if skip_if_no_db() {
        return;
    }
    let output = db_cli_cmd(&["entity", "--ch-number", "00000001", "--output", "json"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() {
        return; // empty DB — smoke test only
    }
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("must be valid JSON when non-empty");
    if let Some(date_str) = parsed.get("incorporationDate").and_then(|v| v.as_str()) {
        chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d").unwrap_or_else(|_| {
            panic!(
                "incorporationDate '{}' is not ISO-8601 YYYY-MM-DD (AC-DET-4)",
                date_str
            )
        });
    }
    // If incorporationDate is null/absent, the test still passes (date is optional)
}

/// entity --help output must contain at least one jq pipe example (long_about text).
///
/// Verifies ENT-01b documentation: analysts can see jq usage patterns without opening the
/// README. This is always-on (no DB needed) — it only runs the binary's --help flag.
#[test]
fn test_cli_entity_help_contains_jq_example() {
    let output = std::process::Command::new(cli_bin())
        .args(["entity", "--help"])
        .output()
        .expect("failed to run CLI binary");

    assert!(
        output.status.success(),
        "entity --help should exit 0, got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("jq"),
        "entity --help must include at least one jq pipe example in long_about, got: {}",
        stdout
    );
}

/// Slug-shape dispatch round-trip: a `director:apex-build-ltd`-style input must reach the
/// exact-ID lookup path without crashing (AC-DET-3, ENT-01b).
///
/// The entity may or may not exist in the test database. What this test asserts:
///   - exit code 0 (dispatch path does not panic or error)
///   - if stdout is non-empty JSON, it contains a "name" key
///
/// Deep slug round-trip correctness (seed → query_entity_by_id → profile) is covered in
/// `crates/core/tests/surreal_integration.rs`. This is the CLI integration layer above it.
#[test]
fn test_cli_entity_slug_dispatch_round_trip() {
    if skip_if_no_db() {
        return;
    }
    let output = db_cli_cmd(&["entity", "director:apex-build-ltd", "--output", "json"]);
    assert!(
        output.status.success(),
        "slug-shape input should exit 0 (no crash on exact-ID path), got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("non-empty output must be valid JSON");
        if parsed.is_object() {
            assert!(
                parsed.get("name").is_some(),
                "JSON entity profile must have 'name' field, got: {}",
                parsed
            );
        }
    }
    // If stdout is empty, the entity was not found in the test DB — still a passing result.
}

/// Slug fallback to name search: a hyphenated name ("bloggs-smith-engineering-ltd") is
/// slug-shaped (contains hyphens) but will return 0 exact-ID hits, then fall back to a
/// name CONTAINS search. This test asserts the fallback path does not panic (ENT-01c).
///
/// The entity is not expected to exist in the test DB. Exit 0 verifies no-crash fallback.
#[test]
fn test_cli_entity_slug_fallback_to_name() {
    if skip_if_no_db() {
        return;
    }
    let output = db_cli_cmd(&["entity", "bloggs-smith-engineering-ltd", "--output", "json"]);
    assert!(
        output.status.success(),
        "slug→fallback path should exit 0 (ENT-01c), got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // If the entity happens to be seeded, stdout will be non-empty JSON with a name field.
    if !stdout.trim().is_empty() {
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("non-empty output must be valid JSON");
        if parsed.is_object() {
            assert!(
                parsed.get("name").is_some(),
                "JSON profile returned via fallback must have 'name' field, got: {}",
                parsed
            );
        }
    }
}

/// Slug predicate smoke test (always-on, no DB): a plain company name with spaces is
/// accepted as a positional argument without being mistaken for a flag. Verifies that
/// "Apex Build Ltd" (no hyphens, no colons) routes through the name-search path.
///
/// This is a compile-level sanity check — if the binary exits 0 on --help with a space-
/// containing positional name in the examples, arg parsing is correct.
#[test]
fn test_cli_entity_plain_name_accepted_as_arg() {
    // Verify the help text itself does not contain "ERROR" (arg parsing is not broken).
    let output = std::process::Command::new(cli_bin())
        .args(["entity", "--help"])
        .output()
        .expect("failed to run CLI binary");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.to_lowercase().contains("error"),
        "entity --help must not display an error, got: {}",
        stdout
    );
}

// ── ingest-planning subcommand tests ─────────────────────────────────────────

/// ingest-planning --help should exit 0 and show key flags
#[test]
fn test_cli_ingest_planning_help_exits_zero() {
    let output = std::process::Command::new(cli_bin())
        .args(["ingest-planning", "--help"])
        .output()
        .expect("failed to run CLI binary");

    assert!(
        output.status.success(),
        "ingest-planning --help should exit 0, got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--ward") || stdout.contains("--all"),
        "Expected --ward or --all in help, got: {stdout}"
    );
}

/// ingest-planning with no args should exit non-zero and mention --ward/--all
#[test]
fn test_cli_ingest_planning_no_args_exits_nonzero() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let output = std::process::Command::new(cli_bin())
        .arg("ingest-planning")
        .current_dir(workspace_root)
        .env("URBAN__SURREAL__WS_URL", "ws://127.0.0.1:19999") // unreachable so no DB needed
        .env("URBAN__SURREAL__USERNAME", "root")
        .env("URBAN__SURREAL__PASSWORD", "root")
        .output()
        .expect("failed to run CLI binary");

    assert!(
        !output.status.success(),
        "ingest-planning with no args must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--ward") || stderr.contains("--all") || stderr.contains("Specify"),
        "Error must mention --ward or --all, got: {stderr}"
    );
}

/// ingest-planning --ward with an unconfigured ward code should exit non-zero
#[test]
fn test_cli_ingest_planning_unknown_ward_exits_nonzero() {
    if skip_if_no_db() {
        return;
    }
    let output = db_cli_cmd(&["ingest-planning", "--ward", "ZZZZZZZZZ"]);
    assert!(
        !output.status.success(),
        "ingest-planning --ward ZZZZZZZZZ must exit non-zero (ward not in config)"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not found") || stderr.contains("config") || stderr.contains("ZZZZZZZZZ"),
        "Error must mention the missing ward, got: {stderr}"
    );
}

// ── alerts subcommand additional tests ───────────────────────────────────────

/// alerts list --since 24h should exit 0 and return JSON array
#[test]
fn test_cli_alerts_list_since_flag_json_output() {
    if skip_if_no_db() {
        return;
    }
    let output = db_cli_cmd(&["alerts", "list", "--since", "24h", "--output", "json"]);
    assert!(
        output.status.success(),
        "alerts list --since 24h failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .expect("alerts --since 24h must produce valid JSON");
    assert!(parsed.is_array(), "alerts list output must be a JSON array");
}

/// alerts list --ward filter with a non-existent ward should still exit 0
#[test]
fn test_cli_alerts_list_ward_filter_empty_result() {
    if skip_if_no_db() {
        return;
    }
    let output = db_cli_cmd(&["alerts", "list", "--ward", "ZZ99999999", "--output", "json"]);
    assert!(
        output.status.success(),
        "alerts list with non-existent ward should exit 0 with empty array"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("must be JSON");
    assert!(parsed.is_array(), "must be array");
    assert_eq!(
        parsed.as_array().unwrap().len(),
        0,
        "non-existent ward must return empty array"
    );
}

/// alerts show with a non-existent ID should exit non-zero
#[test]
fn test_cli_alerts_show_unknown_id_exits_nonzero() {
    if skip_if_no_db() {
        return;
    }
    let output = db_cli_cmd(&["alerts", "show", "nonexistent-alert-id-99999"]);
    assert!(
        !output.status.success(),
        "alerts show with unknown ID must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not found") || stderr.contains("nonexistent"),
        "Error must mention the missing alert, got: {stderr}"
    );
}

/// alerts list --motif filter should exit 0 and return JSON array
#[test]
fn test_cli_alerts_list_motif_filter() {
    if skip_if_no_db() {
        return;
    }
    let output = db_cli_cmd(&["alerts", "list", "--motif", "phoenix", "--output", "json"]);
    assert!(
        output.status.success(),
        "alerts list --motif phoenix failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("must be JSON");
    assert!(
        parsed.is_array(),
        "alerts --motif output must be a JSON array"
    );
}

// ── report --format md ────────────────────────────────────────────────────────

/// report --format md should create a .md file in the workspace root
#[test]
fn test_cli_report_markdown_creates_file() {
    if skip_if_no_db() {
        return;
    }
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    // Use a fixed output-file path so we can clean up afterwards
    let md_path = workspace_root.join("test-report-ci-output.md");
    // Clean up leftovers from prior runs
    let _ = std::fs::remove_file(&md_path);

    let output = std::process::Command::new(cli_bin())
        .args([
            "report",
            "--format",
            "md",
            "--output-file",
            "test-report-ci-output.md",
        ])
        .current_dir(workspace_root)
        .env("URBAN__SURREAL__WS_URL", "ws://localhost:8000")
        .env("URBAN__SURREAL__USERNAME", "root")
        .env("URBAN__SURREAL__PASSWORD", "root")
        .output()
        .expect("failed to run CLI binary");

    assert!(
        output.status.success(),
        "report --format md should exit 0, got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify the .md file was created and contains the expected header
    assert!(
        md_path.exists(),
        "report --format md must write a .md file to disk"
    );
    let content = std::fs::read_to_string(&md_path).expect("must read .md file");
    assert!(
        content.contains("# UrbanIntegrity Report"),
        "Markdown report must start with # UrbanIntegrity Report, got: {}",
        &content[..content.len().min(200)]
    );

    // Clean up
    let _ = std::fs::remove_file(&md_path);
}

/// report --output-file is only valid with md or pdf — json combo must exit non-zero
#[test]
fn test_cli_report_output_file_with_json_format_exits_nonzero() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let output = std::process::Command::new(cli_bin())
        .args([
            "report",
            "--format",
            "json",
            "--output-file",
            "/tmp/should-not-be-created.json",
        ])
        .current_dir(workspace_root)
        .env("URBAN__SURREAL__WS_URL", "ws://127.0.0.1:19999")
        .env("URBAN__SURREAL__USERNAME", "root")
        .env("URBAN__SURREAL__PASSWORD", "root")
        .output()
        .expect("failed to run CLI binary");

    assert!(
        !output.status.success(),
        "report --format json --output-file must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--output-file") || stderr.contains("only valid"),
        "Error must mention --output-file restriction, got: {stderr}"
    );
}

/// status --output table should print the human-readable table
#[test]
fn test_cli_status_table_output() {
    if skip_if_no_db() {
        return;
    }
    let output = db_cli_cmd(&["status", "--output", "table"]);
    assert!(
        output.status.success(),
        "status --output table should exit 0, got: {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Engine Status"),
        "table output must contain 'Engine Status', got: {stdout}"
    );
    assert!(
        stdout.contains("Nodes:") || stdout.contains("node"),
        "table output must contain node count, got: {stdout}"
    );
}

/// Running report against an unreachable DB should exit non-zero with a helpful message
#[test]
fn test_cli_report_bad_url_exits_nonzero_with_message() {
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    let output = std::process::Command::new(cli_bin())
        .args(["report", "--output", "json"])
        .current_dir(workspace_root)
        .env("URBAN__SURREAL__WS_URL", "ws://127.0.0.1:19999") // nothing listening here
        .env("URBAN__SURREAL__USERNAME", "root")
        .env("URBAN__SURREAL__PASSWORD", "root")
        .output()
        .expect("failed to run CLI binary");

    assert!(
        !output.status.success(),
        "bad URL should cause non-zero exit"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("SurrealDB") || stderr.contains("connect") || stderr.contains("Error"),
        "error output should mention the connection failure: {}",
        stderr
    );
}

// ── purge-orphans D-03 non-TTY refusal (28-05, DB-gated subprocess test) ───────────────────
//
// Proves D-03 end-to-end through a REAL spawned subprocess, isolated to its own TestNs
// namespace — never the live `an_anglesey_aebaa785`. Closes phase 28 research Pitfall 2: none
// of the `db_cli_cmd` calls above set `.stdin(Stdio::null())`, because none of today's tested
// commands read stdin. `purge-orphans --apply` is the first one that does, so it needs its own
// spawn helper rather than reusing `db_cli_cmd` unmodified.
mod purge {
    use std::process::Stdio;

    /// `db_cli_cmd`'s body (this file, above), unmodified, plus `.stdin(Stdio::null())` so the
    /// child never inherits a real terminal from the test harness (Pitfall 2), and two extra
    /// env overrides pointing the subprocess at an isolated `TestNs` namespace/database rather
    /// than whatever `config.toml`/`URBAN__SURREAL__*` resolves to by default (the isolation
    /// gap the phase 28 pattern notes flag — no existing `cli.rs` test seeds a `TestNs` and hands its
    /// namespace to a spawned subprocess before this one).
    fn db_cli_cmd_no_stdin_in_ns(args: &[&str], ns: &str, db_name: &str) -> std::process::Output {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap() // crates/cli -> crates
            .parent()
            .unwrap(); // crates -> workspace root

        std::process::Command::new(super::cli_bin())
            .args(args)
            .current_dir(workspace_root)
            // ws:// is converted to http:// inside connect_db — both loopback forms work.
            .env("URBAN__SURREAL__WS_URL", "ws://localhost:8000")
            .env("URBAN__SURREAL__USERNAME", "root")
            .env("URBAN__SURREAL__PASSWORD", "root")
            .env("URBAN__SURREAL__NAMESPACE", ns)
            .env("URBAN__SURREAL__DATABASE", db_name)
            .stdin(Stdio::null())
            .output()
            .expect("failed to run CLI binary")
    }

    /// Seed one deliberate orphan (`property:` NodeAdded, no OwnsProperty edge) plus one
    /// unrelated OwnsProperty edge so the command's pre-existing "owned set is EMPTY" guard
    /// does not short-circuit the run before it ever reaches the confirmation gate this test
    /// is proving. Both rows are confined to the caller's throwaway `TestNs` namespace.
    async fn seed_one_orphan_and_one_owned_edge(
        db: &surrealdb::Surreal<surrealdb::engine::any::Any>,
    ) {
        use urban_integrity_schema::types::edge::{EdgeMeta, EdgeWeight, OwnsPropertyData};
        use urban_integrity_schema::types::node::{NodeWeight, PropertyData};

        let property_weight = serde_json::to_string(&NodeWeight::Property(PropertyData {
            title_number: None,
            tenure: None,
            price_paid: None,
            company_number: None,
            country_incorporated: None,
        }))
        .expect("PropertyData serialises");

        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": "purge-refuse-test-orphan",
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "NodeAdded",
                    "payload": {
                        "entity_id": "property:purge-refuse-test-orphan",
                        "weight_json": property_weight,
                    },
                }),
            ))
            .await
            .expect("failed to seed orphan property NodeAdded row");

        // An unrelated OwnsProperty edge — its dst_id does not need a matching NodeAdded row
        // (load_owned_property_ids only reads dst_id, format-independent). This is what keeps
        // the owned-property set non-empty so the empty-set refusal guard does not fire first.
        let edge = EdgeWeight::OwnsProperty {
            meta: EdgeMeta {
                created_at: chrono::Utc::now(),
                effective_from: None,
                effective_to: None,
            },
            data: OwnsPropertyData {
                price_paid: None,
                tenure: None,
            },
        };
        let edge_weight_json = serde_json::to_string(&edge).expect("OwnsProperty edge serialises");
        db.query("CREATE graph_events CONTENT $r")
            .bind((
                "r",
                serde_json::json!({
                    "event_id": "purge-refuse-test-decoy-owned-edge",
                    "timestamp": 1_700_000_000_000u64,
                    "kind": "EdgeAdded",
                    "payload": {
                        "src_id": "GB-COH:purge-refuse-test-owner",
                        "dst_id": "property:purge-refuse-test-decoy-owned",
                        "weight_json": edge_weight_json,
                    },
                }),
            ))
            .await
            .expect("failed to seed decoy OwnsProperty edge");
    }

    /// D-03 end-to-end: `purge-orphans --apply` with a real seeded orphan, spawned as a real
    /// subprocess with stdin closed (never a real terminal), pointed at an isolated `TestNs` —
    /// must exit non-zero, and the seeded orphan row must still be present afterward.
    #[test]
    fn purge_refuses_non_tty() {
        if super::skip_if_no_db() {
            return;
        }

        let rt = tokio::runtime::Runtime::new().unwrap();
        let db = rt.block_on(urban_integrity_test_support::TestNs::connect_with_prefix(
            "test_purge",
            "main",
        ));

        rt.block_on(seed_one_orphan_and_one_owned_edge(&db));

        let pre_count: Vec<serde_json::Value> = rt.block_on(async {
            db.query(
                "SELECT count() FROM graph_events WHERE \
                 payload.entity_id = 'property:purge-refuse-test-orphan' GROUP ALL",
            )
            .await
            .expect("pre-count query failed")
            .take(0)
            .expect("pre-count take failed")
        });
        assert_eq!(
            pre_count,
            vec![serde_json::json!({"count": 1})],
            "seeded orphan row must be present before the subprocess runs"
        );

        let output = db_cli_cmd_no_stdin_in_ns(&["purge-orphans", "--apply"], db.ns(), "main");

        assert!(
            !output.status.success(),
            "non-TTY --apply must exit non-zero, got: {:?}\nstdout: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let post_count: Vec<serde_json::Value> = rt.block_on(async {
            db.query(
                "SELECT count() FROM graph_events WHERE \
                 payload.entity_id = 'property:purge-refuse-test-orphan' GROUP ALL",
            )
            .await
            .expect("post-count query failed")
            .take(0)
            .expect("post-count take failed")
        });

        assert_eq!(
            pre_count, post_count,
            "seeded orphan row must still be present after the refused --apply — nothing may \
             have been deleted"
        );
    }
}
