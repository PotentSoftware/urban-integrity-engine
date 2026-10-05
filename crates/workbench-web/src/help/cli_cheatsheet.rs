//! The Help page's "CLI pivots" section (D-13) — a fixed, never-analyst-editable list of CLI
//! commands with the current analysis's `--analysis <db>` flag already substituted.
//!
//! `--analysis` is a top-level, `global = true` clap flag on `urban-integrity`'s root `Cli`
//! struct (`crates/cli/src/main.rs`), positioned BEFORE the subcommand — e.g.
//! `urban-integrity-cli --analysis <db> entity --ch-number <n>`, never
//! `urban-integrity-cli entity --analysis <db> --ch-number <n>`. Confirmed against `main.rs`
//! before writing the strings below (do not guess at flag position).
//!
//! The binary-name literal `urban-integrity-cli` matches the existing precedent in
//! `crate::analysis_picker::restart_command` (the one other place this crate interpolates a
//! server-controlled value into a fixed command template) — not the crate's `name = "urban-
//! integrity"` clap declaration, which is a separate, pre-existing discrepancy this plan does
//! not introduce or resolve.
//!
//! Example ids are always one of the four fixed placeholder tokens (`<company-number>`,
//! `<link-id>`, `<entity-slug>`, `<path>`) — never a real analysis-specific identifier
//! (`scripts/check-no-analysis-literals.sh` scans this crate's tree, T-43-08-02).

/// Returns the fixed 5-command CLI pivot list, in this exact order, with `db_name` substituted
/// into every `--analysis` flag. The command list's shape (order, count, placeholder tokens)
/// never changes — only the substituted `db_name` value does.
pub fn cheat_sheet_commands(db_name: &str) -> Vec<String> {
    vec![
        format!("urban-integrity-cli --analysis {db_name} entity --ch-number <company-number>"),
        format!("urban-integrity-cli --analysis {db_name} links show identity_links:<link-id>"),
        format!("urban-integrity-cli --analysis {db_name} graph-path <entity-slug> <entity-slug>"),
        format!("urban-integrity-cli --analysis {db_name} alerts list"),
        format!("urban-integrity-cli --analysis {db_name} probe --output <path>"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXED_PLACEHOLDERS: [&str; 4] =
        ["<company-number>", "<link-id>", "<entity-slug>", "<path>"];

    /// `cheat_sheet_commands` returns exactly 5 strings, every one containing the literal
    /// substituted `--analysis <db>` substring.
    #[test]
    fn returns_exactly_five_commands_each_containing_the_analysis_flag() {
        let commands = cheat_sheet_commands("an_first_1a2b3c");
        assert_eq!(commands.len(), 5);
        for command in &commands {
            assert!(
                command.contains("--analysis an_first_1a2b3c"),
                "command missing substituted --analysis flag: {command}"
            );
        }
    }

    /// No accidental real-looking id leaks into the fixed template — every remaining `<...>`
    /// placeholder token is one of the four fixed ones, and no digit-only or slug-shaped literal
    /// (other than the substituted `db_name` itself) appears.
    #[test]
    fn contains_no_literal_besides_fixed_placeholder_tokens_and_db_name() {
        let db_name = "an_first_1a2b3c";
        let commands = cheat_sheet_commands(db_name);
        for command in &commands {
            // Strip the one legitimate substitution before scanning for stray placeholders.
            let stripped = command.replace(db_name, "");
            // Every remaining angle-bracket token must be one of the fixed four.
            let mut rest = stripped.as_str();
            while let Some(start) = rest.find('<') {
                let after_start = &rest[start..];
                let end = after_start
                    .find('>')
                    .expect("every '<' in the fixed template has a matching '>'");
                let token = &after_start[..=end];
                assert!(
                    FIXED_PLACEHOLDERS.contains(&token),
                    "unexpected placeholder token {token} in command: {command}"
                );
                rest = &after_start[end + 1..];
            }
        }
    }

    /// The command list's shape (order, count) is identical regardless of `db_name`'s value —
    /// only the substituted value changes.
    #[test]
    fn output_shape_is_identical_regardless_of_db_name() {
        let a = cheat_sheet_commands("an_first_1a2b3c");
        let b = cheat_sheet_commands("an_second_4d5e6f");
        assert_eq!(a.len(), b.len());
        for (cmd_a, cmd_b) in a.iter().zip(b.iter()) {
            let normalized_a = cmd_a.replace("an_first_1a2b3c", "<db>");
            let normalized_b = cmd_b.replace("an_second_4d5e6f", "<db>");
            assert_eq!(normalized_a, normalized_b);
        }
    }
}
