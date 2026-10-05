//! `/graph?from=&to=&ids=&algorithm=` (spec §6.2). Parsed once on mount; never written back.
//!
//! Pure functions only — no signals, no fetches — so the whole link grammar is unit-testable
//! without a browser. The Graph page calls [`plan`] once and acts on the result; nothing here
//! knows what a canvas is.

/// What a deep link asks the Graph page to do, decided purely by how many distinct entities the
/// link names (plus an optional algorithm override).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepLinkAction {
    None,
    SelectOne(String),
    ShortestPath {
        from: String,
        to: String,
    },
    HighlightSet(Vec<String>),
    /// `algorithm=` override with two ids: run that slug with From/To.
    RunWithEndpoints {
        algorithm: String,
        from: String,
        to: String,
    },
}

/// The only algorithms an `algorithm=` parameter may select. A link is untrusted input: anything
/// outside this list falls back to the default two-id behaviour rather than being passed through
/// to the server, so a crafted link cannot drive an arbitrary algorithm with arbitrary caps.
const OVERRIDABLE: [&str; 2] = ["all_simple_paths", "shortest_paths"];

/// Collapses `from`, `to` and the `ids` CSV into one ordered, de-duplicated list of entity ids.
///
/// De-duplication preserves first-seen order and removes ALL repeats, not just adjacent ones —
/// `ids=a,b,a` names two distinct entities, so it should behave like any other two-entity link
/// rather than highlighting a set with a repeat in it.
fn collect_ids(from: Option<&str>, to: Option<&str>, ids: Option<&str>) -> Vec<String> {
    let mut list: Vec<String> = Vec::new();
    let mut push = |value: &str| {
        let value = value.trim();
        if !value.is_empty() && !list.iter().any(|existing| existing == value) {
            list.push(value.to_string());
        }
    };
    for value in [from, to].into_iter().flatten() {
        push(value);
    }
    if let Some(csv) = ids {
        for value in csv.split(',') {
            push(value);
        }
    }
    list
}

pub fn plan(
    from: Option<&str>,
    to: Option<&str>,
    ids: Option<&str>,
    algorithm: Option<&str>,
) -> DeepLinkAction {
    let mut list = collect_ids(from, to, ids);
    match list.len() {
        0 => DeepLinkAction::None,
        1 => DeepLinkAction::SelectOne(list.remove(0)),
        2 => {
            let to = list.remove(1);
            let from = list.remove(0);
            match algorithm.filter(|a| OVERRIDABLE.contains(a)) {
                Some(a) if a != "shortest_paths" => DeepLinkAction::RunWithEndpoints {
                    algorithm: a.to_string(),
                    from,
                    to,
                },
                _ => DeepLinkAction::ShortestPath { from, to },
            }
        }
        _ => DeepLinkAction::HighlightSet(list),
    }
}

/// `CompanyNumber` refs map to `GB-COH:{n}`; `Entity` refs are node ids already; every other kind
/// (an `IdentityLink` row, say) is a database record rather than a graph node and produces
/// nothing — a link must not invent a node id for something the canvas has never heard of.
pub fn node_id_for_ref(kind: &str, id: &str) -> Option<String> {
    match kind {
        "Entity" => Some(id.to_string()),
        "CompanyNumber" => Some(format!("GB-COH:{id}")),
        _ => None,
    }
}

/// Percent-encodes one id for the `ids=` query value.
///
/// The wasm build uses the browser's own `encodeURIComponent`; the host build (unit tests) has no
/// `js_sys`, so it encodes exactly the two characters that matter for this query value: `:`,
/// which every entity id contains, and `,`, which is the separator itself. Anything else in an
/// entity id is already query-safe.
fn encode(value: &str) -> String {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::encode_uri_component(value)
            .as_string()
            .unwrap_or_default()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        value.replace(':', "%3A").replace(',', "%2C")
    }
}

