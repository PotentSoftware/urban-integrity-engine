//! `/help/algorithms` and `/help/algorithms/:slug` (spec §5): five fixed blocks per algorithm.
//! Parameters read `workbench_dto::caps`; the worked example reads
//! `workbench_dto::planted_examples`, which a native test pins to the planted manifest.
//!
//! Nothing here retypes a number. Defaults and ceilings come from `caps`, the worked examples come
//! from `planted_examples`, and [`tests::help_defaults_match_the_rails_own_defaults`] compares
//! every stated default against what `tool_rail::algorithm_defaults` actually puts in the field —
//! so help that describes a different tool than the one on screen fails the build. That test earns
//! its keep immediately: `ward_subgraph`'s hop default is 2, not the shared `DEFAULT_HOP_CAP` of
//! 4, and a hand-written help table would have said 4.

use leptos::prelude::*;
use leptos_router::hooks::use_params_map;
use urban_integrity_workbench_dto::caps::*;
use urban_integrity_workbench_dto::planted_examples::PLANTED_EXAMPLES;

use crate::breadcrumb::Breadcrumb;

/// Which `AlgorithmRequest` field a row of the parameter table describes. Typed rather than
/// matched on the display name, so the drift test compares the help against the real request the
/// rail builds instead of against another string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamField {
    HopCap,
    ResultCap,
    BudgetSecs,
    K,
    TopN,
    MinCommunitySize,
    Seed,
    Resolution,
}

pub struct ParamHelp {
    /// Read only by [`tests::help_defaults_match_the_rails_own_defaults`], which is the point:
    /// it lets that test compare each documented default against the corresponding field of the
    /// request `tool_rail::algorithm_defaults` actually builds, instead of matching on the
    /// display name. Without it the test degrades to string comparison and stops catching the
    /// case it was written for (`ward_subgraph`'s hop default differing from `DEFAULT_HOP_CAP`).
    /// Justified `allow` per the project's clippy rule (docs/DEVELOPMENT.md) rather than deleted — the alternative is a
    /// weaker test, not less code.
    #[cfg_attr(not(test), allow(dead_code))]
    pub field: ParamField,
    pub name: &'static str,
    pub default: String,
    pub ceiling: String,
    pub raise: &'static str,
    pub lower: &'static str,
}

pub struct AlgorithmHelp {
    pub slug: &'static str,
    pub title: &'static str,
    pub question: &'static str,
    pub params: Vec<ParamHelp>,
    pub when_it_lies: &'static [&'static str],
}

/// A parameter with a real ceiling the server clamps to.
fn p(
    field: ParamField,
    name: &'static str,
    default: impl ToString,
    ceiling: impl ToString,
    raise: &'static str,
    lower: &'static str,
) -> ParamHelp {
    ParamHelp {
        field,
        name,
        default: default.to_string(),
        ceiling: ceiling.to_string(),
        raise,
        lower,
    }
}

/// A tuning parameter with no ceiling — the table says so in words rather than printing a number
/// the clamp does not enforce.
fn p_uncapped(
    field: ParamField,
    name: &'static str,
    default: impl ToString,
    raise: &'static str,
    lower: &'static str,
) -> ParamHelp {
    p(field, name, default, "none", raise, lower)
}

