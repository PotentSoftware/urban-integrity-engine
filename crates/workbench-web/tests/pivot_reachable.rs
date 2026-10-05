//! **Proof that every surface PIV-03 names actually renders its entity references as links, and
//! that the set of classes it links is the set the read path can open.**
//!
//! Not ceremony. Phase 43's verification found the `Abbr` component fully built, unit-tested,
//! CI-guarded and called from **nowhere** — every page rendered bare abbreviations across five
//! plans, and nothing failed, because every test asked whether the component worked rather than
//! whether anything called it. Plan 44-14 found a live defect the moment that sweep was extended.
//! Registering one module in `abbr_reachable.rs` on this branch exposed two latent holes in the
//! sweep itself. This file is the same instrument aimed at pivots.
//!
//! Three things it asserts, and one it deliberately does not:
//!
//! 1. **Call sites, not functions.** Every source under `src/` that renders an entity reference is
//!    registered here, the registry is checked for completeness against a walk of the tree rather
//!    than trusted, and each registered surface is checked to contain the render. A source
//!    rendering `<EntityRefView>` that nobody remembered to register fails
//!    [`the_registry_names_every_source_that_renders_an_entity_reference`].
//! 2. **One href builder.** `pivot_href` is the only way a reference becomes a link, on every
//!    surface. A file that builds its own — the exact mutation that a unit test asking
//!    `pivot_href` a question cannot see — fails
//!    [`the_shared_pivot_policy_is_the_only_href_builder_a_surface_calls`].
//! 3. **The shipped width, as shipped.** PIV-03's `must_have` says every row is a click-through.
//!    **As shipped that is false, deliberately and on record**: two of the five `RecordKind`
//!    classes render as anchors, and the rest render as text because
//!    `crates/core::persistence::queries::query_entity_by_id` has no branch for them. A sweep
//!    asserting the aspiration would have to be either vacuous or wrong, so this one asserts the
//!    fact — and asserts it *against the read path's own source*, so it fails the day that function
//!    learns a third node class and the rendering does not follow.
//!
//! What it does not assert: that any of this looks right in a browser. Every test here is a
//! host-side source read, the only kind this crate's test model supports (no DOM, no headless
//! browser — phase 42 research Pitfall 10). Scroll restoration in particular is unobservable here;
//! see the plan summary.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Every shipped source that renders an entity reference, with its contents baked in at compile
/// time. Completeness is not assumed — see
/// [`the_registry_names_every_source_that_renders_an_entity_reference`].
const PIVOT_SURFACES: &[(&str, &str, &str)] = &[
    (
        "src/profile/sections.rs",
        include_str!("../src/profile/sections.rs"),
        "the entity profile's eighteen sections — neighbours, contracts, addresses and links",
    ),
    (
        "src/entity_ref_view.rs",
        include_str!("../src/entity_ref_view.rs"),
        "the shared reference renderer every non-profile surface mounts",
    ),
    (
        "src/analysis_page.rs",
        include_str!("../src/analysis_page.rs"),
        "the report's probe evidence rows",
    ),
    (
        "src/graph_results_list.rs",
        include_str!("../src/graph_results_list.rs"),
        "the graph results list — path hops, community members, ranked entities",
    ),
    (
        "src/side_card.rs",
        include_str!("../src/side_card.rs"),
        "the canvas node side card",
    ),
    (
        "src/entity_picker.rs",
        include_str!("../src/entity_picker.rs"),
        "the entity picker's resolved chip — the one suppressed pivot",
    ),
    (
        "src/named_entity_chips.rs",
        include_str!("../src/named_entity_chips.rs"),
        "the finding board's compact chip fallback for the 11 Chips-kind probes (plan 50-06)",
    ),
    (
        "src/finding_detail_page.rs",
        include_str!("../src/finding_detail_page.rs"),
        "the finding detail route's Evidence section, full name (id) form (plan 50-07)",
    ),
    (
        "src/motif_path.rs",
        include_str!("../src/motif_path.rs"),
        "the general motif-path renderer's single-participant chip (46-07, MotifKind::SingleSubject)",
    ),
    (
        "src/alerts/row.rs",
        include_str!("../src/alerts/row.rs"),
        "the alert row card's participant chips — every entity in the motif is a pivot (46-08, TRI-02)",
    ),
    (
        "src/alert_detail_page.rs",
        include_str!("../src/alert_detail_page.rs"),
        "the alert detail route's Participants list — every participant, full name (id) form (46-10, TRI-02)",
    ),
];

