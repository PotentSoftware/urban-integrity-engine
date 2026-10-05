//! `GlossaryPage` — the searchable glossary (`/help/glossary`, D-14) rendering the same parsed
//! `docs/abbreviations.md` table Plan 03's `glossary.rs` and Plan 04's `<abbr>` component read.
//! "One source, two renderers": this page is the second renderer, never a separate copy of the
//! glossary content (see the phase 43 UI spec's "Glossary page" section).

use leptos::prelude::*;

use crate::breadcrumb::Breadcrumb;
use crate::glossary::{self, GlossaryRow, GlossarySection};

/// One parsed segment of a `meaning`/`notes` cell's inline markup (checkpoint fix item 2,
/// 43-09 Task 3 review): `docs/abbreviations.md`'s raw Markdown source — `**bold**`, `` `code` ``
/// spans and the escaped pipe `\|` a 3-column table cell needs to contain a literal `|` — was
/// reaching the DOM as literal asterisks/backticks/backslashes instead of being rendered. This is
/// a tiny, deliberately non-recursive inline parser (no new crate dependency, keeps the wasm
/// bundle small per D-25 / the phase 43 design notes' size-budget note) — it does NOT nest (`**a `b` c**`
/// parses the whole span as one `Bold("a `b` c")`, never a `Bold` containing a `Code`), which is
/// sufficient for every real row `docs/abbreviations.md` contains.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InlineSegment {
    /// Plain text, rendered verbatim.
    Text(String),
    /// `**text**` — rendered as `<strong>`.
    Bold(String),
    /// `` `text` `` — rendered as `<code class="font-mono">`.
    Code(String),
}

/// Finds the index of the next `**` (a two-character delimiter) starting at or after `from`, or
/// `None` if the input runs out first — an unterminated `**bold` (no closing marker) is exactly
/// the `None` case, and the caller falls back to literal text for it.
fn find_double_star(chars: &[char], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < chars.len() {
        if chars[i] == '*' && chars[i + 1] == '*' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Finds the index of the next single `target` character starting at or after `from` — used for
/// the backtick code-span delimiter. `None` (an unterminated `` `code ``) falls back to literal
/// text, same discipline as [`find_double_star`].
fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|&i| chars[i] == target)
}

/// Parses `input` into a sequence of [`InlineSegment`]s (checkpoint fix item 2): `**x**` becomes
/// `Bold(x)`, `` `x` `` becomes `Code(x)`, `\|` becomes a literal `|` folded into the surrounding
/// text, and every other character is literal text. An unterminated `**`/`` ` `` marker (no
/// matching close before the end of the string) is emitted as literal text — the marker
/// characters themselves, never dropped and never left half-parsed.
fn parse_inline_markup(input: &str) -> Vec<InlineSegment> {
    let chars: Vec<char> = input.chars().collect();
    let mut segments = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];

        if c == '\\' && i + 1 < chars.len() && chars[i + 1] == '|' {
            buf.push('|');
            i += 2;
            continue;
        }

        if c == '*' && i + 1 < chars.len() && chars[i + 1] == '*' {
            match find_double_star(&chars, i + 2) {
                Some(end) => {
                    if !buf.is_empty() {
                        segments.push(InlineSegment::Text(std::mem::take(&mut buf)));
                    }
                    let inner: String = chars[i + 2..end].iter().collect();
                    segments.push(InlineSegment::Bold(unescape_pipes(&inner)));
                    i = end + 2;
                }
                None => {
                    // Unterminated `**` — literal, per this function's documented fallback.
                    buf.push('*');
                    buf.push('*');
                    i += 2;
                }
            }
            continue;
        }

        if c == '`' {
            match find_char(&chars, i + 1, '`') {
                Some(end) => {
                    if !buf.is_empty() {
                        segments.push(InlineSegment::Text(std::mem::take(&mut buf)));
                    }
                    let inner: String = chars[i + 1..end].iter().collect();
                    segments.push(InlineSegment::Code(unescape_pipes(&inner)));
                    i = end + 1;
                }
                None => {
                    // Unterminated backtick — literal, per this function's documented fallback.
                    buf.push('`');
                    i += 1;
                }
            }
            continue;
        }

        buf.push(c);
        i += 1;
    }

    if !buf.is_empty() {
        segments.push(InlineSegment::Text(buf));
    }
    segments
}