/// The `/graph?…` href for an evidence row, or `None` when no ref resolves to a node id — a row
/// whose only refs are database records gets no link at all rather than a link to an empty canvas.
pub fn graph_href_for_refs(refs: &[(String, String)]) -> Option<String> {
    let ids: Vec<String> = refs
        .iter()
        .filter_map(|(kind, id)| node_id_for_ref(kind, id))
        .collect();
    if ids.is_empty() {
        return None;
    }
    let encoded: Vec<String> = ids.iter().map(|id| encode(id)).collect();
    Some(format!("/graph?ids={}", encoded.join(",")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_kinds_map() {
        assert_eq!(
            node_id_for_ref("Entity", "director:x"),
            Some("director:x".into())
        );
        assert_eq!(
            node_id_for_ref("CompanyNumber", "12345678"),
            Some("GB-COH:12345678".into())
        );
        assert_eq!(node_id_for_ref("IdentityLink", "identity_links:1"), None);
    }

    #[test]
    fn href_skips_identity_links_and_maps_company_numbers() {
        let refs = vec![
            ("IdentityLink".to_string(), "identity_links:1".to_string()),
            ("CompanyNumber".to_string(), "12345678".to_string()),
            ("Entity".to_string(), "director:x".to_string()),
        ];
        assert_eq!(
            graph_href_for_refs(&refs),
            Some("/graph?ids=GB-COH%3A12345678,director%3Ax".into())
        );
        assert_eq!(
            graph_href_for_refs(&[("IdentityLink".to_string(), "x".to_string())]),
            None
        );
    }

    /// A row with no refs at all is the aggregate-evidence case — it gets no link, not a link to
    /// an empty canvas.
    #[test]
    fn a_row_with_no_refs_has_no_href() {
        assert_eq!(graph_href_for_refs(&[]), None);
    }

    /// The href this builds must round-trip through the link grammar that reads it back — the two
    /// halves are written in the same module precisely so they cannot drift apart.
    #[test]
    fn the_href_it_builds_parses_back_into_the_same_ids() {
        let refs = vec![
            ("Entity".to_string(), "director:a".to_string()),
            ("Entity".to_string(), "director:b".to_string()),
            ("CompanyNumber".to_string(), "99".to_string()),
        ];
        let href = graph_href_for_refs(&refs).expect("three resolvable refs");
        let raw = href
            .strip_prefix("/graph?ids=")
            .expect("expected ids query");
        let decoded = raw.replace("%3A", ":");
        assert_eq!(
            plan(None, None, Some(&decoded), None),
            DeepLinkAction::HighlightSet(vec![
                "director:a".into(),
                "director:b".into(),
                "GB-COH:99".into(),
            ])
        );
    }

    #[test]
    fn one_id_selects() {
        assert_eq!(
            plan(Some("a"), None, None, None),
            DeepLinkAction::SelectOne("a".into())
        );
        assert_eq!(
            plan(None, None, Some("a"), None),
            DeepLinkAction::SelectOne("a".into())
        );
    }

    #[test]
    fn two_ids_run_shortest_path_by_default() {
        assert_eq!(
            plan(Some("a"), Some("b"), None, None),
            DeepLinkAction::ShortestPath {
                from: "a".into(),
                to: "b".into()
            }
        );
        assert_eq!(
            plan(None, None, Some("a,b"), Some("nope")),
            DeepLinkAction::ShortestPath {
                from: "a".into(),
                to: "b".into()
            }
        );
    }

    #[test]
    fn two_ids_with_override_run_that_algorithm() {
        assert_eq!(
            plan(Some("a"), Some("b"), None, Some("all_simple_paths")),
            DeepLinkAction::RunWithEndpoints {
                algorithm: "all_simple_paths".into(),
                from: "a".into(),
                to: "b".into()
            }
        );
    }

    /// PROVE THIS CAN FAIL: drop the `OVERRIDABLE` filter and a crafted link could name any
    /// algorithm identifier — including one the rail deliberately does not offer.
    #[test]
    fn an_unlisted_algorithm_override_is_ignored_not_forwarded() {
        for crafted in [
            "highlight_set",
            "community",
            "k_core",
            "../../etc/passwd",
            "",
        ] {
            assert_eq!(
                plan(Some("a"), Some("b"), None, Some(crafted)),
                DeepLinkAction::ShortestPath {
                    from: "a".into(),
                    to: "b".into()
                },
                "{crafted} must not be forwarded as an algorithm"
            );
        }
    }

    #[test]
    fn three_or_more_highlight() {
        assert_eq!(
            plan(None, None, Some("a, b,c,"), None),
            DeepLinkAction::HighlightSet(vec!["a".into(), "b".into(), "c".into()])
        );
    }

    #[test]
    fn empty_is_none() {
        assert_eq!(plan(None, None, Some(""), None), DeepLinkAction::None);
        assert_eq!(plan(None, None, None, None), DeepLinkAction::None);
        assert_eq!(
            plan(Some("   "), None, Some(" , ,"), None),
            DeepLinkAction::None
        );
    }

    /// Repeats collapse wherever they appear, not just when adjacent: naming the same entity
    /// twice does not make a link into a three-entity highlight set.
    #[test]
    fn repeated_ids_collapse_regardless_of_position() {
        assert_eq!(
            plan(None, None, Some("a,b,a"), None),
            DeepLinkAction::ShortestPath {
                from: "a".into(),
                to: "b".into()
            }
        );
        assert_eq!(
            plan(Some("a"), Some("a"), None, None),
            DeepLinkAction::SelectOne("a".into())
        );
        assert_eq!(
            plan(None, None, Some("a,b,c,b,a"), None),
            DeepLinkAction::HighlightSet(vec!["a".into(), "b".into(), "c".into()])
        );
    }

    /// `from`/`to` are read before `ids`, so a link carrying both keeps the endpoints in the
    /// order the caller wrote them — which is what makes From/To meaningful.
    #[test]
    fn from_and_to_lead_the_list() {
        assert_eq!(
            plan(Some("x"), Some("y"), Some("z"), None),
            DeepLinkAction::HighlightSet(vec!["x".into(), "y".into(), "z".into()])
        );
    }
}
