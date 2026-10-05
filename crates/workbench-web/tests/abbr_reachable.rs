//! Proof that the `Abbr` component (D-03) is actually reachable from the shipped render tree,
//! host-side (`#[test]`, no wasm, no DB), in the same source-reading style as
//! `routes_render.rs`. Phase 43's verification found `abbr.rs` fully built and unit-tested but
//! with zero call sites — every page rendered bare abbreviations — and nothing failed, because
//! `scripts/check-abbreviations.sh` proves a token is *documented*, never that it is *rendered
//! inside `<abbr>`*. Two layers close that hole:
//!
//! 1. The two copy sites D-03 reaches today are pinned by name: the Help page's "CLI pivots"
//!    heading (full words, then the token in parentheses, inside `<Abbr>`) and the Home page's
//!    Help-card support line ("Graph algorithms and CLI pivots", token inside `<Abbr>`).
//! 2. A sweep over every shipped page source asserts that no `view!` string literal still
//!    carries a bare glossary abbreviation (a `format!` template's fixed text included) — so the
//!    next page that writes "CLI" (or any other
//!    glossary token) as plain text fails here, not in a browser six weeks later.
//!
//! Non-vacuity: replace either `<Abbr token=CLI_ABBR.to_string() />` with a bare `"CLI"`
//! literal and re-run — the named test AND the sweep both fail. Revert after confirming.

const HELP_RS: &str = include_str!("../src/help/mod.rs");
const HOME_RS: &str = include_str!("../src/home_page.rs");
const APP_RS: &str = include_str!("../src/app.rs");
const GLOSSARY_MD: &str = include_str!("../../../docs/abbreviations.md");

/// Every shipped page / component source the sweep covers, with its contents. `abbr.rs` and
/// `glossary.rs` are the mechanism itself; `cosmograph_spike.rs` is unmounted (43-09).
const SWEPT_SOURCES: &[(&str, &str)] = &[
    ("src/app.rs", APP_RS),
    ("src/home_page.rs", HOME_RS),
    ("src/help/mod.rs", HELP_RS),
    (
        "src/help/algorithms.rs",
        include_str!("../src/help/algorithms.rs"),
    ),
    (
        "src/help/glossary_page.rs",
        include_str!("../src/help/glossary_page.rs"),
    ),
    (
        "src/help/cli_cheatsheet.rs",
        include_str!("../src/help/cli_cheatsheet.rs"),
    ),
    (
        "src/analysis_page.rs",
        include_str!("../src/analysis_page.rs"),
    ),
    ("src/graph_page.rs", include_str!("../src/graph_page.rs")),
    (
        "src/graph_deep_link.rs",
        include_str!("../src/graph_deep_link.rs"),
    ),
    (
        "src/edge_palette.rs",
        include_str!("../src/edge_palette.rs"),
    ),
    ("src/breadcrumb.rs", include_str!("../src/breadcrumb.rs")),
    ("src/timestamp.rs", include_str!("../src/timestamp.rs")),
    ("src/copy_button.rs", include_str!("../src/copy_button.rs")),
    (
        "src/entity_ref_view.rs",
        include_str!("../src/entity_ref_view.rs"),
    ),
    (
        "src/verdict_badge.rs",
        include_str!("../src/verdict_badge.rs"),
    ),
    (
        "src/analysis_picker.rs",
        include_str!("../src/analysis_picker.rs"),
    ),
    ("src/theme.rs", include_str!("../src/theme.rs")),
    ("src/tool_rail.rs", include_str!("../src/tool_rail.rs")),
    (
        "src/entity_picker.rs",
        include_str!("../src/entity_picker.rs"),
    ),
    (
        "src/graph_results_list.rs",
        include_str!("../src/graph_results_list.rs"),
    ),
    (
        "src/graph_status_line.rs",
        include_str!("../src/graph_status_line.rs"),
    ),
    ("src/side_card.rs", include_str!("../src/side_card.rs")),
    (
        "src/neighbourhood.rs",
        include_str!("../src/neighbourhood.rs"),
    ),
    (
        "src/disambiguation.rs",
        include_str!("../src/disambiguation.rs"),
    ),
    ("src/profile/mod.rs", include_str!("../src/profile/mod.rs")),
    (
        "src/profile/as_at.rs",
        include_str!("../src/profile/as_at.rs"),
    ),
    (
        "src/profile/pivot.rs",
        include_str!("../src/profile/pivot.rs"),
    ),
    (
        "src/profile/sections.rs",
        include_str!("../src/profile/sections.rs"),
    ),
    (
        "src/profile/provenance_chip.rs",
        include_str!("../src/profile/provenance_chip.rs"),
    ),
    // the phase 45 UI spec names nine new modules that must be registered "before this phase ships".
    // `canvas_filters.rs` is one of them and was NOT registered when plan 45-14 created it — 1,582
    // lines of copy, unswept, with nothing able to say so. Plan 45-15 registers it alongside its
    // own three, and adds `every_shipped_source_is_registered_for_the_sweep` so the next unswept
    // module fails here rather than shipping.
    (
        "src/canvas_filters.rs",
        include_str!("../src/canvas_filters.rs"),
    ),
    (
        "src/investigation/mod.rs",
        include_str!("../src/investigation/mod.rs"),
    ),
    (
        "src/investigation/pin.rs",
        include_str!("../src/investigation/pin.rs"),
    ),
    (
        "src/investigation/notes.rs",
        include_str!("../src/investigation/notes.rs"),
    ),
    (
        "src/investigation/page.rs",
        include_str!("../src/investigation/page.rs"),
    ),
    ("src/motif_star.rs", include_str!("../src/motif_star.rs")),
    ("src/motif_path.rs", include_str!("../src/motif_path.rs")),
    ("src/motif_bars.rs", include_str!("../src/motif_bars.rs")),
    (
        "src/motif_typed_edge.rs",
        include_str!("../src/motif_typed_edge.rs"),
    ),
    (
        "src/named_entity_chips.rs",
        include_str!("../src/named_entity_chips.rs"),
    ),
    (
        "src/finding_card.rs",
        include_str!("../src/finding_card.rs"),
    ),
    (
        "src/finding_board.rs",
        include_str!("../src/finding_board.rs"),
    ),
    (
        "src/finding_detail_page.rs",
        include_str!("../src/finding_detail_page.rs"),
    ),
    ("src/alerts/mod.rs", include_str!("../src/alerts/mod.rs")),
    ("src/alerts/row.rs", include_str!("../src/alerts/row.rs")),
    (
        "src/alerts/filters.rs",
        include_str!("../src/alerts/filters.rs"),
    ),
    (
        "src/alerts/withdraw.rs",
        include_str!("../src/alerts/withdraw.rs"),
    ),
    (
        "src/alert_detail_page.rs",
        include_str!("../src/alert_detail_page.rs"),
    ),
];

