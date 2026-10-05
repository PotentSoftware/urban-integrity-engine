//! Plan 45-15 (PIV-05): **the pin control is mounted where the requirement says it is, the
//! investigation list is reachable from the shell, and there is one definition of what a pin is.**
//!
//! # Why these are source sweeps
//!
//! `workbench-web` is a **binary** crate with no library target, so an integration test here can
//! read its sources but cannot call its functions. The decisions and the strings are unit-tested
//! inside `src/investigation/`, where they can be called for real; this file proves those functions
//! are the ones the pages actually mount.
//!
//! The standing reason for the style is Phase 43's: a fully built, unit-tested, CI-guarded `Abbr`
//! component shipped with **zero call sites** across five plans and nothing failed, because every
//! test asked whether the component worked rather than whether anything used it. An "every X in the
//! UI" rule needs a test that finds call sites.

fn source_of(relative: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()))
}

/// Every shipped source in the crate, so a sweep can be exhaustive rather than hand-listed.
fn shipped_sources() -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("src must be readable") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut paths = Vec::new();
    walk(&root, &mut paths);
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let relative = format!(
                "src/{}",
                path.strip_prefix(&root)
                    .expect("under src")
                    .to_string_lossy()
                    .replace('\\', "/")
            );
            let body = std::fs::read_to_string(&path).expect("source must be readable");
            (relative, body)
        })
        .collect()
}

