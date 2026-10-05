//! `Breadcrumb` — the only navigation affordance besides the Home cards (D-08, D-09), rendered at
//! the top of every page except Home. See the phase 43 UI spec's "Breadcrumb" section.

use leptos::prelude::*;
use leptos_router::hooks::use_location;

/// Fixed segment list for the 5 known non-Home paths this phase defines. A leading `Home` segment
/// (always a link to `/`) is followed by zero or more link segments and one final, non-linked
/// current-page segment. `/analysis`'s `#<probe-name>` hash suffix does not add a segment
/// (D-07/D-08): the expanded probe is indicated by scroll position and open-accordion state, not
/// the breadcrumb, so the hash is stripped before matching.
pub fn breadcrumb_segments(path: &str) -> Vec<(&'static str, Option<&'static str>)> {
    let path = path.split('#').next().unwrap_or(path);
    match path {
        "/analysis" => vec![("Home", Some("/")), ("Analysis", None)],
        "/graph" => vec![("Home", Some("/")), ("Graph", None)],
        "/entity" => vec![("Home", Some("/")), ("Entity", None)],
        "/help" => vec![("Home", Some("/")), ("Help", None)],
        // PIV-05's saved investigations. The list is a shell destination (there is a header link
        // to it on every page), and one investigation hangs off it — so a reopened investigation
        // reads `Home / Investigations / {name}`, with the name supplied through `trailing`
        // exactly as the entity profile's is.
        "/investigations" => vec![("Home", Some("/")), ("Investigations", None)],
        "/help/glossary" => vec![
            ("Home", Some("/")),
            ("Help", Some("/help")),
            ("Glossary", None),
        ],
        // `/entity/{id}` — the phase 45 UI spec fixes this page's breadcrumb as `Home / Entity / {name}`.
        // The entity's name is not a fixed segment (it is whatever the profile loaded), so this
        // arm supplies the two segments that ARE fixed and the profile page passes the name in
        // through `Breadcrumb`'s `trailing` prop. A match on the prefix rather than on the whole
        // path, because every id is a different path.
        _ if path.starts_with("/investigations/") => {
            vec![
                ("Home", Some("/")),
                ("Investigations", Some("/investigations")),
            ]
        }
        _ if path.starts_with("/entity/") => {
            vec![("Home", Some("/")), ("Entity", Some("/entity"))]
        }
        // `/finding/{probe}` — plan 50-07's detail route. The phase 50 UI spec fixes this page's
        // breadcrumb as `Home / Analysis / {probe display name}`: the probe's own display name is
        // not a fixed segment (it is whatever the report resolved), so this arm supplies the two
        // segments that ARE fixed and `FindingDetailPage` passes the probe name in through
        // `Breadcrumb`'s `trailing` prop, exactly as the entity profile's `{name}` does above.
        _ if path.starts_with("/finding/") => {
            vec![("Home", Some("/")), ("Analysis", Some("/analysis"))]
        }
        // `/alert/{id}` — plan 46-10's detail route. The phase 46 UI spec fixes this page's breadcrumb
        // as `Home / Alerts / {detector display name}`, mirroring `/finding/{probe}`'s own
        // pattern above: the detector display name is not a fixed segment (it is whatever the
        // fetched `AlertDetailView` resolved), so this arm supplies the two segments that ARE
        // fixed and `AlertDetailPage` passes the name in through `Breadcrumb`'s `trailing` prop.
        //
        // Not in plan 46-10's own `files_modified` list — added as a Rule 2/3 deviation
        // (missing critical functionality / blocking): the plan's own truths section requires
        // "the alert detail route mirrors /finding/:probe's shell — breadcrumb, …" and
        // the phase 46 UI spec's detail-route contract names this exact trail, which is unreachable
        // without this match arm (every unmatched path falls through to a bare "Home").
        _ if path.starts_with("/alert/") => {
            vec![("Home", Some("/")), ("Alerts", Some("/alerts"))]
        }
        _ => vec![("Home", Some("/"))],
    }
}