/// The pivot policy's own source, read as text so the sweep reads the shipped constants rather than
/// a copy of them.
const PIVOT_RS: &str = include_str!("../src/profile/pivot.rs");

/// The profile screen, for the router-history assertions.
const PROFILE_MOD_RS: &str = include_str!("../src/profile/mod.rs");

/// The as-at control, for the state-ownership assertion — plan 45-12's half of the same invariant
/// `the_profile_screen_navigates_through_the_router_and_stores_no_route_state` pins one file over.
const AS_AT_RS: &str = include_str!("../src/profile/as_at.rs");

/// `crates/core`'s read path and `crates/schema`'s target taxonomy, both read **as text**: this
/// crate is wasm-compiled and `scripts/check-wasm-boundary.sh` forbids it an edge to either. The
/// idiom is 45-10's (`attribution.rs`, read the same way for the same reason) — a test may read the
/// file that decides what the renderer will be asked to render.
const QUERIES_RS: &str = include_str!("../../core/src/persistence/queries.rs");
const VERDICT_RS: &str = include_str!("../../schema/src/types/verdict.rs");

/// The one call site allowed to render a reference as plain text where it would otherwise open.
/// See `entity_picker.rs`'s comment at the site for the reason.
const SUPPRESSED_PIVOT: &str = "src/entity_picker.rs";

/// The source up to its trailing `#[cfg(test)] mod tests` — the same cut `abbr_reachable.rs` makes,
/// looking for the test **module** rather than the first `#[cfg(test)]` attribute, because a
/// test-only module declared higher up would otherwise make every line after it invisible to the
/// sweep (the hole 45-10 found and closed there).
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

/// Drops `//`-comment lines. A rule written about what the code *does* must not be satisfied — or
/// broken — by prose describing it, which is the trap plan 35-07 hit four times in one phase.
fn without_comments(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The crate's `src/` tree, walked at test time, as `src/…`-relative paths. This is what makes the
/// registry above checkable rather than merely declarative: a new surface that nobody registered is
/// still found here.
fn every_source_file() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("the crate source tree must be readable") {
            let path = entry.expect("a readable directory entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    walk(&root, &mut out);
    out.sort();
    out
}

fn relative(path: &Path) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    format!(
        "src/{}",
        path.strip_prefix(root.join("src"))
            .expect("every swept file lives under src/")
            .display()
    )
    .replace('\\', "/")
}

/// The string values of a `&[&str]` constant declared in `source`, by name.
fn const_str_list(source: &str, name: &str) -> Vec<String> {
    let decl = source
        .split_once(&format!("{name}: &[&str] = &["))
        .unwrap_or_else(|| panic!("{name} must be declared as a &[&str] slice"))
        .1;
    let body = decl.split_once("];").expect("the slice must be closed").0;
    string_literals(body)
}

/// Every `"…"` literal in a fragment of Rust source.
fn string_literals(fragment: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = fragment;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('"') else { break };
        out.push(after[..end].to_string());
        rest = &after[end + 1..];
    }
    out
}

// ---------------------------------------------------------------------------------------------
// 1. Call sites
// ---------------------------------------------------------------------------------------------