/// Sources deliberately outside the sweep, each with the reason it is not UI copy.
///
/// An exclusion list rather than silence: the phase 45 UI spec's rule is *"an unregistered module is an
/// unswept module"*, and the way that rule failed was not that someone argued a module was exempt.
/// It was that nobody noticed one had appeared.
const NOT_SWEPT: &[(&str, &str)] = &[
    ("src/abbr.rs", "the mechanism itself"),
    (
        "src/glossary.rs",
        "the mechanism itself — it holds the token constants",
    ),
    (
        "src/cosmograph_spike.rs",
        "unmounted since 43-09; no route reaches it",
    ),
    (
        "src/cosmograph_trait.rs",
        "the shim's binding layer: JavaScript names and error plumbing, no rendered copy",
    ),
    ("src/main.rs", "module declarations and the mount call"),
    (
        "src/node_palette.rs",
        "node CLASS names, which are the graph's vocabulary rather than authored copy",
    ),
    (
        "src/profile/test_fixtures.rs",
        "test-only, declared behind #[cfg(test)]",
    ),
];

/// **Every shipped source is either swept or listed as an exception.**
///
/// The hole this closes is the one the phase 45 UI spec warned about and the phase fell into anyway:
/// plan 45-14 shipped `src/canvas_filters.rs` — the CNV-03 filter surface, 1,582 lines, five
/// pickers, an empty-canvas sentence and a save refusal, all of it rendered copy — and it was in
/// neither `SWEPT_SOURCES` nor any exclusion. The sweep passed. It always would have.
///
/// PROVE THIS CAN FAIL: remove any entry from `SWEPT_SOURCES` — this names the file.
#[test]
fn every_shipped_source_is_registered_for_the_sweep() {
    fn walk(dir: &std::path::Path, out: &mut Vec<String>, root: &std::path::Path) {
        for entry in std::fs::read_dir(dir).expect("src must be readable") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, out, root);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(format!(
                    "src/{}",
                    path.strip_prefix(root)
                        .expect("under src")
                        .to_string_lossy()
                        .replace('\\', "/")
                ));
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut on_disk = Vec::new();
    walk(&root, &mut on_disk, &root);
    on_disk.sort();

    assert!(
        on_disk.len() > 20,
        "non-vacuity: the walk must actually find the crate's sources, found {}",
        on_disk.len()
    );

    let registered: std::collections::BTreeSet<&str> =
        SWEPT_SOURCES.iter().map(|(name, _)| *name).collect();
    let excluded: std::collections::BTreeSet<&str> =
        NOT_SWEPT.iter().map(|(name, _)| *name).collect();

    let missing: Vec<&String> = on_disk
        .iter()
        .filter(|name| !registered.contains(name.as_str()) && !excluded.contains(name.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "these sources are neither swept nor listed as exceptions — an unregistered module is an \
         unswept module (the phase 45 UI spec): {missing:?}"
    );

    // And the lists do not name files that are gone, which is how an exclusion outlives its reason.
    for (name, _) in SWEPT_SOURCES.iter().chain(NOT_SWEPT.iter()) {
        assert!(
            on_disk.iter().any(|on| on == name),
            "{name} is registered but no longer exists"
        );
    }
}

/// `crates/schema/src/types/attribution.rs`, read as text rather than as a dependency:
/// `workbench-web` must never gain an edge to `crates/schema` (`scripts/check-wasm-boundary.sh`),
/// but a test may still read the file that decides what the provenance chip will be asked to
/// render.
const ATTRIBUTION_RS: &str = include_str!("../../schema/src/types/attribution.rs");

const TOOL_RAIL_RS: &str = include_str!("../src/tool_rail.rs");

fn normalize_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The source up to its trailing `#[cfg(test)] mod tests` — test fixtures are not UI copy.
///
/// **It looks for the test MODULE, not merely for the first `#[cfg(test)]` attribute**, and the
/// difference is a hole this sweep carried until plan 45-10. Every source registered before then
/// happened to put its only `#[cfg(test)]` immediately above `mod tests` at the foot of the file,
/// so cutting at the first attribute and cutting at the test module were the same cut — and the
/// first source to declare a test-only helper module higher up (`profile/mod.rs`'s
/// `#[cfg(test)] pub mod test_fixtures;`) would have made **every line after it invisible to the
/// sweep**, silently, with the sweep still reporting a pass.
///
/// That is the same shape of latent defect as the escaped-quote hole plan 45-09 found one function
/// down: not a mis-parse to shrug at, but a span of real UI copy quietly excluded from scanning.
fn shipped_portion(source: &str) -> &str {
    const ATTRIBUTE: &str = "#[cfg(test)]";
    let mut from = 0usize;
    while let Some(offset) = source[from..].find(ATTRIBUTE) {
        let start = from + offset;
        let after = &source[start + ATTRIBUTE.len()..];
        if after.trim_start().starts_with("mod tests") {
            return &source[..start];
        }
        from = start + ATTRIBUTE.len();
    }
    source
}

/// Glossary tokens: the bold first cell of every three-column glossary row, restricted to the
/// two-to-six-letter all-caps shape D-04's guard scans for. Mixed-case rows (`GraphML`) and the
/// trailing non-glossary tables (D-25) are outside D-03's `<abbr>` rule by construction.
fn glossary_tokens() -> Vec<String> {
    let mut tokens: Vec<String> = GLOSSARY_MD
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("| **")?;
            let end = rest.find("**")?;
            let token = &rest[..end];
            let shape_ok =
                (2..=6).contains(&token.len()) && token.chars().all(|c| c.is_ascii_uppercase());
            shape_ok.then(|| token.to_string())
        })
        .collect();
    tokens.sort();
    tokens.dedup();
    tokens
}