/// Renders the current path's breadcrumb segments — non-final segments as `<A href=...>`
/// (`--muted-foreground`, underline + `--primary` on hover/focus), the final segment as a plain
/// `<span>` (`--foreground`, not a link), separated by a `/` glyph in `--border`. Height 40px,
/// `.text-heading` size (the phase 43 UI spec — was `.text-label` until 2026-09-06, when the operator
/// found 12px too small for the only navigation affordance on an inner page).
///
/// `trailing` is an optional final segment whose text is only known at runtime — the entity
/// profile's `{name}` (the phase 45 UI spec: `Home / Entity / {name}`). Omitted, the breadcrumb is
/// exactly what it was before plan 45-10, which is why every other call site is unchanged. An
/// empty string is treated as absent, so a page that has not loaded its subject yet renders the
/// path it can stand behind rather than a dangling separator.
#[component]
#[allow(non_snake_case, dead_code)]
pub fn Breadcrumb(#[prop(optional, into)] trailing: String) -> impl IntoView {
    let location = use_location();
    let segments = move || {
        let mut segs: Vec<(String, Option<String>)> = breadcrumb_segments(&location.pathname.get())
            .into_iter()
            .map(|(label, href)| (label.to_string(), href.map(str::to_string)))
            .collect();
        if !trailing.is_empty() {
            segs.push((trailing.clone(), None));
        }
        segs
    };

    view! {
        <nav
            class="text-heading"
            style="display: flex; align-items: center; height: 40px; gap: var(--space-xs);"
        >
            {move || {
                let segs = segments();
                let last_index = segs.len().saturating_sub(1);
                segs.into_iter()
                    .enumerate()
                    .map(|(i, (label, href))| {
                        let is_last = i == last_index;
                        let separator = (i > 0)
                            .then(|| view! { <span style="color: var(--border);">"/"</span> });
                        let segment = if is_last {
                            view! {
                                <span style="color: var(--foreground);">{label}</span>
                            }
                                .into_any()
                        } else {
                            view! {
                                <leptos_router::components::A
                                    href=href.unwrap_or_else(|| "/".to_string())
                                    attr:style="color: var(--muted-foreground); text-decoration: none;"
                                >
                                    {label}
                                </leptos_router::components::A>
                            }
                                .into_any()
                        };
                        view! { <span style="display: flex; align-items: center; gap: var(--space-xs);">{separator} {segment}</span> }
                    })
                    .collect_view()
            }}
        </nav>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analysis_breadcrumb_has_two_segments_and_final_is_not_linked() {
        assert_eq!(
            breadcrumb_segments("/analysis"),
            vec![("Home", Some("/")), ("Analysis", None)]
        );
    }

    #[test]
    fn analysis_hash_suffix_does_not_add_a_segment() {
        assert_eq!(
            breadcrumb_segments("/analysis#probe-name"),
            vec![("Home", Some("/")), ("Analysis", None)]
        );
    }

    #[test]
    fn help_glossary_breadcrumb_has_exactly_three_segments() {
        let segments = breadcrumb_segments("/help/glossary");
        assert_eq!(segments.len(), 3);
        assert_eq!(
            segments,
            vec![
                ("Home", Some("/")),
                ("Help", Some("/help")),
                ("Glossary", None),
            ]
        );
    }

    /// `Home / Entity` for the search surface, and `Home / Entity(link)` for a profile — whose
    /// third segment is the entity name, supplied by the page through `trailing` because no fixed
    /// table can hold it.
    #[test]
    fn the_entity_pages_carry_the_spec_s_breadcrumb_trail() {
        assert_eq!(
            breadcrumb_segments("/entity"),
            vec![("Home", Some("/")), ("Entity", None)]
        );
        assert_eq!(
            breadcrumb_segments("/entity/GB-COH%3AX6999001"),
            vec![("Home", Some("/")), ("Entity", Some("/entity"))]
        );
        // The `Entity` segment of a profile IS a link, unlike the search page's own final
        // segment — a reader on a profile can climb back to the search.
        assert_eq!(
            breadcrumb_segments("/entity/director%3Ajane-smith")
                .last()
                .map(|(_, href)| *href),
            Some(Some("/entity"))
        );
    }

    /// `Home / Alerts(link)` for the alert detail route — the alert's own detector display name
    /// is the third segment, supplied through `trailing` exactly as the finding detail route's is.
    #[test]
    fn alert_detail_breadcrumb_has_the_two_fixed_segments_alerts_linked() {
        assert_eq!(
            breadcrumb_segments("/alert/alert%3Aabc123"),
            vec![("Home", Some("/")), ("Alerts", Some("/alerts"))]
        );
        assert_eq!(
            breadcrumb_segments("/alert/alert%3Aabc123")
                .last()
                .map(|(_, href)| *href),
            Some(Some("/alerts"))
        );
    }

    #[test]
    fn graph_breadcrumb_has_two_segments() {
        assert_eq!(
            breadcrumb_segments("/graph"),
            vec![("Home", Some("/")), ("Graph", None)]
        );
    }

    /// `Home / Analysis(link)` for the finding detail route — the probe's own display name is the
    /// third segment, supplied through `trailing` exactly as the entity profile's `{name}` is.
    /// `Analysis` IS a link here (mirrors the entity profile's own linked penultimate segment): a
    /// reader on a finding's detail route can climb back to the board it came from.
    #[test]
    fn finding_detail_breadcrumb_has_the_two_fixed_segments_analysis_linked() {
        assert_eq!(
            breadcrumb_segments("/finding/shared-exact-address"),
            vec![("Home", Some("/")), ("Analysis", Some("/analysis"))]
        );
        assert_eq!(
            breadcrumb_segments("/finding/shared-exact-address")
                .last()
                .map(|(_, href)| *href),
            Some(Some("/analysis"))
        );
    }
}
