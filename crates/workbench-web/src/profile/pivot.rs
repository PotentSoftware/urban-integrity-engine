//! **Pivot link construction — the one place an `EntityRef` becomes a profile href.**
//!
//! Plan 45-10 built this policy inside `profile/mod.rs`, for the profile screen's own rows. Plan
//! 45-11 Task 2 moves it here unchanged in behaviour and widens its *callers*, because PIV-03's
//! wording is "every neighbour, contract, address, link, **report evidence row and graph result**
//! on screen is a click-through" — and the last two live on the Analysis page and the Graph page,
//! which were still rendering every entity reference as inert text.
//!
//! One implementation, three surfaces: [`pivot_href`] is called by `profile::sections::ProfileRow`
//! (the profile's eighteen sections), by [`crate::entity_ref_view::EntityRefView`] (the Analysis
//! page's evidence refs, the Graph page's path hops / community members / ranked entities, and the
//! canvas side card), and by nothing else. A second copy of "which kind opens" is the drift that
//! makes an analyst's click land on a 404 in one place and a page in another.
//!
//! # `RecordKind::Entity` is a catch-all, and the kind field alone is not enough
//!
//! 45-10's policy was `opens_a_profile(kind)` and nothing else, which is exactly right for the
//! rows it had: every `pivot` on the profile carries `crates/workbench`'s `node_kind_name` — the
//! node's real class. The Analysis page's evidence refs do not. They carry
//! `RecordKind::name`, whose `Entity` variant is documented in `crates/schema` as *"the catch-all
//! every node id is addressable as"* — it means "this id names some graph node", not "this node is
//! a `NodeWeight::Entity`".
//!
//! Measured on the live analysis's persisted report, 2026-09-16: of 1,874 evidence refs, 1,844
//! carry `kind: "Entity"`, and **50 of those 1,844 name a node class the profile route cannot
//! open** — 20 `CF:` contracts, 19 `LOC:` plus 1 `location:` addresses, and 10 `ward:` wards.
//! A kind-only policy would have shipped fifty links that resolve to
//! "No entity with id … exists in this analysis."
//!
//! Two changes close it, and both are needed:
//!
//! 1. **the producer**, `crates/workbench::report_view::entity_ref_from_record_ref`, now resolves
//!    each ref's id through the boot-time `LabelIndex` and puts the **node class** on the wire, so
//!    the two vocabularies genuinely agree rather than agreeing in a doc comment;
//! 2. **this guard**, [`namespace_class`], which refuses a pivot whose id sits in a namespace the
//!    graph mints for a non-opening node class, whatever the `kind` field says. It is needed as
//!    well as (1) because `workbench_report:latest` is **persisted and rehydrated at boot**: a
//!    report written before (1) keeps its stale kinds and is served unchanged until the next probe
//!    run.
//!
//! The guard only ever *narrows*. It cannot turn a plain-text row into an anchor, so the worst an
//! unrecognised namespace can do is leave today's behaviour in place.

use urban_integrity_workbench_dto::EntityRef;

/// The node classes `GET /api/entity/profile` can actually open.
///
/// This is not a style choice and not a shortlist of the interesting ones: it is
/// `query_entity_by_id`'s own reach, written down. That function looks for a `NodeWeight::Entity`
/// birth row and falls back to `NodeWeight::OffshoreEntity`; it has no branch for any other class.
/// The strings are `crates/workbench`'s `node_kind_name` spellings, which is what every `pivot`
/// on the wire carries (D-06 spelled `RecordKind`'s two node-class variants to match, so one node
/// class has one spelling whichever producer filled the field).
///
/// **When a later plan gives a `Location` or `Contract` node a subject lookup, this list grows and
/// the anchors light up on their own** — no row type has to be revisited, because the policy is
/// applied per row by the target's class.
///
/// # What the policy costs, measured
///
/// Measured on the live analysis, 2026-09-16. Of the five row classes 45-11 Task 1 gave a pivot id,
/// **two render as anchors and three as plain text**: contracts, planning applications and
/// addresses target `Contract`/`Location` nodes, which have no profile route.
///
/// The two that do open are wider than they look, because the `director:` namespace is
/// `NodeWeight::Entity`, not `NodeWeight::Officer` — the projected entity table holds exactly four
/// namespaces (`GB-MUT:` 32,407, `director:` 20,660, `GB-COH:` 16,130, `CF-PARTY:` 4,668; 73,865
/// total), so a director slug opens a profile like a company does. Over the 16,878 `auto_linked`
/// and `confirmed` identity links in that analysis, the OSINT-link pivot's targets are `GB-COH:`
/// 9,821, `director:` 7,020, `PA:` 21, `GB-MUT:` 12 and `CF-PARTY:` 4 — so **16,857 of 16,878
/// (99.88%) are node classes this list opens**, and the 21 `PA:` political-actor targets are the
/// only ones rendered as plain text. Related-entity rows pivot on `GB-COH:{ch_number}` and open
/// likewise.
pub const OPENABLE_KINDS: &[&str] = &["Entity", "OffshoreEntity"];