/// **A surface that renders an entity reference and is not registered here fails.**
///
/// The registry is a list a human wrote; this checks it against the tree. Phase 43's lesson is that
/// "its caller is coming" is exactly the assumption nothing verifies, and 44-14's is that extending
/// a sweep is when defects surface.
///
/// PROVE THIS CAN FAIL: delete the `src/side_card.rs` entry from `PIVOT_SURFACES` and this fails
/// naming it.
#[test]
fn the_registry_names_every_source_that_renders_an_entity_reference() {
    let registered: BTreeSet<&str> = PIVOT_SURFACES.iter().map(|(name, _, _)| *name).collect();
    let mut found = BTreeSet::new();
    for path in every_source_file() {
        let source = std::fs::read_to_string(&path).expect("a readable source file");
        let shipped = without_comments(shipped_portion(&source));
        if shipped.contains("<EntityRefView") || shipped.contains("pivot_href(") {
            found.insert(relative(&path));
        }
    }
    // `profile/mod.rs` imports the policy but renders no reference itself, and `profile/pivot.rs`
    // IS the policy; neither is a surface.
    found.remove("src/profile/pivot.rs");
    found.remove("src/profile/mod.rs");
    // The as-at control builds a href for the current entity, not for a reference someone might
    // click through to — see `MAY_OPEN_DIRECTLY` in
    // `the_shared_pivot_policy_is_the_only_href_builder_a_surface_calls`.
    found.remove("src/profile/as_at.rs");

    // Non-vacuity, twice: the walk must find files at all, and it must find MORE than the profile
    // — a sweep that swept only what 45-10 already shipped would prove nothing about this task.
    assert!(
        found.len() >= 5,
        "the walk found only {} reference-rendering sources, which cannot be right: {found:?}",
        found.len()
    );
    assert!(
        found.contains("src/analysis_page.rs") && found.contains("src/graph_results_list.rs"),
        "the two surfaces this plan exists to link must be among them: {found:?}"
    );

    let unregistered: Vec<&String> = found
        .iter()
        .filter(|f| !registered.contains(f.as_str()))
        .collect();
    assert!(
        unregistered.is_empty(),
        "these sources render an entity reference and are not registered in PIVOT_SURFACES — \
         register them, and decide deliberately whether each one links: {unregistered:?}"
    );
    let stale: Vec<&&str> = registered.iter().filter(|r| !found.contains(**r)).collect();
    assert!(
        stale.is_empty(),
        "these registered surfaces no longer render an entity reference at all — a pivot surface \
         that quietly disappeared is the defect this sweep exists to catch: {stale:?}"
    );
}

/// Each registered surface genuinely renders the reference, rather than merely importing the type.
///
/// PROVE THIS CAN FAIL: delete the `<EntityRefView entity=entity />` element from
/// `analysis_page.rs`'s evidence rows and this fails naming that file.
#[test]
fn every_registered_surface_renders_a_reference_rather_than_importing_one() {
    let mut inert = Vec::new();
    for (name, source, what) in PIVOT_SURFACES {
        let shipped = without_comments(shipped_portion(source));
        let renders = shipped.contains("<EntityRefView") || shipped.contains("pivot_href(");
        if !renders {
            inert.push(format!("{name} ({what})"));
        }
    }
    assert!(
        inert.is_empty(),
        "these surfaces are registered as rendering an entity reference and do not: {inert:?}"
    );
    assert_eq!(
        PIVOT_SURFACES.len(),
        11,
        "eleven surfaces are registered; a change to that number is a change to what PIV-03 \
         covers and should be a deliberate edit here"
    );
}

// ---------------------------------------------------------------------------------------------
// 2. One href builder
// ---------------------------------------------------------------------------------------------