pub fn algorithm_help(slug: &str) -> Option<AlgorithmHelp> {
    let help = match slug {
        "shortest_paths" => AlgorithmHelp {
            slug: "shortest_paths",
            title: "Shortest / k-shortest paths",
            question: "What is the fewest-hop chain connecting these two, and what are the next-shortest alternatives?",
            params: vec![p(
                ParamField::K,
                "k",
                DEFAULT_K_SHORTEST,
                CEILING_K_SHORTEST,
                "more alternative routes, longer ones included",
                "only the tightest route",
            )],
            when_it_lies: &[
                "Every hop counts one, so a shared registered office weighs the same as a directorship.",
                "With IssuedBy or Represents included, almost any two contracts are two hops apart through the council or the ward; leave them unchecked unless that is the question.",
            ],
        },
        "all_simple_paths" => AlgorithmHelp {
            slug: "all_simple_paths",
            title: "All simple paths",
            question: "Every distinct route between these two that visits no entity twice, up to a length.",
            params: vec![
                p(
                    ParamField::HopCap,
                    "Hop cap",
                    DEFAULT_HOP_CAP,
                    CEILING_HOP_CAP,
                    "longer chains, count grows fast",
                    "only short chains",
                ),
                p(
                    ParamField::ResultCap,
                    "Result cap",
                    DEFAULT_RESULT_CAP,
                    CEILING_RESULT_CAP,
                    "more routes listed",
                    "fewer routes listed; truncation reported",
                ),
                p(
                    ParamField::BudgetSecs,
                    "Wall-clock budget",
                    DEFAULT_BUDGET_SECS,
                    CEILING_BUDGET_SECS,
                    "search runs longer before stopping",
                    "stops sooner; truncation reported",
                ),
            ],
            when_it_lies: &[
                "A truncated result is a sample of routes, not the set. Read the status line.",
                "Dense hubs multiply routes without adding meaning; exclude them.",
            ],
        },
        "community" => AlgorithmHelp {
            slug: "community",
            title: "Community detection",
            question: "Which entities hang together more than they hang with the rest?",
            params: vec![
                p_uncapped(
                    ParamField::Seed,
                    "Seed",
                    DEFAULT_SEED,
                    "different tie-breaks, same graph",
                    "different tie-breaks, same graph",
                ),
                p(
                    ParamField::MinCommunitySize,
                    "Minimum community size",
                    DEFAULT_MIN_COMMUNITY_SIZE,
                    CEILING_MIN_COMMUNITY_SIZE,
                    "small clusters hidden",
                    "small clusters shown",
                ),
                p_uncapped(
                    ParamField::Resolution,
                    "Resolution",
                    DEFAULT_RESOLUTION,
                    "more, smaller communities",
                    "fewer, larger communities",
                ),
            ],
            when_it_lies: &[
                "On this corpus communities follow topology, not fraud: a shared address or a council buyer pulls hundreds of unrelated companies together.",
                "Seeded means reproducible, not right. Two seeds can split a bridge node differently.",
            ],
        },
        "k_core" => AlgorithmHelp {
            slug: "k_core",
            title: "k-core",
            question: "Which entities each have at least k connections to others that also have at least k?",
            params: vec![p(
                ParamField::K,
                "k",
                DEFAULT_K_CORE,
                CEILING_K_CORE,
                "smaller, denser core",
                "larger, looser core",
            )],
            when_it_lies: &[
                "Dense cores on this corpus are large firms with many directors, not rings.",
            ],
        },
        "betweenness" => AlgorithmHelp {
            slug: "betweenness",
            title: "Betweenness centrality",
            question: "Which entities sit on the most shortest paths between others, the brokers and bridges?",
            params: vec![
                p(
                    ParamField::TopN,
                    "Top N",
                    DEFAULT_TOP_N,
                    CEILING_TOP_N,
                    "longer ranked list",
                    "shorter ranked list",
                ),
                p_uncapped(
                    ParamField::Seed,
                    "Seed",
                    DEFAULT_SEED,
                    "different sample of source nodes",
                    "different sample of source nodes",
                ),
            ],
            when_it_lies: &[
                "Over the whole population the top rows are the council buyer and the biggest registered offices.",
                "Above the pivot count the score is sampled; the figure moves with the seed. It is exact only on small graphs.",
            ],
        },
        "ward_subgraph" => AlgorithmHelp {
            slug: "ward_subgraph",
            title: "Ward-reachable subgraph",
            question: "Which contracts does this ward represent?",
            params: vec![p(
                ParamField::HopCap,
                "Hops",
                DEFAULT_WARD_SUBGRAPH_HOPS,
                CEILING_HOP_CAP,
                "no effect beyond one hop today",
                "no effect below one hop",
            )],
            when_it_lies: &[
                "It walks Represents only, so it returns the ward and its contracts and nothing further. Use paths from a contract to go deeper.",
            ],
        },
        _ => return None,
    };
    Some(help)
}