/// Every `"..."` string literal on a source line that can be UI copy.
///
/// **Escape-aware, and it has to be.** This scanner used to say "no escape handling needed — UI
/// copy here never embeds a double quote", and stopped a literal at the first `"` it met. Phase 45
/// broke that assumption the moment it registered `src/disambiguation.rs`, whose heading quotes the
/// analyst's query back (`{n} entities match \"{query}\".`), and the phase 45 UI spec's copy deck has more
/// of the same shape queued (`Delete \"{name}\"? This cannot be undone.`).
///
/// The consequence was not a mis-parse to shrug at: with `"he said \"CLI\" and left"`, the old
/// scanner returned `["he said \", " and left"]` — the text BETWEEN the escaped quotes, where the
/// bare token sits, was treated as being outside a literal and never scanned at all. A glossary
/// abbreviation in that position was invisible to the sweep. `the_literal_scanner_sees_copy_between
/// _escaped_quotes` pins the fix.
///
/// Lines that are comments, that build a
/// `String` value (`.to_string()` — DTO fixtures and const-fed props, never text nodes), that
/// define an `*_ABBR` token constant (the `<Abbr>` input itself), or that are `aria-hidden`
/// decoration (the app mark) are not copy and are skipped.
fn string_literals(source: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//")
            || line.contains(".to_string()")
            || line.contains("_ABBR: &str")
            || line.contains("aria-hidden=\"true\"")
        {
            continue;
        }
        let mut rest = line;
        while let Some(start) = rest.find('"') {
            let after = &rest[start + 1..];
            // Escape-aware: `\"` inside a literal does NOT end it.
            let mut idx = None;
            let bytes = after.as_bytes();
            let mut i = 0usize;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'"' => {
                        idx = Some(i);
                        break;
                    }
                    _ => i += 1,
                }
            }
            let Some(len) = idx else { break };
            out.push(&after[..len]);
            rest = &after[len + 1..];
        }
    }
    out
}