/// **Every surface pivots through `pivot_href`, and none builds its own href.**
///
/// This is the assertion a unit test cannot make. `entity_ref_view.rs`'s own unit test asks
/// `pivot_href` a question and passes whatever the component does with it; swapping the component's
/// call for `profile_href(&entity.id, …)` — which would link every ward, contract and address in
/// the report straight to a 404 — leaves that test green. It does not leave this one green.
///
/// `profile_href` is reachable from exactly one place, `profile/mod.rs`, where the entity search
/// page opens a profile directly rather than pivoting to one.
///
/// PROVE THIS CAN FAIL: replace `pivot_href(Some(&entity), …)` in `entity_ref_view.rs` with
/// `crate::profile::pivot::profile_href(&entity.id, …)` and this fails naming the file.
#[test]
fn the_shared_pivot_policy_is_the_only_href_builder_a_surface_calls() {
    // `src/profile/as_at.rs` is here because the as-at control is **not** a pivot: it re-opens the
    // page the operator is already on under a different date window. There is no `EntityRef` and
    // no question of whether the target opens. It still goes through `profile_href` rather than
    // interpolating a query string, so the id is encoded by the one function that encodes ids.
    const MAY_OPEN_DIRECTLY: &[&str] = &[
        "src/profile/mod.rs",
        "src/profile/pivot.rs",
        "src/profile/as_at.rs",
    ];

    let mut own_builders = Vec::new();
    let mut pivoting = BTreeSet::new();
    for path in every_source_file() {
        let name = relative(&path);
        let source = std::fs::read_to_string(&path).expect("a readable source file");
        let shipped = without_comments(shipped_portion(&source));
        if shipped.contains("pivot_href(") {
            pivoting.insert(name.clone());
        }
        if shipped.contains("profile_href(") && !MAY_OPEN_DIRECTLY.contains(&name.as_str()) {
            own_builders.push(name);
        }
    }
    assert!(
        own_builders.is_empty(),
        "these sources build a profile href of their own instead of going through pivot_href — \
         two rules for which reference opens is one analyst clicking into a 404: {own_builders:?}"
    );
    // Non-vacuity: the policy has real callers, on both the profile and the shared renderer, or the
    // assertion above would be satisfied by a codebase that had stopped linking altogether.
    assert!(
        pivoting.contains("src/profile/sections.rs") && pivoting.contains("src/entity_ref_view.rs"),
        "the profile's rows and the shared renderer must both pivot through the policy: {pivoting:?}"
    );
    // And the policy applies BOTH halves of the test — the class and the id's namespace — rather
    // than the class alone, which is what the report's fifty mislabelled refs turn on.
    let policy = without_comments(shipped_portion(PIVOT_RS));
    assert!(
        policy.contains("opens_a_profile(&entity.kind) && namespace_class(&entity.id).is_none()"),
        "opens_a_profile_ref must test the class AND the id's namespace"
    );
    assert!(
        policy.contains("opens_a_profile_ref(target).then("),
        "pivot_href must consult the whole-reference test, not the kind alone"
    );
}

/// **Exactly one pivot is suppressed, and it is the one that is documented.**
///
/// `no_link` exists so that a reference which *could* open can deliberately be rendered as text.
/// That is precisely the mechanism by which PIV-03 could be quietly un-done one call site at a
/// time, so the set is pinned rather than the count.
///
/// PROVE THIS CAN FAIL: add `no_link=true` to any other `<EntityRefView>` and this fails naming
/// the file; remove it from the picker and the non-vacuity assertion fails.
#[test]
fn the_only_suppressed_pivot_is_the_pickers_own_chip() {
    let mut suppressing = BTreeSet::new();
    let mut occurrences = 0usize;
    for path in every_source_file() {
        let name = relative(&path);
        let source = std::fs::read_to_string(&path).expect("a readable source file");
        let shipped = without_comments(shipped_portion(&source));
        let count = shipped.matches("no_link=true").count();
        if count > 0 {
            suppressing.insert(name);
            occurrences += count;
        }
    }
    assert_eq!(
        suppressing,
        BTreeSet::from([SUPPRESSED_PIVOT.to_string()]),
        "the suppression list is a decision, not an accident — every entry needs a reason at the \
         site and a line here"
    );
    assert_eq!(
        occurrences, 1,
        "one chip, one suppression: a second one in the same file is as much a decision as one in \
         a new file"
    );
}