pub const ALGORITHM_SLUGS: [&str; 6] = [
    "shortest_paths",
    "all_simple_paths",
    "community",
    "k_core",
    "betweenness",
    "ward_subgraph",
];

const VALIDATE_STEPS: [&str; 3] = [
    "Run scripts/toy-analysis.sh from the repository root, then start the workbench with the printed --analysis slug.",
    "Open the Graph page and set the parameters listed under Worked example.",
    "Press Run and compare the results list with the expected rows. Every figure comes from the planted graph's manifest.",
];

#[component]
#[allow(non_snake_case)]
pub fn AlgorithmsIndexPage() -> impl IntoView {
    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-lg); padding-top: var(--space-md);">
            <Breadcrumb />
            <h1 class="text-heading">"Graph algorithms"</h1>
            <p class="text-body" style="margin: 0;">
                "Every page states what the algorithm answers, what its parameters do, where it misleads, and a worked example you can reproduce on the planted fixture."
            </p>
            <ul style="display: flex; flex-direction: column; gap: var(--space-sm); margin: 0; padding-left: var(--space-lg);">
                {ALGORITHM_SLUGS.iter().map(|slug| {
                    let help = algorithm_help(slug).expect("known slug");
                    view! {
                        <li>
                            <a class="text-body" href=format!("/help/algorithms/{slug}")>{help.title}</a>
                        </li>
                    }
                }).collect_view()}
            </ul>
        </div>
    }
}