/// Whether a pivot target of this node class has a profile route today. See [`OPENABLE_KINDS`].
///
/// **This is the class test only.** A caller holding a whole [`EntityRef`] wants
/// [`opens_a_profile_ref`], which also applies [`namespace_class`] — see the module doc for the
/// fifty live refs that pass this test and still 404.
pub fn opens_a_profile(kind: &str) -> bool {
    OPENABLE_KINDS.contains(&kind)
}

/// Id namespaces the graph mints for node classes with no profile route, and the class each names.
///
/// Not a parallel taxonomy (D-06 forbids one): every pair here is transcribed from the prose that
/// already fixes it in `crates/schema`'s `RecordKind` — *"`CF:…` for a Contracts Finder award,
/// `planning:…` for a planning application"* for [`Contract`], *"`location:…` or `LOC:…`, both live
/// namespaces"* for [`Location`] — plus `ward:`, which `EntityId`'s ward keying fixes and which
/// `RecordKind::Entity`'s own doc comment lists among the ids that variant is a catch-all for.
///
/// Deliberately a **deny list of the classes that cannot open**, not an allow list of the ones that
/// can. An allow list would have to enumerate every namespace whose nodes are `NodeWeight::Entity`
/// (`GB-COH:`, `GB-MUT:`, `director:`, `CF-PARTY:`, `GB-OCOD:`, …) and would silently stop linking
/// a real entity the day the corpus grew a namespace nobody had written down. This list fails the
/// other way: an unrecognised namespace keeps whatever the `kind` field said, which is 45-10's
/// shipped behaviour exactly.
///
/// `CF-PARTY:` is an `Entity` namespace and is **not** matched by `CF:` — the trailing colon in
/// each prefix is load-bearing, and `a_contracts_finder_party_is_not_a_contract` pins it.
pub const NON_OPENING_NAMESPACES: &[(&str, &str)] = &[
    ("CF:", "Contract"),
    ("planning:", "Contract"),
    ("location:", "Location"),
    ("LOC:", "Location"),
    ("ward:", "Ward"),
];

/// The node class `id`'s namespace names, when that namespace belongs to a class with no profile
/// route. `None` means "this id says nothing that contradicts its `kind`", which includes every
/// openable namespace. See [`NON_OPENING_NAMESPACES`].
pub fn namespace_class(id: &str) -> Option<&'static str> {
    NON_OPENING_NAMESPACES
        .iter()
        .find(|(prefix, _)| id.starts_with(prefix))
        .map(|(_, class)| *class)
}

/// Whether this whole reference opens a profile: its class must be openable **and** its id must
/// not sit in a namespace that names a class which is not.
///
/// The second half is the half a `kind`-only test cannot do, and the module doc measures what it
/// is worth on the live corpus.
pub fn opens_a_profile_ref(entity: &EntityRef) -> bool {
    opens_a_profile(&entity.kind) && namespace_class(&entity.id).is_none()
}