// ---------------------------------------------------------------------------------------------
// 3. The shipped width, measured against the read path
// ---------------------------------------------------------------------------------------------

/// **The classes the renderer links are exactly the classes the read path can open.**
///
/// `OPENABLE_KINDS` is a hand-written list in `workbench-web`; `query_entity_by_id` is the function
/// in `crates/core` that decides whether a profile exists. They have to agree, and nothing but this
/// makes them: the two live in different crates with a wasm boundary between them, so the compiler
/// cannot.
///
/// **This is the test that fails the day the read path widens without the rendering following.**
/// Add a `NodeWeight::Contract` branch to `query_entity_by_id` and it fails until `OPENABLE_KINDS`
/// grows to match — at which point every contract row on the profile becomes an anchor with no
/// other edit, because the policy is applied per row by the target's class.
///
/// PROVE THIS CAN FAIL: add `"Location"` to `OPENABLE_KINDS` (the renderer running ahead of the
/// read path) or delete the `OffshoreEntity` fallback from `query_entity_by_id` (the read path
/// narrowing under a renderer that kept linking) — either direction fails.
#[test]
fn the_linked_classes_are_exactly_the_classes_the_read_path_opens() {
    let rendered: BTreeSet<String> = const_str_list(PIVOT_RS, "OPENABLE_KINDS")
        .into_iter()
        .collect();

    // The body of `query_entity_by_id`, from its signature to the next top-level `pub async fn`.
    let body = {
        let from = QUERIES_RS
            .split_once("pub async fn query_entity_by_id(")
            .expect("crates/core must still declare query_entity_by_id")
            .1;
        from.split_once("\npub async fn ")
            .expect("another function must follow it")
            .0
    };
    // Each node class it resolves appears as the scan predicate `"\"type\":\"X\""`.
    let mut resolved = BTreeSet::new();
    let mut rest = body;
    while let Some(at) = rest.find(r#"\"type\":\""#) {
        let after = &rest[at + r#"\"type\":\""#.len()..];
        let end = after.find('\\').expect("the predicate literal must close");
        resolved.insert(after[..end].to_string());
        rest = &after[end..];
    }

    // Non-vacuity, before comparing: both sides must have found something, and something plural.
    assert!(
        resolved.len() >= 2,
        "the read-path scan found {resolved:?}, which means the extraction has stopped working \
         rather than that the read path narrowed"
    );
    assert!(
        rendered.contains("Entity"),
        "the renderer must link at least the Entity class, or this comparison is vacuous"
    );

    assert_eq!(
        rendered, resolved,
        "the classes the workbench renders as anchors must be the classes crates/core can open — \
         left is OPENABLE_KINDS, right is query_entity_by_id's own reach"
    );

    // The handler reaches the read path through the as-at sibling, which must delegate rather than
    // resolve anything of its own — otherwise the reach measured above is not the reach that runs.
    let as_at_body = QUERIES_RS
        .split_once("pub async fn query_entity_by_id_as_at(")
        .expect("the as-at sibling must exist")
        .1
        .split_once("\npub async fn ")
        .expect("another function must follow it")
        .0;
    assert!(
        as_at_body.contains("query_entity_by_id(db, entity_id)"),
        "GET /api/entity/profile's read path must delegate to the function swept above"
    );
    assert!(
        !as_at_body.contains(r#"\"type\":\""#),
        "the as-at sibling must not resolve a node class of its own"
    );
}

/// **Every pivot-target class is in one of two columns, and the column it is in is stated.**
///
/// The honest shipped width, as a table rather than as an aspiration. `RecordKind` is the taxonomy
/// a pivot target is named by (D-06); each of its variants either renders as an anchor or renders
/// as text for a reason. A **new** variant is in neither column and fails here, which is the point:
/// adding a pivot target without deciding whether it opens is how a dead link ships.
///
/// PROVE THIS CAN FAIL: add a variant to `RecordKind` in `crates/schema` without adding it below,
/// and this fails naming it.
#[test]
fn the_shipped_pivot_width_accounts_for_every_target_class() {
    /// The classes that render as anchors today.
    const ANCHORS: &[&str] = &["Entity"];
    /// The classes that render as plain text, each with the reason it does.
    const PLAIN_TEXT: &[(&str, &str)] = &[
        (
            "Contract",
            "NodeWeight::Contract has no subject lookup — query_entity_by_id has no branch for it",
        ),
        (
            "Location",
            "NodeWeight::Location has no subject lookup — a profile assembled for an address would \
             answer fourteen sections about officers it does not have",
        ),
        (
            "CompanyNumber",
            "a bare Companies House number is not a node id; the GB-COH: node it names is reached \
             as an Entity",
        ),
        (
            "IdentityLink",
            "an identity_links record id is not a node id at all",
        ),
    ];

    // `RecordKind`'s variants, read out of the schema's own `name()` match arms.
    let arms = VERDICT_RS
        .split_once("pub const fn name(self) -> &'static str {")
        .expect("RecordKind::name must exist")
        .1
        .split_once("\n    }")
        .expect("the match must close")
        .0;
    let variants: BTreeSet<String> = arms
        .lines()
        .filter_map(|line| line.trim().strip_prefix("RecordKind::"))
        .filter_map(|rest| rest.split_once(" => "))
        .map(|(variant, _)| variant.to_string())
        .collect();

    assert!(
        variants.len() >= 5,
        "the RecordKind parse found {variants:?} — the extraction has broken, not the taxonomy"
    );

    let accounted: BTreeSet<String> = ANCHORS
        .iter()
        .map(|v| v.to_string())
        .chain(PLAIN_TEXT.iter().map(|(v, _)| v.to_string()))
        .collect();
    let unaccounted: Vec<&String> = variants.difference(&accounted).collect();
    assert!(
        unaccounted.is_empty(),
        "these pivot-target classes are in neither column — decide whether each opens a profile, \
         and say why if it does not: {unaccounted:?}"
    );

    // `OffshoreEntity` is the fifth linked class and has no `RecordKind` variant: it is a node
    // class the profile route opens and no probe emits. Stated rather than left as a silent
    // asymmetry between the two vocabularies.
    let rendered = const_str_list(PIVOT_RS, "OPENABLE_KINDS");
    assert!(
        rendered.contains(&"OffshoreEntity".to_string()) && !variants.contains("OffshoreEntity"),
        "OffshoreEntity opens a profile and is not a RecordKind variant; if that changes, this \
         table needs a line, not a deletion"
    );
    for anchor in ANCHORS {
        assert!(
            rendered.contains(&anchor.to_string()),
            "{anchor} is listed here as an anchor and is not in OPENABLE_KINDS"
        );
    }
    for (text, _) in PLAIN_TEXT {
        assert!(
            !rendered.contains(&text.to_string()),
            "{text} is listed here as plain text and IS in OPENABLE_KINDS"
        );
    }
}

/// The namespace guard's deny list, pinned. It is the half of the policy that a `kind` field cannot
/// state, and the half that fifty live evidence refs turn on.
///
/// PROVE THIS CAN FAIL: delete the `ward:` entry and this fails naming it.
#[test]
fn the_namespace_guard_names_every_class_that_cannot_open() {
    let declared = PIVOT_RS
        .split_once("NON_OPENING_NAMESPACES: &[(&str, &str)] = &[")
        .expect("the guard's list must be declared")
        .1
        .split_once("];")
        .expect("the list must close")
        .0;
    let pairs: Vec<String> = string_literals(declared);
    let expected = [
        "CF:",
        "Contract",
        "planning:",
        "Contract",
        "location:",
        "Location",
        "LOC:",
        "Location",
        "ward:",
        "Ward",
    ];
    assert_eq!(
        pairs,
        expected.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        "the namespaces a pivot is refused for are a decision about live data; changing the list \
         is a deliberate edit here"
    );
    // Non-vacuity: every class named on the right is one the renderer does NOT link, or the guard
    // would be refusing links the policy already allows.
    let rendered = const_str_list(PIVOT_RS, "OPENABLE_KINDS");
    for class in ["Contract", "Location", "Ward"] {
        assert!(!rendered.contains(&class.to_string()));
    }
}

// ---------------------------------------------------------------------------------------------
// 4. The pivot chain
// ---------------------------------------------------------------------------------------------

/// **The profile screen navigates client-side and reads its whole state from the router.**
///
/// Both halves are what make back and forward walk the chain: a `location.assign` anywhere would
/// turn a hop into a document load, and a copy of the as-at date in a signal would be a second
/// source of truth that `popstate` cannot reach.
///
/// PROVE THIS CAN FAIL: restore `window.location().assign(href)` in place of `use_navigate` and
/// this fails; declare `let as_at = RwSignal::new(None::<String>);` and the second assertion fails.
#[test]
fn the_profile_screen_navigates_through_the_router_and_stores_no_route_state() {
    let shipped = without_comments(shipped_portion(PROFILE_MOD_RS));
    assert!(
        shipped.contains("use_navigate()"),
        "a pivot must be a client-side navigation, or every hop reloads the bundle"
    );
    assert!(
        !shipped.contains("location().assign"),
        "a document navigation anywhere on this screen breaks the pivot chain it is on"
    );
    assert!(
        shipped.contains("ProfileRoute::new(") && shipped.contains("use_query_map"),
        "the screen's state must be built from the router's own params and query"
    );
    let stored: Vec<&str> = shipped
        .lines()
        .filter(|line| line.contains("RwSignal") && line.contains("as_at"))
        .collect();
    assert!(
        stored.is_empty(),
        "the as-at date must live in the URL and nowhere else — back cannot restore a signal: \
         {stored:?}"
    );
    assert!(
        shipped.contains("response_is_still_wanted(&for_request, &current)"),
        "a response for a route the analyst has left must be dropped, or a fast Back settles the \
         screen on the wrong entity"
    );
}

/// **The as-at control keeps no copy of the date, and is mounted.**
///
/// Plan 45-12's control is the obvious place for the second source of truth 45-11 spent a test
/// preventing one file over: a signal holding the chosen date would leave the input showing one
/// window while the address bar, the banner and the rows showed another, and Back could not reach
/// it. The control must navigate and stop.
///
/// The mount half is the Phase 43 lesson: a control that is built, unit-tested and called from
/// nowhere passes every test that asks whether it works.
///
/// PROVE THIS CAN FAIL: give `AsAtControl` a `let chosen = RwSignal::new(as_at.clone());` and the
/// first assertion fails; delete `<AsAtControl` from `ProfileHeader` and the last one does.
#[test]
fn the_as_at_control_stores_no_date_of_its_own_and_is_mounted() {
    let control = without_comments(shipped_portion(AS_AT_RS));

    let stored: Vec<&str> = control
        .lines()
        .filter(|line| {
            line.contains("RwSignal") || line.contains("StoredValue") || line.contains("signal(")
        })
        .collect();
    assert!(
        stored.is_empty(),
        "the as-at control must hold no state — the date lives in the URL, where Back can reach \
         it: {stored:?}"
    );

    assert!(
        control.contains("use_navigate"),
        "setting a date must be a client-side navigation to a URL that states the new window"
    );
    assert!(
        control.contains("profile_href(id, Some(chosen))"),
        "the control must build its target through the one href builder, so the id is encoded once"
    );

    let screen = without_comments(shipped_portion(PROFILE_MOD_RS));
    assert!(
        screen.contains("<AsAtControl"),
        "PIV-04's control must be mounted by the profile header, not merely defined"
    );
}