#[component]
#[allow(non_snake_case)]
pub fn AlgorithmHelpPage() -> impl IntoView {
    let params = use_params_map();
    let slug = move || params.read().get("slug").unwrap_or_default();
    view! {
        <div style="display: flex; flex-direction: column; gap: var(--space-lg); padding-top: var(--space-md);">
            <Breadcrumb />
            {move || match algorithm_help(&slug()) {
                None => view! {
                    <p class="text-body">"No such algorithm."</p>
                    <a class="text-body" href="/help/algorithms">"Back to the algorithm list"</a>
                }.into_any(),
                Some(help) => {
                    let example = PLANTED_EXAMPLES.iter().find(|e| e.algorithm == help.slug);
                    view! {
                        <h1 class="text-heading">{help.title}</h1>
                        <section style="display: flex; flex-direction: column; gap: var(--space-xs);">
                            <h2 class="text-display" style="margin: 0;">"Question it answers"</h2>
                            <p class="text-body" style="margin: 0;">{help.question}</p>
                        </section>
                        <section style="display: flex; flex-direction: column; gap: var(--space-xs);">
                            <h2 class="text-display" style="margin: 0;">"Parameters"</h2>
                            <div style="overflow-x: auto;">
                                <table class="text-body">
                                    <thead>
                                        <tr>
                                            <th>"Name"</th>
                                            <th>"Default"</th>
                                            <th>"Ceiling"</th>
                                            <th>"Raising it"</th>
                                            <th>"Lowering it"</th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {help.params.into_iter().map(|param| view! {
                                            <tr>
                                                <td>{param.name}</td>
                                                <td class="font-mono">{param.default}</td>
                                                <td class="font-mono">{param.ceiling}</td>
                                                <td>{param.raise}</td>
                                                <td>{param.lower}</td>
                                            </tr>
                                        }).collect_view()}
                                    </tbody>
                                </table>
                            </div>
                        </section>
                        <section style="display: flex; flex-direction: column; gap: var(--space-xs);">
                            <h2 class="text-display" style="margin: 0;">"Worked example on the planted graph"</h2>
                            {example.map(|e| view! {
                                <p class="text-body" style="margin: 0;">"Set:"</p>
                                <ul style="margin: 0;">
                                    {e.setup.iter().map(|s| view! { <li class="text-body">{*s}</li> }).collect_view()}
                                </ul>
                                <p class="text-body" style="margin: 0;">"Expect:"</p>
                                <ul style="margin: 0;">
                                    {e.expected_rows.iter().map(|s| view! { <li class="text-body">{*s}</li> }).collect_view()}
                                </ul>
                            })}
                        </section>
                        <section style="display: flex; flex-direction: column; gap: var(--space-xs);">
                            <h2 class="text-display" style="margin: 0;">"When it lies"</h2>
                            <ul style="margin: 0;">
                                {help.when_it_lies.iter().map(|s| view! { <li class="text-body">{*s}</li> }).collect_view()}
                            </ul>
                        </section>
                        <section style="display: flex; flex-direction: column; gap: var(--space-xs);">
                            <h2 class="text-display" style="margin: 0;">"Validate it yourself"</h2>
                            <ol style="margin: 0;">
                                {VALIDATE_STEPS.iter().map(|s| view! { <li class="text-body">{*s}</li> }).collect_view()}
                            </ol>
                        </section>
                    }.into_any()
                }
            }}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_rail::algorithm_defaults;

    #[test]
    fn every_slug_has_help_and_a_worked_example() {
        for slug in ALGORITHM_SLUGS {
            let help = algorithm_help(slug).expect(slug);
            assert_eq!(help.slug, slug);
            assert!(
                PLANTED_EXAMPLES.iter().any(|e| e.algorithm == slug),
                "no example for {slug}"
            );
            assert!(
                !help.when_it_lies.is_empty(),
                "{slug} claims no failure mode"
            );
            assert!(!help.params.is_empty(), "{slug} documents no parameters");
        }
        assert!(algorithm_help("nope").is_none());
    }

    /// The help's own slug list and the rail's algorithm list must be the same set — a rail
    /// algorithm with no help page is the gap this whole plan exists to close, and a help page
    /// for an algorithm the rail does not offer is a dead link from the index.
    #[test]
    fn the_slug_list_matches_the_worked_examples() {
        let mut from_help: Vec<&str> = ALGORITHM_SLUGS.to_vec();
        let mut from_examples: Vec<&str> = PLANTED_EXAMPLES.iter().map(|e| e.algorithm).collect();
        from_help.sort_unstable();
        from_examples.sort_unstable();
        assert_eq!(from_help, from_examples);
    }

    /// PROVE THIS CAN FAIL: state `DEFAULT_HOP_CAP` (4) as `ward_subgraph`'s hop default instead
    /// of `DEFAULT_WARD_SUBGRAPH_HOPS` (2) — as the plan's own draft did — and this fails. The
    /// help must describe the request the rail actually builds, not a plausible constant with a
    /// similar name.
    #[test]
    fn help_defaults_match_the_rails_own_defaults() {
        for slug in ALGORITHM_SLUGS {
            let help = algorithm_help(slug).expect(slug);
            let request = algorithm_defaults(slug);
            for param in &help.params {
                let actual = match param.field {
                    ParamField::HopCap => request.hop_cap.to_string(),
                    ParamField::ResultCap => request.result_cap.to_string(),
                    ParamField::BudgetSecs => request.budget_secs.to_string(),
                    ParamField::K => request.k.to_string(),
                    ParamField::TopN => request.top_n.to_string(),
                    ParamField::MinCommunitySize => request.min_community_size.to_string(),
                    ParamField::Seed => request.seed.to_string(),
                    ParamField::Resolution => request.resolution.to_string(),
                };
                assert_eq!(
                    param.default, actual,
                    "{slug}: help says {} is {}, the rail sends {actual}",
                    param.name, param.default
                );
            }
        }
    }

    #[test]
    fn parameter_ceilings_come_from_caps() {
        let help = algorithm_help("all_simple_paths").unwrap();
        assert_eq!(help.params[0].default, DEFAULT_HOP_CAP.to_string());
        assert_eq!(help.params[0].ceiling, CEILING_HOP_CAP.to_string());

        // An uncapped tuning parameter must say so in words, never print a number the clamp does
        // not enforce.
        let community = algorithm_help("community").unwrap();
        let resolution = community
            .params
            .iter()
            .find(|p| p.field == ParamField::Resolution)
            .unwrap();
        assert_eq!(resolution.ceiling, "none");
    }
}