/// The table-cell pipe escape `\|` is a Markdown-table artefact, not content, so it must fold to
/// a literal `|` inside `**bold**` and `` `code` `` spans exactly as it does in plain text —
/// `` `anomalies list\|show\|review` `` reads as `anomalies list|show|review`.
fn unescape_pipes(inner: &str) -> String {
    inner.replace("\\|", "|")
}

/// Renders `input`'s parsed [`InlineSegment`]s into a Leptos fragment: `Text` verbatim, `Bold` as
/// `<strong>`, `Code` as `<code class="font-mono">` — applied to both `meaning` and `notes` cells
/// (checkpoint fix item 2). The glossary search filter (`filter_sections`) is unaffected: it
/// matches against the raw `row.meaning`/`row.abbr` strings, never these parsed segments.
fn render_inline_markup(input: &str) -> impl IntoView {
    parse_inline_markup(input)
        .into_iter()
        .map(|segment| match segment {
            InlineSegment::Text(text) => text.into_any(),
            InlineSegment::Bold(text) => view! { <strong>{text}</strong> }.into_any(),
            InlineSegment::Code(text) => view! { <code class="font-mono">{text}</code> }.into_any(),
        })
        .collect_view()
}

/// Filters `sections` by `query` (case-insensitive substring match against each row's `abbr` OR
/// `meaning`), keeping only matching rows. A section with zero remaining matching rows is
/// dropped entirely from the result — never rendered with an empty row list. An empty `query`
/// returns every section unchanged (every row matches the empty substring).
pub fn filter_sections(
    sections: &'static [GlossarySection],
    query: &str,
) -> Vec<(&'static str, Vec<&'static GlossaryRow>)> {
    let needle = query.to_lowercase();
    sections
        .iter()
        .filter_map(|section| {
            let rows: Vec<&'static GlossaryRow> = section
                .rows
                .iter()
                .filter(|row| {
                    row.abbr.to_lowercase().contains(&needle)
                        || row.meaning.to_lowercase().contains(&needle)
                })
                .collect();
            if rows.is_empty() {
                None
            } else {
                Some((section.heading, rows))
            }
        })
        .collect()
}

/// The fixed "no results" copy, per the phase 43 UI spec's Copywriting Contract: `"No abbreviations
/// match \"{query}\"."` — interpolates the exact query string.
pub fn no_match_text(query: &str) -> String {
    format!("No abbreviations match \"{query}\".")
}