/// `encodeURIComponent`, for one path segment of `/entity/{id}`, **plus the dot**.
///
/// Entity ids carry colons (`GB-COH:99000001`), and the `LOC:` namespace carries spaces and commas
/// (a postcode, then a house name and street, with the separators intact). `leptos_router` matches
/// a path with `decode_uri` (JavaScript's `decodeURI`, which leaves `%2E` and `%3A` alone because
/// `encodeURI` would never have produced them) and then decodes each captured parameter with
/// `decode_uri_component`, so a fully-encoded segment round-trips back to the exact id — through a
/// client-side pivot, a `pushState`, a back button and a hard reload alike.
///
/// # Why `.` is encoded when `encodeURIComponent` leaves it alone
///
/// `crates/workbench`'s `is_spa_route` treats any path carrying a **file extension** as a static
/// asset rather than a client-side route, so `/entity/location:1%20High%20St.` would resolve to a
/// real `404` on a hard reload or a pasted link instead of to the SPA shell. Plan 45-10 §7.7 found
/// the hazard, checked it was not reachable (`analysis::naming::slugify` reduces every
/// non-alphanumeric character to `_`, and none of the four namespaces in the projected entity table
/// carries a dot) and handed it here. Encoding the dot is the whole fix: `%2E` carries no extension
/// as far as `std::path::Path::extension` is concerned, and `decodeURIComponent` turns it back into
/// a dot in the route parameter.
///
/// The wasm build calls the browser's own `encodeURIComponent` and then escapes the dot the same
/// way; the host build (unit tests) has no `js_sys`, so it reproduces that function's rule with
/// `.` removed from the unreserved set — the unreserved set is `A-Z a-z 0-9 - _ ! ~ * ' ( )` and
/// everything else is percent-encoded byte by byte. The two halves are the same function, which is
/// what lets a host test assert the href a browser builds.
pub(crate) fn encode_segment(value: &str) -> String {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::encode_uri_component(value)
            .as_string()
            .unwrap_or_default()
            .replace('.', "%2E")
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut out = String::with_capacity(value.len());
        for byte in value.as_bytes() {
            let c = *byte as char;
            // `.` is deliberately absent from this set — see the doc comment above.
            if c.is_ascii_alphanumeric() || "-_!~*'()".contains(c) {
                out.push(c);
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
        out
    }
}

/// The profile href for one entity id, carrying the current as-at date forward.
///
/// The as-at date travels in the URL rather than in a signal, which is what makes the phase 45 UI spec
/// Open Question 2's answer ("the date persists across a pivot") true by construction: a pivot is
/// a navigation to a URL that already states the filter, so back, forward, a reload and a pasted
/// link all agree about what the screen is showing.
pub fn profile_href(id: &str, as_at: Option<&str>) -> String {
    match as_at {
        Some(date) if !date.is_empty() => {
            format!(
                "/entity/{}?as_at={}",
                encode_segment(id),
                encode_segment(date)
            )
        }
        _ => format!("/entity/{}", encode_segment(id)),
    }
}

/// The pivot policy for a reference that carries **no class** — the only half available to one.
///
/// A saved investigation's pin stores a *reference* and nothing else (45-07): an entity id, with no
/// `kind` beside it, because a pin is not a copy of the facts it points at. So
/// [`opens_a_profile_ref`]'s first half has nothing to read, and the id's namespace is the whole
/// test that remains.
///
/// It lives here, beside the two-part policy, rather than in `investigation/mod.rs`, for the reason
/// `the_shared_pivot_policy_is_the_only_href_builder_a_surface_calls` exists: two rules for which
/// reference opens is one analyst clicking into a 404. This is a **weaker** test than the pivot's,
/// not a different one — it can still return a link for an id in a namespace nobody has written
/// down, which is 45-10's shipped behaviour for an unrecognised namespace exactly.
pub fn pivot_href_by_id(id: &str) -> Option<String> {
    namespace_class(id)
        .is_none()
        .then(|| profile_href(id, None))
}

/// The href a pivot renders as an anchor, or `None` when it must render as plain text.
///
/// Three things make it `None`, and they are different failures worth keeping apart:
///
/// * the row carries no pivot at all (`OfficerRowView`, `PropertyRowView`, `LinkedRecordRowView`,
///   `BeneficialOwnerRowView`, `DisqualificationRowView` and a *director* `RelatedEntityRowView` —
///   45-11 Task 1 §3 lists them, and four of the six have an id in scope at their construction
///   site that a later plan can carry);
/// * the target's node class has no profile route ([`opens_a_profile`]) — every contract, planning
///   application and address today;
/// * the target's class *says* it opens but its id names a namespace that does not
///   ([`namespace_class`]) — the fifty live evidence refs the module doc measures.
pub fn pivot_href(pivot: Option<&EntityRef>, as_at: Option<&str>) -> Option<String> {
    let target = pivot?;
    opens_a_profile_ref(target).then(|| profile_href(&target.id, as_at))
}

/// **The whole state of `/entity/:id?as_at=…`.**
///
/// The profile screen is a pure function of this pair, and that is the entire mechanism behind
/// PIV-03's "browser back/forward moves through the pivot history correctly". Nothing else is
/// needed: a pivot is a navigation to a URL that already states both halves, so a `popstate` —
/// which hands the router a URL and nothing else — restores the id *and* the date window, in one
/// step, with no signal to keep in sync and no chance of the banner disagreeing with the rows
/// beneath it. Plan 45-10 put the date in the URL for the pivot's sake; back and forward inherit it.
///
/// It is a type rather than a tuple because [`EntityProfilePage`](crate::profile::EntityProfilePage)
/// compares two of them — see [`response_is_still_wanted`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRoute {
    pub id: String,
    pub as_at: Option<String>,
}