/// Whole comment lines removed.
///
/// **A rule must not be satisfiable — or breakable — by a comment describing it.** That is 35-07's
/// four-times-in-one-phase lesson, restated by 45-12 §7.1 when `check-abbreviations.sh` read a doc
/// comment as a string literal. Both of the first two failures of this file's own sweeps were
/// exactly that: `app.rs`'s module doc mentions `<Router><Routes>` and would have been read as the
/// route table, and `investigation/mod.rs`'s doc explains why `localStorage` is NOT used, which the
/// no-browser-storage sweep read as a use of it.
fn without_comments(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The portion of a source before its `#[cfg(test)]` items — test fixtures are not shipped code.
fn shipped_portion(source: &str) -> &str {
    match source.find("#[cfg(test)]") {
        Some(at) => &source[..at],
        None => source,
    }
}

/// **The pin control is mounted in the one profile renderer, at BOTH densities.**
///
/// The phase 45 UI spec asks for a pin control "in the profile header **and** on the canvas inspection
/// panel". Those are one mount, because D-11 fixes that there is one profile renderer and 45-13
/// mounts it in the panel at `Density::Compact`. The load-bearing assertion is therefore not that
/// the control exists — it is that the mount is **not** gated on `Density::Full`, the way the as-at
/// control deliberately is, because a gate there would silently remove the control from the canvas
/// while every unit test stayed green.
///
/// PROVE THIS CAN FAIL: wrap the mount in `(density == Density::Full).then(...)` — the
/// density-derived-origin assertion fails. Delete the mount — the first assertion fails.
#[test]
fn the_pin_control_is_mounted_in_the_one_profile_renderer_at_both_densities() {
    let profile = source_of("src/profile/mod.rs");

    let mount = profile
        .find("<PinControl")
        .expect("the profile header must mount the pin control");
    let end = profile[mount..]
        .find("/>")
        .expect("the mount must be a self-closing element");
    let element = &profile[mount..mount + end];

    assert!(
        element.contains("origin=pin_origin_for(density)"),
        "the pin's origin must be DERIVED from the density, which is what makes one mount serve \
         both surfaces: {element}"
    );
    assert!(
        !element.contains("Density::Full"),
        "the mount must not be gated on a density — that would drop it from the canvas panel \
         while every other test stayed green: {element}"
    );

    // The as-at control beside it IS gated, deliberately (45-12 §8), and the contrast is the
    // point: if this stops being true the two controls have been conflated.
    assert!(
        profile.contains("(density == Density::Full).then(|| (profile.id.clone()"),
        "the as-at control is still the Full-only one"
    );
}

/// **Seven canvas clicks in ten reach a node with no profile, and they can still be pinned.**
///
/// 45-11 measured that 103,973 of 362,609 projected nodes carry a class with a profile route. The
/// inspection panel's no-profile branch is therefore the common case on the canvas, and a pin
/// control that lived only inside the compact profile would be unavailable there.
///
/// PROVE THIS CAN FAIL: delete the `<PinControl` from `side_card.rs`'s `Some(class)` arm.
#[test]
fn a_canvas_node_with_no_profile_route_can_still_be_pinned() {
    let side_card = source_of("src/side_card.rs");
    let branch = side_card
        .find("Some(class) => view! {")
        .expect("the no-profile branch must exist");
    let arm_end = side_card[branch..]
        .find("}.into_any(),")
        .expect("the arm must close");
    let arm = &side_card[branch..branch + arm_end];
    assert!(
        arm.contains("<PinControl"),
        "the no-profile branch must mount the pin control: {arm}"
    );
    assert!(
        arm.contains("pin_origin_for(Density::Compact)"),
        "and record that the pin came from the canvas"
    );
}

/// **There is one definition of what a pin points at.**
///
/// The identity of a pin is the entity id alone; `PinTarget::member_key` derives the record id from
/// it and a `UNIQUE` index is the second witness. A second construction site is how a second idea
/// of the identity arrives — the as-at appended to the id, say — and it would produce two rows for
/// one subject with nothing failing.
///
/// PROVE THIS CAN FAIL: build a `PinTargetView::Entity { .. }` inline in `pin.rs` instead of
/// calling `entity_pin_target`.
#[test]
fn a_pin_target_is_constructed_in_exactly_one_module() {
    let mut offenders = Vec::new();
    for (name, body) in shipped_sources() {
        if name == "src/investigation/mod.rs" {
            continue;
        }
        if without_comments(shipped_portion(&body)).contains("PinTargetView::Entity") {
            offenders.push(name);
        }
    }
    assert!(
        offenders.is_empty(),
        "only `investigation::entity_pin_target` may build an entity pin target: {offenders:?}"
    );

    let module = source_of("src/investigation/mod.rs");
    assert_eq!(
        module.matches("pub fn entity_pin_target(").count(),
        1,
        "exactly one definition"
    );
    // And the control reaches it rather than reaching around it.
    let pin = source_of("src/investigation/pin.rs");
    assert!(
        pin.matches("entity_pin_target(").count() >= 2,
        "both the pin and the unpin path must build their target the one way"
    );
}

/// **The date window never reaches the pin's identity.**
///
/// The as-at is carried to the control for one purpose — to say that it is *not* saved — and the
/// only function that may read it is the notice. This is the sweep half of
/// `a_pin_target_is_the_entity_id_and_nothing_else`: the unit test proves the target ignores a
/// window, this proves nothing on the write path is handed one.
///
/// PROVE THIS CAN FAIL: pass `as_at` into `pin_and_reopen` or into `entity_pin_target`.
#[test]
fn the_as_at_is_read_only_by_the_notice_that_says_it_is_not_saved() {
    let pin = source_of("src/investigation/pin.rs");
    let uses: Vec<&str> = pin
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .filter(|line| line.contains("as_at"))
        .collect();
    assert!(
        !uses.is_empty(),
        "non-vacuity: the control must actually receive the window, or this proves nothing"
    );
    for line in &uses {
        let ok = line.contains("as_at_pin_notice")
            || line.contains("as_at: Option<String>")
            || line.contains("let notice")
            || line.trim() == "as_at: Option<String>,";
        assert!(
            ok,
            "the window may only reach the notice, never a request: {line}"
        );
    }
    assert!(
        pin.contains("as_at_pin_notice(as_at.as_deref())"),
        "and the notice must be the thing that reads it"
    );
}

/// **The investigation list is reachable from the shell, not only from a page the operator happens
/// to be on.**
///
/// This plan's second truth, and it is an "every X in the UI" rule, so it needs a test that finds
/// a **call site** rather than one that proves a page renders. A Home card would not satisfy it:
/// Home is a page the operator happens to be on.
///
/// The load-bearing part is *where* the link sits. It must be in the `<header>`, which `app.rs`
/// renders **outside** `<Routes>` — that is what makes it present on every route rather than on one
/// — and it must be an `<A>` rather than a bare `<a href>`, because a bare anchor reloads the whole
/// wasm application and is a page load dressed as navigation.
///
/// PROVE THIS CAN FAIL: move the link below `</header>`, or change it to `<a href=`. Both fail.
#[test]
fn the_investigation_list_is_reachable_from_the_shell_on_every_route() {
    let app = without_comments(&source_of("src/app.rs"));

    let header_start = app.find("<header").expect("the shell has a header");
    let header_end = app.find("</header>").expect("the header closes");
    assert!(header_start < header_end);
    let header = &app[header_start..header_end];

    assert!(
        header.contains("href=INVESTIGATIONS_PATH"),
        "the shell header must link to the investigation list: {header}"
    );
    assert!(
        header.contains("<A\n") || header.contains("<A "),
        "the shell link must be a router anchor, not a full page load"
    );

    // The header is outside the route table, which is the whole claim: it renders on every route.
    let routes_at = app.find("<Routes").expect("a route table exists");
    assert!(
        header_end < routes_at,
        "the header must be rendered outside <Routes>, or the link is on one page rather than all"
    );
    // And the router must wrap the header, or `<A>` has no context and the link silently dies.
    let router_at = app.find("<Router>").expect("a router exists");
    assert!(
        router_at < header_start,
        "<Router> must wrap the header for the shell link to navigate client-side"
    );

    // Both routes are mounted. (`routes_render.rs` counts them; this names them.)
    assert!(app.contains(r#"path!("investigations") view=InvestigationsPage"#));
    assert!(app.contains(r#"path!("investigations/:id") view=InvestigationPage"#));

    // The shell provides the open-investigation context exactly once, so the profile header and
    // the canvas panel cannot be looking at two copies.
    assert_eq!(
        app.matches("provide_active_investigation()").count(),
        1,
        "exactly one provider"
    );
}

/// **There is one notes editor, and both note surfaces are it.**
///
/// The phase 45 UI spec calls notes analyst work product. Two editors would be two save-state machines,
/// and the one used less would be the one that lost work — the same class of failure as two profile
/// renderers, which D-11 forbids for the same reason.
///
/// PROVE THIS CAN FAIL: give the member rows their own `<textarea>` instead of a `<NoteField>` —
/// the textarea count assertion fails.
#[test]
fn both_note_surfaces_are_the_one_editor() {
    let notes = source_of("src/investigation/notes.rs");
    let page = source_of("src/investigation/page.rs");

    assert_eq!(
        notes.matches("pub fn NoteField(").count(),
        1,
        "exactly one editor is defined"
    );
    assert!(
        page.contains("target=NoteTarget::Investigation")
            && page.contains("target=NoteTarget::Member"),
        "both surfaces must mount it"
    );
    // And both must hand it what the server holds, or a reopened note renders blank over a note
    // that is really there.
    assert!(
        page.contains("initial=detail.investigation.notes.clone()")
            && page.contains("initial=pin.note.clone()"),
        "both must be seeded from the reopened investigation"
    );

    // No second text area anywhere on this surface. `src/alerts/withdraw.rs`'s reason field
    // (46-09, D-10) is deliberately excepted: a withdrawal reason is a required justification
    // stored with a write action, not investigation work product — a different concept from the
    // one-`NoteField` rule this test protects, not a second copy of it.
    let mut stray = Vec::new();
    for (name, body) in shipped_sources() {
        if name == "src/investigation/notes.rs" || name == "src/alerts/withdraw.rs" {
            continue;
        }
        if without_comments(shipped_portion(&body)).contains("<textarea") {
            stray.push(name);
        }
    }
    assert!(
        stray.is_empty(),
        "every free-text note must go through the one editor: {stray:?}"
    );
}

/// **Deleting takes the named confirm step, and nothing else on this surface does.**
///
/// The phase 45 UI spec: deleting an investigation is the one destructive action this phase adds, and it
/// requires a confirm step naming the investigation. Unpinning and clearing filters are not
/// destructive and take none.
///
/// PROVE THIS CAN FAIL: call `on_delete` straight from the row's button instead of opening
/// `confirming` — the guard assertion fails.
#[test]
fn deleting_an_investigation_takes_the_named_confirm_step() {
    let page = source_of("src/investigation/page.rs");

    assert!(
        page.contains("delete_confirm(&name_for_confirm)"),
        "the confirmation must name the investigation, not ask a generic question"
    );
    assert!(
        page.contains("{DELETE_ACTION}") && page.contains("{DELETE_CANCEL}"),
        "both the action and the escape must be rendered"
    );

    // The delete request is reachable only from inside the confirm branch.
    let confirm_at = page
        .find("delete_confirm(&name_for_confirm)")
        .expect("checked above");
    let delete_at = page
        .find("on:click=move |_| on_delete(")
        .expect("the delete button exists");
    assert!(
        confirm_at < delete_at,
        "the delete button must sit inside the branch the confirmation opens"
    );

    // Unpinning, by contrast, takes no confirmation — asserted so the two do not converge.
    assert!(
        page.contains("on:click=move |_| on_remove("),
        "removing a member is a plain control"
    );
    assert!(
        !page.contains("delete_confirm(&entity_id)"),
        "unpinning must not acquire a destructive confirmation it does not need"
    );
}

/// **Reopening an investigation also opens it**, so the next pin from a profile lands in the set
/// the analyst just came back to rather than prompting for a name they already chose.
///
/// PROVE THIS CAN FAIL: delete the `active.detail.set(Some(detail.clone()))` line — the loop breaks
/// silently and every other test stays green.
#[test]
fn reopening_an_investigation_makes_it_the_open_one() {
    let page = source_of("src/investigation/page.rs");
    assert!(
        page.contains("active.detail.set(Some(detail.clone()))"),
        "reopening must set the session's open investigation"
    );
    assert!(
        page.contains("fetch_investigation(&id).await"),
        "and the set must come from the server, in one request, not from anything held locally"
    );
    // Nothing on this surface may persist an investigation in the browser: phases 47 and 48 read
    // this primitive server-side, where browser storage is invisible.
    let mut offenders = Vec::new();
    for (name, body) in shipped_sources() {
        if !name.starts_with("src/investigation/") {
            continue;
        }
        let shipped = without_comments(shipped_portion(&body));
        if shipped.contains("local_storage") || shipped.contains("localStorage") {
            offenders.push(name);
        }
    }
    assert!(
        offenders.is_empty(),
        "an investigation lives in the analysis database, never in browser storage: {offenders:?}"
    );
}