/// `#[component] GlossaryPage`: breadcrumb (`Home / Help / Glossary`), page title, a live
/// case-insensitive search box, and the filtered glossary table(s) below — or the fixed
/// no-match text, centered, when nothing matches.
#[component]
#[allow(non_snake_case)]
pub fn GlossaryPage() -> impl IntoView {
    let query = RwSignal::new(String::new());

    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-lg); padding-top: var(--space-xl);">
            <div style="display: flex; flex-direction: column; gap: var(--space-md);">
                <Breadcrumb />
                <h1 class="text-heading">"Glossary"</h1>
            </div>

            <div style="position: relative; display: flex; align-items: center; height: 44px; background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: 0 var(--space-sm);">
                <input
                    type="text"
                    class="text-body"
                    placeholder="Search abbreviations…"
                    style="flex: 1; background: transparent; border: none; outline: none; color: var(--foreground); height: 100%;"
                    prop:value=move || query.get()
                    on:input=move |ev| query.set(event_target_value(&ev))
                />
                <Show when=move || !query.get().is_empty()>
                    <button
                        aria-label="Clear search"
                        style="width: 44px; height: 44px; display: flex; align-items: center; justify-content: center; background: transparent; border: none; color: var(--muted-foreground); cursor: pointer;"
                        on:click=move |_| query.set(String::new())
                    >
                        "×"
                    </button>
                </Show>
            </div>

            {move || {
                let current_query = query.get();
                let filtered = filter_sections(glossary::sections(), &current_query);
                if filtered.is_empty() {
                    view! {
                        <p class="text-body" style="color: var(--muted-foreground); text-align: center;">
                            {no_match_text(&current_query)}
                        </p>
                    }.into_any()
                } else {
                    filtered
                        .into_iter()
                        .map(|(heading, rows)| {
                            view! {
                                <div style="display: flex; flex-direction: column; gap: var(--space-sm);">
                                    <h2 class="text-heading">{heading}</h2>
                                    <table style="width: 100%; border-collapse: collapse;">
                                        <thead>
                                            <tr style="border-bottom: 1px solid var(--border);">
                                                <th class="text-label" style="color: var(--muted-foreground); text-align: left; padding: var(--space-xs) var(--space-sm);">"Abbr."</th>
                                                <th class="text-label" style="color: var(--muted-foreground); text-align: left; padding: var(--space-xs) var(--space-sm);">"Meaning"</th>
                                                <th class="text-label" style="color: var(--muted-foreground); text-align: left; padding: var(--space-xs) var(--space-sm);">"Notes"</th>
                                            </tr>
                                        </thead>
                                        <tbody>
                                            {rows.into_iter().map(|row| view! {
                                                <tr style="border-bottom: 1px solid var(--border);">
                                                    <td class="font-mono text-body" style="padding: var(--space-xs) var(--space-sm);">{row.abbr}</td>
                                                    <td class="text-body" style="padding: var(--space-xs) var(--space-sm);">{render_inline_markup(row.meaning)}</td>
                                                    <td class="text-body" style="padding: var(--space-xs) var(--space-sm);">{render_inline_markup(row.notes)}</td>
                                                </tr>
                                            }).collect_view()}
                                        </tbody>
                                    </table>
                                </div>
                            }
                        })
                        .collect_view()
                        .into_any()
                }
            }}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Inline markup parser (checkpoint fix item 2) ────────────────────────────────────────

    #[test]
    fn parses_a_bold_span() {
        let segments = parse_inline_markup("**keys are typed**");
        assert_eq!(
            segments,
            vec![InlineSegment::Bold("keys are typed".to_string())]
        );
    }

    #[test]
    fn parses_a_code_span() {
        let segments = parse_inline_markup("`stream.companieshouse.gov.uk`");
        assert_eq!(
            segments,
            vec![InlineSegment::Code(
                "stream.companieshouse.gov.uk".to_string()
            )]
        );
    }

    #[test]
    fn parses_an_escaped_pipe_as_a_literal_pipe_folded_into_surrounding_text() {
        let segments = parse_inline_markup(r"before \| after");
        assert_eq!(
            segments,
            vec![InlineSegment::Text("before | after".to_string())]
        );
    }

    #[test]
    fn parses_mixed_bold_code_and_plain_text_in_order() {
        let segments = parse_inline_markup("see `CH` — **required** always");
        assert_eq!(
            segments,
            vec![
                InlineSegment::Text("see ".to_string()),
                InlineSegment::Code("CH".to_string()),
                InlineSegment::Text(" — ".to_string()),
                InlineSegment::Bold("required".to_string()),
                InlineSegment::Text(" always".to_string()),
            ]
        );
    }

    /// The escaped pipe folds to a literal `|` INSIDE a code span too — the anomaly row's
    /// `` `anomalies list\|show\|review` `` must not render its backslashes.
    #[test]
    fn unescapes_pipes_inside_code_and_bold_spans() {
        assert_eq!(
            parse_inline_markup("`list\\|show`"),
            vec![InlineSegment::Code("list|show".to_string())]
        );
        assert_eq!(
            parse_inline_markup("**a\\|b**"),
            vec![InlineSegment::Bold("a|b".to_string())]
        );
    }

    /// An unterminated `**`/`` ` `` marker falls back to literal text — never dropped, never
    /// left half-parsed as an unclosed `Bold`/`Code` segment.
    #[test]
    fn unterminated_bold_marker_falls_back_to_literal_text() {
        let segments = parse_inline_markup("**not closed");
        assert_eq!(
            segments,
            vec![InlineSegment::Text("**not closed".to_string())]
        );
    }

    #[test]
    fn unterminated_code_marker_falls_back_to_literal_text() {
        let segments = parse_inline_markup("`not closed");
        assert_eq!(
            segments,
            vec![InlineSegment::Text("`not closed".to_string())]
        );
    }

    /// Empty query returns every section unchanged — "unchanged" meaning every section that has
    /// at least one row in the raw parsed data still has exactly that row set; a section that is
    /// already empty in the raw data (e.g. the graph node-key-prefix section, per `glossary.rs`'s
    /// own doc comment: node-key/compound tokens are out of `abbr`'s bare-alphanumeric scope, so
    /// it contributes zero rows even before filtering) is dropped by the same "never render an
    /// empty section" rule this filter always applies, empty query or not.
    #[test]
    fn empty_query_returns_every_section_unchanged() {
        let sections = glossary::sections();
        let filtered = filter_sections(sections, "");
        let non_empty_sections: usize = sections.iter().filter(|s| !s.rows.is_empty()).count();
        assert_eq!(filtered.len(), non_empty_sections);
        let total_rows: usize = filtered.iter().map(|(_, rows)| rows.len()).sum();
        let expected_rows: usize = sections.iter().map(|s| s.rows.len()).sum();
        assert_eq!(total_rows, expected_rows);
    }

    /// Case-insensitive match on meaning text; sections with no matching row are excluded
    /// entirely (not just hidden by their rows being absent).
    #[test]
    fn case_insensitive_match_on_meaning_finds_ch_row() {
        let filtered = filter_sections(glossary::sections(), "companies house");
        let found = filtered
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .any(|row| row.abbr == "CH");
        assert!(found, "expected CH row to match on its meaning text");

        // Every returned section actually has at least one matching row.
        for (heading, rows) in &filtered {
            assert!(
                !rows.is_empty(),
                "section {heading} was returned with zero matching rows"
            );
        }
    }

    /// A query matching nothing returns zero sections, zero rows.
    #[test]
    fn no_match_query_returns_empty_result() {
        let filtered = filter_sections(glossary::sections(), "zzqxnomatch");
        assert!(filtered.is_empty());
        assert_eq!(
            no_match_text("zzqxnomatch"),
            "No abbreviations match \"zzqxnomatch\"."
        );
    }

    /// Matches on the abbreviation token itself, not only the meaning text — case-insensitive
    /// against BOTH `abbr` and `meaning`.
    #[test]
    fn matches_on_abbreviation_token_case_insensitively() {
        let filtered = filter_sections(glossary::sections(), "ch");
        let found = filtered
            .iter()
            .flat_map(|(_, rows)| rows.iter())
            .any(|row| row.abbr == "CH");
        assert!(
            found,
            "lowercase query 'ch' must match the 'CH' row's abbreviation token"
        );
    }

    /// Structural proof that `filter_sections` never returns a section with an empty row vec:
    /// constructing a query that matches in only some sections must never leave an empty-vec
    /// entry behind for a non-matching section.
    #[test]
    fn never_returns_a_section_with_an_empty_row_vec() {
        let filtered = filter_sections(glossary::sections(), "companies house");
        for (_, rows) in &filtered {
            assert!(!rows.is_empty());
        }
    }
}