impl ProfileRoute {
    pub fn new(id: impl Into<String>, as_at: Option<String>) -> Self {
        Self {
            id: id.into(),
            as_at: as_at.filter(|d| !d.is_empty()),
        }
    }
}

/// Whether a profile response requested for `requested` may still be shown, now that the address
/// bar says `current`.
///
/// **A race that plan 45-11 Task 3 created and therefore owns.** While a pivot was a full document
/// navigation (`location.assign`, 45-10's deliberate placeholder) every hop discarded the running
/// wasm instance, so two profile fetches could never be in flight together. Client-side routing
/// removes that accident: three quick presses of Back leave three `GET /api/entity/profile`
/// requests racing, and the last one to *land* is not the one the analyst is looking at. Without
/// this check the screen settles on whichever response was slowest — a profile whose id contradicts
/// the URL, the breadcrumb and the copy button beside it.
///
/// The response is wanted only if both halves still agree, the as-at included: reloading a profile
/// with a different date window is as much a different screen as a different entity.
pub fn response_is_still_wanted(requested: &ProfileRoute, current: &ProfileRoute) -> bool {
    requested == current
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The browser's half of the round trip, modelled.** `leptos_router` matches a route against
    /// the *encoded* `location.pathname` and then hands each captured parameter (and each query
    /// value) through `decodeURIComponent` — `Url::unescape` in its `params.rs`. This is that
    /// decode, host-side, so a test with no DOM can assert what the page will actually be given.
    ///
    /// Only the decoding half is modelled. The encoding half is [`encode_segment`] itself, the
    /// shipped function, so the round trip below is a real one rather than two test helpers
    /// agreeing with each other.
    fn decode_component(value: &str) -> String {
        let bytes = value.as_bytes();
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let Ok(byte) = u8::from_str_radix(&value[i + 1..i + 3], 16) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8(out).expect("a percent-decoded id must be valid text")
    }

    /// The [`ProfileRoute`] `EntityProfilePage` will read out of the router, given one href — i.e.
    /// what the analyst's screen becomes after navigating to it, whether by clicking, by pressing
    /// Back, or by pasting the link.
    fn route_of(href: &str) -> ProfileRoute {
        let (path, search) = match href.split_once('?') {
            Some((path, search)) => (path, Some(search)),
            None => (href, None),
        };
        let id = decode_component(
            path.strip_prefix("/entity/")
                .expect("a profile href starts with /entity/"),
        );
        let as_at = search.and_then(|s| {
            s.split('&')
                .find_map(|pair| pair.strip_prefix("as_at="))
                .map(decode_component)
        });
        ProfileRoute::new(id, as_at)
    }

    fn entity_ref(id: &str, kind: &str) -> EntityRef {
        EntityRef {
            id: id.to_string(),
            name: id.to_string(),
            kind: kind.to_string(),
        }
    }

    #[test]
    fn only_the_two_node_classes_the_read_path_resolves_open_a_profile() {
        // `query_entity_by_id` looks for a `NodeWeight::Entity` birth row and falls back to
        // `NodeWeight::OffshoreEntity`. Every other class `node_kind_name` can emit must be plain
        // text, including the two `RecordKind` gained in 45-11 Task 1.
        for kind in ["Entity", "OffshoreEntity"] {
            assert!(opens_a_profile(kind), "{kind} must open a profile");
        }
        for kind in [
            "Contract",
            "Location",
            "Officer",
            "Ward",
            "Psc",
            "Charity",
            "Property",
            "Disqualification",
            "PoliticalActor",
            "CompanyNumber",
            "IdentityLink",
        ] {
            assert!(
                !opens_a_profile(kind),
                "{kind} has no profile route — it must render as plain text, not a dead link"
            );
        }
    }

    /// PROVE THIS CAN FAIL: add `"Location"` to `OPENABLE_KINDS` and the loop above fails naming
    /// it; drop `"OffshoreEntity"` and the first loop fails. The list is the policy, so the test
    /// is written against the list rather than against a single call.
    #[test]
    fn a_contract_or_address_pivot_renders_as_plain_text_not_an_anchor() {
        let contract = entity_ref("CF:x9995-cf-00001", "Contract");
        let address = entity_ref("location:1-example-street", "Location");
        let company = entity_ref("GB-COH:X6999001", "Entity");
        assert_eq!(pivot_href(Some(&contract), None), None);
        assert_eq!(pivot_href(Some(&address), None), None);
        // Non-vacuity: a class that DOES open must produce an href, or this test would pass
        // against a `pivot_href` that returned `None` for everything.
        assert_eq!(
            pivot_href(Some(&company), None).as_deref(),
            Some("/entity/GB-COH%3AX6999001")
        );
        assert_eq!(pivot_href(None, None), None);
    }

    /// **The fifty live refs.** `RecordKind::Entity` is a catch-all for "some graph node id", so a
    /// `kind` of `"Entity"` on an evidence ref does not mean the node is a `NodeWeight::Entity`.
    /// Every id below appears with exactly this kind on the live analysis's persisted report.
    ///
    /// PROVE THIS CAN FAIL: drop the `namespace_class(...).is_none()` conjunct from
    /// `opens_a_profile_ref` and all four refusals below become hrefs.
    #[test]
    fn an_entity_kind_over_a_non_entity_namespace_is_still_plain_text() {
        for id in [
            "CF:x9995-cf-00001",
            "LOC:AB101JB:37 Example Place",
            "location:/-temporary-location",
            "ward:X05999001",
            "planning:x9995-plan-00001",
        ] {
            let mislabelled = entity_ref(id, "Entity");
            assert_eq!(
                pivot_href(Some(&mislabelled), None),
                None,
                "{id} names a node class with no profile route, whatever its kind field says"
            );
        }
        // Non-vacuity, twice over: the same kind over an entity namespace still opens, and the
        // guard is the thing refusing above rather than a blanket `None`.
        assert!(pivot_href(Some(&entity_ref("GB-COH:X6999001", "Entity")), None).is_some());
        assert!(pivot_href(Some(&entity_ref("director:jane-smith", "Entity")), None).is_some());
    }

    /// `CF-PARTY:` is a `NodeWeight::Entity` namespace (4,668 of them in the projected entity
    /// table) and must not be swallowed by the `CF:` contract prefix. The trailing colon on every
    /// prefix in [`NON_OPENING_NAMESPACES`] is what keeps them apart.
    ///
    /// PROVE THIS CAN FAIL: drop the colon from the `"CF:"` entry and this refuses a live entity.
    #[test]
    fn a_contracts_finder_party_is_not_a_contract() {
        assert_eq!(namespace_class("CF-PARTY:example-council"), None);
        assert_eq!(namespace_class("CF:ocds-b5fd17-0001"), Some("Contract"));
        assert!(pivot_href(
            Some(&entity_ref("CF-PARTY:example-council", "Entity")),
            None
        )
        .is_some());
    }

    #[test]
    fn the_as_at_date_rides_the_pivot_href_so_it_survives_a_hop() {
        let company = entity_ref("GB-COH:X6999001", "Entity");
        let href = pivot_href(Some(&company), Some("2021-06-30")).expect("an Entity pivot opens");
        assert_eq!(href, "/entity/GB-COH%3AX6999001?as_at=2021-06-30");
        // And an unfiltered profile pivots to an unfiltered profile — the parameter is carried,
        // never invented.
        assert!(!pivot_href(Some(&company), None).unwrap().contains("as_at"));
    }

    #[test]
    fn an_id_with_spaces_and_commas_survives_the_path_segment() {
        // The `LOC:` namespace carries both. This id has no profile route today, but the encoder
        // is the same one every href uses and the day that class opens is not the day to discover
        // the URL was malformed.
        let encoded = profile_href("LOC:AB101JB:Example Partners,37 Example Place", None);
        assert_eq!(
            encoded,
            "/entity/LOC%3AAB101JB%3AExample%20Partners%2C37%20Example%20Place"
        );
        assert!(!encoded.contains(' '));
    }

    /// **The deep-link hazard 45-10 §7.7 handed this plan.** `crates/workbench`'s `is_spa_route`
    /// calls a path with a file extension a static asset, so an unescaped dot in an entity id turns
    /// a pasted link or a hard reload into a real 404 instead of the SPA shell.
    ///
    /// PROVE THIS CAN FAIL: put `.` back in `encode_segment`'s unreserved set and both assertions
    /// below fail — `std::path::Path::new("entity/x.y").extension()` is `Some("y")`.
    #[test]
    fn a_dot_in_an_id_is_escaped_so_the_path_carries_no_file_extension() {
        let href = profile_href("location:12 St. Mary's Rd", None);
        assert!(
            !href.contains('.'),
            "no literal dot may reach the path segment: {href}"
        );
        assert!(
            href.contains("%2E"),
            "the dot must be escaped, not dropped: {href}"
        );
        // The predicate `crates/workbench::assets::is_spa_route` actually applies, restated here
        // because this crate must not depend on that one.
        let path = href.trim_start_matches('/');
        assert!(
            std::path::Path::new(path).extension().is_none(),
            "{path} must not look like a static asset to the SPA fallback"
        );
    }
    /// **Three pivots, walked back to the start and forward to the end, with the date window
    /// intact at every stop.** PIV-03: "pivoting is recursive and browser back/forward moves
    /// through the pivot history correctly", and "back returns to the previous profile at … its
    /// previous as-at setting".
    ///
    /// The chain is built the way the screen builds it — each hop's href is produced by
    /// [`pivot_href`] from the as-at that [`route_of`] read off the *previous* hop's URL, never
    /// from a constant — so the test is over the composition, not over one call. Walking it is a
    /// `Vec` plus a cursor because the browser owns the real stack; what the app owns, and what is
    /// asserted here, is that every entry on that stack names a complete screen.
    ///
    /// PROVE THIS CAN FAIL: make `ProfileRoute::new` store `as_at: None` — the page forgetting the
    /// date it read from the URL, which is exactly the clause above — and the second hop is built
    /// unfiltered, so every assertion about the date fails from there on.
    #[test]
    fn back_and_forward_walk_the_pivot_chain_with_the_as_at_intact() {
        const AS_AT: &str = "2021-06-30";
        let chain = [
            entity_ref("GB-COH:X6999001", "Entity"),
            entity_ref("director:jane-smith", "Entity"),
            // An id carrying a dot and a comma, so the walk exercises the escaping the SPA
            // fallback depends on rather than only colon-separated ids.
            entity_ref("GB-OCOD:St. Example Holdings, Ltd", "OffshoreEntity"),
            entity_ref("GB-MUT:X0125R", "Entity"),
        ];

        // The entry point: a profile opened with a date window already set.
        let mut stack = vec![profile_href(&chain[0].id, Some(AS_AT))];
        // Three pivots. Each is built from the as-at the CURRENT page carries, read back out of
        // that page's own URL — which is the property the whole chain rests on.
        for target in &chain[1..] {
            let here = route_of(stack.last().expect("the stack is never empty"));
            let href = pivot_href(Some(target), here.as_at.as_deref())
                .expect("every target in this chain opens a profile");
            stack.push(href);
        }

        // Non-vacuity, before any walking: four distinct entries, four distinct entities, and at
        // least one href that the encoder had real work to do on.
        assert_eq!(stack.len(), 4, "three pivots make four history entries");
        let mut distinct = stack.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            4,
            "a chain of identical URLs proves nothing"
        );
        assert!(
            stack.iter().any(|href| href.contains("%2E")),
            "the chain must include an id the path encoder had to escape: {stack:?}"
        );

        let expected_at =
            |position: usize| ProfileRoute::new(&chain[position].id, Some(AS_AT.into()));

        // The analyst is on the last page. Walk all the way back to the first.
        let mut cursor = stack.len() - 1;
        assert_eq!(route_of(&stack[cursor]), expected_at(cursor));
        let mut steps_back = 0;
        while cursor > 0 {
            cursor -= 1;
            steps_back += 1;
            let restored = route_of(&stack[cursor]);
            assert_eq!(
                restored,
                expected_at(cursor),
                "back {steps_back} must restore both the entity and the date window"
            );
            assert_eq!(
                restored.as_at.as_deref(),
                Some(AS_AT),
                "the as-at survives step {steps_back} back"
            );
        }
        assert_eq!(steps_back, 3, "the walk back must be three steps, not one");

        // And forward again, to the end.
        let mut steps_forward = 0;
        while cursor + 1 < stack.len() {
            cursor += 1;
            steps_forward += 1;
            let restored = route_of(&stack[cursor]);
            assert_eq!(
                restored,
                expected_at(cursor),
                "forward {steps_forward} must restore both the entity and the date window"
            );
        }
        assert_eq!(steps_forward, 3, "the walk forward must be three steps");
        assert_eq!(route_of(&stack[cursor]).id, chain[3].id);
    }

    /// An unfiltered chain stays unfiltered: the parameter is carried, never invented, at every
    /// hop rather than only at the first.
    #[test]
    fn a_chain_with_no_date_window_never_acquires_one() {
        let chain = [
            entity_ref("GB-COH:X6999001", "Entity"),
            entity_ref("director:jane-smith", "Entity"),
            entity_ref("GB-MUT:X0125R", "Entity"),
        ];
        let mut stack = vec![profile_href(&chain[0].id, None)];
        for target in &chain[1..] {
            let here = route_of(stack.last().unwrap());
            stack.push(pivot_href(Some(target), here.as_at.as_deref()).expect("opens"));
        }
        assert_eq!(stack.len(), 3);
        for href in &stack {
            assert!(!href.contains("as_at"), "{href} invented a date window");
            assert_eq!(route_of(href).as_at, None);
        }
        // Non-vacuity: the same chain WITH a window does carry one, so this is not a test that
        // passes because `route_of` never finds a query string.
        assert_eq!(
            route_of(&profile_href(&chain[0].id, Some("2021-06-30")))
                .as_at
                .as_deref(),
            Some("2021-06-30")
        );
    }

    /// **The race client-side routing creates.** Pivot A -> B, then Back to A, with B's slower
    /// response landing last. Without [`response_is_still_wanted`] the screen settles on B while
    /// the URL, the breadcrumb and the copy button all say A.
    ///
    /// PROVE THIS CAN FAIL: make `response_is_still_wanted` return `true` unconditionally and the
    /// final assertion reads B.
    #[test]
    fn a_response_for_a_route_the_analyst_has_already_left_is_dropped() {
        let a = ProfileRoute::new("GB-COH:X6999001", Some("2021-06-30".into()));
        let b = ProfileRoute::new("director:jane-smith", Some("2021-06-30".into()));
        assert_ne!(a, b, "the fixture must be two different screens");

        // Both fetches are in flight; the analyst is back on A by the time either lands.
        let current = a.clone();
        let mut shown: Option<ProfileRoute> = None;
        for landed in [&a, &b] {
            if response_is_still_wanted(landed, &current) {
                shown = Some(landed.clone());
            }
        }
        assert_eq!(shown.as_ref(), Some(&a), "B's response must be dropped");

        // The same date window on a different entity is a different screen, and so is the same
        // entity on a different window — both halves are compared, not just the id.
        let a_undated = ProfileRoute::new("GB-COH:X6999001", None);
        assert!(!response_is_still_wanted(&a_undated, &a));
        // Non-vacuity: a response for the screen you are actually on is accepted.
        assert!(response_is_still_wanted(&a, &a));
    }

    /// An empty `as_at` query value is the same state as no query value at all — a bare
    /// `?as_at=` in a pasted URL must not make the screen a different one from `/entity/{id}`,
    /// because the effect above treats them as different routes and would refetch forever.
    #[test]
    fn an_empty_date_window_is_the_absent_one() {
        assert_eq!(
            ProfileRoute::new("GB-COH:X6999001", Some(String::new())),
            ProfileRoute::new("GB-COH:X6999001", None)
        );
    }
}