/// An **entity-id namespace prefix** (`"CF:"`, `"GB-COH:"`, `"location:"`) rather than copy.
///
/// The third thing this sweep has had to learn, and reported rather than dodged. Plan 45-11's
/// `NON_OPENING_NAMESPACES` matches a pivot target's id against the namespaces the graph mints for
/// node classes with no profile route; `"CF:"` is a value compared against an id with
/// `starts_with`, never a string any analyst reads. Writing it as `concat!("CF", ":")` would have
/// satisfied the sweep and is exactly the dodge 45-10 refused, so the sweep is taught the
/// distinction instead.
///
/// **The shape is deliberately narrow**: one trailing colon, no whitespace, and nothing but
/// alphanumerics, `-` and `_` before it. A sentence that mentions a namespace ("OSINT: 12 links")
/// carries a space and is still swept; the residual hole is a literal that is a bare `"TOKEN:"`
/// rendered as a label, which
/// `a_sentence_mentioning_a_namespace_is_still_swept` bounds by pinning the sentence form.
fn looks_like_id_namespace(literal: &str) -> bool {
    let Some(stem) = literal.strip_suffix(':') else {
        return false;
    };
    !stem.is_empty()
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A literal counts as UI copy when it is a text node, an attribute value, or the fixed text of
/// a `format!` template (its placeholders are stripped by [`without_placeholders`]; the fixed
/// words around them reach the DOM verbatim) — not a URL, a CSS declaration block, or an entity-id
/// namespace prefix ([`looks_like_id_namespace`]).
fn looks_like_ui_copy(literal: &str) -> bool {
    let trimmed = literal.trim();
    !(trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.starts_with("http")
        || looks_like_id_namespace(trimmed)
        || trimmed.contains(": ") && trimmed.ends_with(';'))
}

/// The namespace exemption does not swallow a sentence that happens to name a namespace.
///
/// PROVE THIS CAN FAIL: relax `looks_like_id_namespace` to `literal.contains(':')` and the first
/// assertion below stops seeing the bare token.
#[test]
fn a_sentence_mentioning_a_namespace_is_still_swept() {
    assert!(
        looks_like_ui_copy("OSINT: 12 links"),
        "a sentence is copy however it is punctuated"
    );
    assert!(
        looks_like_ui_copy("CF: the contract"),
        "a sentence is copy however it is punctuated"
    );
    // Non-vacuity: the exemption really does exempt the shape it was written for.
    assert!(!looks_like_ui_copy("CF:"));
    assert!(!looks_like_ui_copy("GB-COH:"));
}

/// Drops every `{…}` placeholder so only a template's fixed text is scanned.
fn without_placeholders(literal: &str) -> String {
    let mut out = String::with_capacity(literal.len());
    let mut depth = 0usize;
    for c in literal.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

fn contains_bare_token(literal: &str, token: &str) -> bool {
    literal
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| word == token)
}

/// The scanner sees copy sitting between two escaped quotes.
///
/// PROVE THIS CAN FAIL: restore the old body of `string_literals`' inner loop
/// (`let Some(len) = after.find('"') else { break };`) — the fixture below yields
/// `["he said \\", " and left"]`, `CLI` never appears in any fragment, and this test fails. That
/// was the live behaviour until this plan.
#[test]
fn the_literal_scanner_sees_copy_between_escaped_quotes() {
    let line = r#"        let heading = "he said \"CLI\" and left";"#;
    let literals = string_literals(line);
    assert!(
        literals.iter().any(|l| contains_bare_token(l, "CLI")),
        "a glossary token between escaped quotes must be visible to the sweep, got {literals:?}"
    );
}

#[test]
fn help_page_renders_the_cli_abbreviation_through_the_abbr_component() {
    let help = normalize_whitespace(HELP_RS);
    assert!(
        help.contains("use crate::abbr::Abbr;"),
        "help/mod.rs must import the Abbr component"
    );
    assert_eq!(
        help.matches("<Abbr token=CLI_ABBR.to_string() />").count(),
        1,
        "the CLI pivots heading must wrap its token in exactly one <Abbr>"
    );
    assert!(
        help.contains(
            r#"{CLI_PIVOTS_HEADING_PREFIX} " (" <Abbr token=CLI_ABBR.to_string() /> ") pivots""#
        ),
        "D-03: first occurrence in a heading is the full words, then the abbreviation in parentheses"
    );
    assert!(
        !help.contains(r#">"CLI pivots"<"#),
        "the bare \"CLI pivots\" heading literal must be gone"
    );
}

#[test]
fn home_help_card_renders_the_cli_abbreviation_through_the_abbr_component() {
    let home = normalize_whitespace(HOME_RS);
    assert!(
        home.contains("use crate::abbr::Abbr;"),
        "home_page.rs must import the Abbr component"
    );
    assert!(
        home.contains(r#""Graph algorithms and " <Abbr token=CLI_ABBR.to_string() /> " pivots""#),
        "the Help card's support line must wrap CLI in <Abbr>"
    );
    assert!(
        !home.contains(r#""Graph algorithms and CLI pivots""#),
        "the bare Help-card literal must be gone"
    );
}

#[test]
fn graph_legend_renders_osint_and_psc_through_abbr() {
    let tool_rail = normalize_whitespace(TOOL_RAIL_RS);
    assert!(
        tool_rail.contains("use crate::abbr::Abbr;"),
        "tool_rail.rs must import the Abbr component"
    );
    assert_eq!(
        tool_rail
            .matches("<Abbr token=OSINT_ABBR.to_string() />")
            .count(),
        1,
        "the canvas legend must wrap OSINT in exactly one <Abbr>"
    );
    assert_eq!(
        tool_rail
            .matches("<Abbr token=PSC_ABBR.to_string() />")
            .count(),
        1,
        "the canvas legend must wrap PSC in exactly one <Abbr>"
    );
    assert!(
        !tool_rail.contains(r#">"OSINT"<"#),
        "the bare OSINT legend literal must be gone"
    );
    assert!(
        !tool_rail.contains(r#">"PSC"<"#),
        "the bare PSC legend literal must be gone"
    );
}

#[test]
fn the_pages_carrying_the_abbr_are_mounted_by_the_route_table() {
    let app = normalize_whitespace(APP_RS);
    for (path, component) in [("", "HomePage"), ("help", "HelpPage")] {
        let expected = format!(r#"<Route path=path!("{path}") view={component} />"#);
        assert!(
            app.contains(&expected),
            "{component} must be mounted at /{path}"
        );
    }
}

#[test]
fn no_shipped_page_literal_carries_a_bare_glossary_abbreviation() {
    let tokens = glossary_tokens();
    assert!(
        tokens.iter().any(|t| t == "CLI"),
        "the glossary parse must find CLI, or this sweep is vacuous"
    );
    let mut offenders = Vec::new();
    for (name, source) in SWEPT_SOURCES {
        for literal in string_literals(shipped_portion(source)) {
            if !looks_like_ui_copy(literal) {
                continue;
            }
            let fixed_text = without_placeholders(literal);
            for token in &tokens {
                if contains_bare_token(&fixed_text, token) {
                    offenders.push(format!("{name}: \"{literal}\" carries bare {token}"));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "glossary abbreviations must render through <Abbr>, not as bare text:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_heading_words_are_the_glossary_meaning_for_cli() {
    // `| **CLI** | Command-Line Interface | ... |` is the genuine glossary row (the trailing
    // "Requirement category codes" table's `CLI` row is excluded by D-25 and has a different
    // meaning); the heading must not drift from it.
    let row = GLOSSARY_MD
        .lines()
        .find(|line| line.starts_with("| **CLI** |") && line.contains("Command-Line Interface"))
        .expect("docs/abbreviations.md must carry the CLI glossary row");
    assert!(row.contains("| Command-Line Interface |"));
}

/// The disambiguation list actually RENDERS its three pieces of copy and its three row fields.
///
/// Reachability, not mere presence, for the same reason `evidence_rows_render_show_on_graph`
/// exists: every function this asserts on is unit-tested in its own module, and Phase 43 shipped a
/// fully unit-tested component with zero call sites.
#[test]
fn the_disambiguation_list_renders_its_heading_help_line_and_row_fields() {
    let src = shipped_portion(include_str!("../src/disambiguation.rs"));
    assert!(
        src.contains("{heading}"),
        "the heading must be rendered, not merely computed"
    );
    assert!(
        src.contains("{DISAMBIGUATION_HELP}"),
        "the help line must be rendered"
    );
    assert!(
        src.contains("{secondary}"),
        "the kind/status/ward line must be rendered"
    );
    assert!(
        src.contains("{NO_ENTITY_MATCHES}"),
        "the no-match copy must be rendered, never a blank list"
    );
    assert!(
        src.contains("row_secondary_text(&row)") && src.contains("heading_text(&view)"),
        "the rendered values must come from the tested builders, not from re-inlined formatting"
    );
}

/// **PIV-01's load-bearing clause, at the source level.** `a_multi_match_search_opens_no_profile`
/// proves the decision never opens a profile; this proves there is no second, undecided route to
/// one hiding in the module — no profile endpoint is named anywhere in the shipped source, so the
/// only way out is the `on_open` callback that decision controls.
///
/// PROVE THIS CAN FAIL: add `let _ = "/api/entity/profile";` to the shipped portion of
/// `disambiguation.rs` and this test fails naming it.
#[test]
fn the_disambiguation_module_never_fetches_a_profile_itself() {
    let src = shipped_portion(include_str!("../src/disambiguation.rs"));
    for forbidden in ["/api/entity/profile", "/api/entity/neighbourhood"] {
        assert!(
            !src.contains(forbidden),
            "{forbidden} must not be reachable from the disambiguation module — a profile is \
             opened through `on_open`, which `apply_outcome` alone invokes"
        );
    }
    assert!(
        src.contains("/api/entity/disambiguate?q="),
        "the one endpoint this module does read must be the disambiguate one, or this test is \
         vacuous"
    );
    assert_eq!(
        src.matches("gloo_net::http::Request").count(),
        1,
        "exactly one fetch may exist in this module"
    );
}

/// The mount site 45-09 pinned as absent now exists, so the pin is replaced by its positive form.
///
/// 45-09 shipped `DisambiguationList`/`EntitySearch` ahead of their caller and asserted
/// `the_list_has_no_mount_site_yet_and_45_10_owns_one` — "this test fails the day 45-10 mounts the
/// component, and should then be deleted". It did, and it is: plan 45-10 mounts `EntitySearch` on
/// `/entity` (which is also the middle segment of the phase 45 UI spec's own `Home / Entity / {name}`
/// breadcrumb) and routes `/entity/:id` to the profile.
///
/// PROVE THIS CAN FAIL: delete the `<EntitySearch` element from `profile/mod.rs` and this fails
/// naming it; delete either `<Route>` line from `app.rs` and the route assertions fail.
#[test]
fn the_disambiguation_list_is_mounted_by_the_entity_search_page() {
    let profile = shipped_portion(include_str!("../src/profile/mod.rs"));
    assert!(
        profile.contains("<EntitySearch"),
        "PIV-01's search surface must be mounted, or the profile page is unreachable"
    );
    assert!(
        profile.contains("use crate::disambiguation::{status_label, EntitySearch}"),
        "the mount must import the shipped component, not re-declare one"
    );
    let app = normalize_whitespace(APP_RS);
    for (path, component) in [
        ("entity", "EntitySearchPage"),
        ("entity/:id", "EntityProfilePage"),
    ] {
        let expected = format!(r#"<Route path=path!("{path}") view={component} />"#);
        assert!(
            app.contains(&expected),
            "{component} must be mounted at /{path}"
        );
    }
}

/// The profile header renders the CLI's status vocabulary rather than respelling it.
///
/// D-01: the CLI and the workbench must not disagree about one company. 45-09 left `status_label`
/// — `crates/cli`'s `output::format_entity_status` strings character for character — in
/// `disambiguation.rs` specifically for this page to call, and a third spelling of "Dissolved
/// {date}" is the drift that would follow from ignoring it.
///
/// PROVE THIS CAN FAIL: inline `match profile.status.value { ... }` in `header_kind_and_status`
/// instead of calling `status_label`, and this fails.
#[test]
fn the_profile_header_renders_the_shared_status_vocabulary() {
    let profile = shipped_portion(include_str!("../src/profile/mod.rs"));
    assert!(
        profile.contains("status_label(&profile.status.value)"),
        "the header status must come from the shared vocabulary"
    );
    assert!(
        profile.contains("{kind_and_status}"),
        "the status line must be rendered, not merely computed"
    );
}

/// Reachability, not mere presence: `graph_href_for_refs` is unit-tested in its own module, but
/// that proves nothing about whether any evidence row actually renders the link. Phase 43's `Abbr`
/// component shipped fully unit-tested with zero call sites, which is the failure this style of
/// test exists to catch.
#[test]
fn evidence_rows_render_show_on_graph() {
    let src = shipped_portion(include_str!("../src/analysis_page.rs"));
    assert!(
        src.contains("graph_href_for_refs"),
        "evidence rows must build the Show on graph link"
    );
    assert!(
        src.contains("\"Show on graph\""),
        "the link text itself must be rendered"
    );
    assert!(
        src.contains("without graph pivots"),
        "the probe header must state how many rows offer no pivot"
    );
    assert!(
        src.contains("if without == 1"),
        "the pivot-gap count must say \"1 row\", not \"1 rows\""
    );
}

/// The sweep reads past a test-only module declared above the test module itself.
///
/// PROVE THIS CAN FAIL: restore the old body (`match source.find("#[cfg(test)]")`) and this fails —
/// the fixture's copy sits after a `#[cfg(test)] pub mod test_fixtures;` declaration, which the old
/// cut treated as the end of the shipped source.
#[test]
fn the_sweep_reads_past_a_test_only_module_declared_before_the_test_module() {
    let source = concat!(
        "//! doc\n",
        "#[cfg(test)]\n",
        "pub mod test_fixtures;\n",
        "\n",
        "pub const COPY: &str = \"a CLI pivot\";\n",
        "\n",
        "#[cfg(test)]\n",
        "mod tests {\n",
        "    const FIXTURE: &str = \"a CLI fixture\";\n",
        "}\n"
    );
    let shipped = shipped_portion(source);
    assert!(
        shipped.contains("a CLI pivot"),
        "shipped copy after a test-only module must still be swept, got: {shipped:?}"
    );
    assert!(
        !shipped.contains("a CLI fixture"),
        "the test module must still be excluded, got: {shipped:?}"
    );
    // And the real file that provoked this: its components sit after that declaration.
    assert!(
        shipped_portion(include_str!("../src/profile/mod.rs")).contains("ProfileRenderer"),
        "profile/mod.rs's renderer must be inside the swept portion"
    );
}

/// **Every source-dataset value the read path can put on a provenance chip is accounted for.**
///
/// The phase 45 UI spec makes this a contract: "the provenance chip maps its source-dataset value through
/// a single lookup that returns either an `<Abbr>`-wrapped token or plain text", and "a test must
/// pin that every source-dataset value the DTO can carry resolves through that lookup".
///
/// The lookup itself is data-driven and unit-tested in `provenance_chip.rs`. What a *source-reading*
/// test can add is the half the unit test cannot see: that no abbreviation-shaped dataset constant
/// exists without a glossary row behind it. Such a constant would reach the chip, satisfy the
/// shape test, miss the glossary, and render as bare text — a D-03 violation living in data rather
/// than in a string literal, where the sweep below could never find it.
///
/// PROVE THIS CAN FAIL: add `pub const HMLR_PPD: &str = "HMLR";` to attribution.rs with no glossary
/// row and this fails naming it. (`HMLR` does have a row, so use an invented token to try it.)
#[test]
fn every_source_dataset_the_read_path_can_emit_resolves_through_the_chip_lookup() {
    let tokens = glossary_tokens();
    let mut datasets: Vec<&str> = Vec::new();
    for line in ATTRIBUTION_RS.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("pub const ") else {
            continue;
        };
        let Some((_, value)) = rest.split_once("&str = \"") else {
            continue;
        };
        let Some(value) = value.split('"').next() else {
            continue;
        };
        datasets.push(value);
    }
    assert!(
        datasets.contains(&"CH") && datasets.contains(&"OpenSanctions"),
        "the attribution parse must find both an abbreviation and a name, or this is vacuous: \
         {datasets:?}"
    );

    let mut unwrappable = Vec::new();
    let mut wrapped = 0;
    for dataset in &datasets {
        let abbreviation_shaped =
            (2..=6).contains(&dataset.len()) && dataset.chars().all(|c| c.is_ascii_uppercase());
        if !abbreviation_shaped {
            continue;
        }
        if tokens.iter().any(|t| t == dataset) {
            wrapped += 1;
        } else {
            unwrappable.push(*dataset);
        }
    }
    assert!(
        wrapped >= 8,
        "most dataset constants are abbreviations; only {wrapped} resolved, so this sweep is \
         looking at the wrong file"
    );
    assert!(
        unwrappable.is_empty(),
        "these source datasets would render as bare abbreviations with no glossary expansion: \
         {unwrappable:?}"
    );
}

/// The profile's sections actually RENDER the things this plan's unit tests compute.
///
/// Reachability, not mere presence — the idiom `evidence_rows_render_show_on_graph` and
/// `the_disambiguation_list_renders_its_heading_help_line_and_row_fields` established, and the one
/// Phase 43's zero-call-site `Abbr` exists to justify. Every function named below is unit-tested in
/// its own module; none of that proves a row ever reaches the screen.
#[test]
fn the_profile_renders_its_rows_its_empty_states_and_its_provenance() {
    let sections = shipped_portion(include_str!("../src/profile/sections.rs"));
    for (fragment, why) in [
        (
            "{empty_sentence",
            "the empty-state sentence must be rendered",
        ),
        ("{notice", "the as-at notice must be rendered"),
        (
            "<SentenceView",
            "both sentences must render through the component that wraps their glossary token",
        ),
        ("{caption", "the paging caption must be rendered"),
        ("<ProvenanceChip", "the provenance chip must be mounted"),
        (
            "<SectionProvenanceMarker",
            "the section-level provenance marker must be mounted",
        ),
        (
            "<VerdictBadge",
            "findings must render the shared verdict badge",
        ),
        (
            "<SectionLabel",
            "section names must render through the Abbr-aware label",
        ),
        (
            "{mark.map(",
            "a row the filter marked must carry its chip — out of force, or untested",
        ),
        (
            "{untested",
            "a section the filter could not test must say so, and say how much of it",
        ),
        ("{SHOW_MORE}", "a paged section must offer its control"),
    ] {
        assert!(sections.contains(fragment), "{why} ({fragment})");
    }
    assert!(
        sections.contains("empty_state_sentence(section, reason)")
            && sections.contains("section_state(&profile, section, density)")
            && sections.contains("row_as_at_mark(&row)")
            && sections.contains("as_at_untested_notice(&rows, section, as_at.as_deref())"),
        "the rendered values must come from the tested builders, not from re-inlined formatting"
    );
    // The chip text is no longer a literal at the render site — `row_as_at_mark` decides which of
    // the two marks a row carries, so the sweep follows it to the function and checks both are
    // reachable from there. A chip that can never be returned is the same zero-call-site defect
    // one level in.
    for (chip, why) in [
        (
            "AsAtStatus::OutOfForce => Some(OUTSIDE_AS_AT)",
            "a row the filter placed outside the date",
        ),
        (
            "AsAtStatus::Undated => Some(NOT_TESTED)",
            "a row the filter could not test at all — the live majority (45-06)",
        ),
    ] {
        assert!(
            sections.contains(chip),
            "{why} must be reachable from row_as_at_mark ({chip})"
        );
    }
    let profile = shipped_portion(include_str!("../src/profile/mod.rs"));
    assert!(
        profile.contains("<ProfileSections"),
        "the sections must be mounted by the one profile renderer"
    );
    // The banner's "see below" must have a below. `NULL_RANGE_RULE` reached this crate in no
    // source at all before plan 45-12, so the promise dangled for two plans with every test green.
    assert!(
        profile.contains("as_at_rule_statement(profile.as_at.as_deref())")
            && profile.contains("{rule.map("),
        "the null-range rule the banner defers to must be rendered, not merely importable"
    );
}
