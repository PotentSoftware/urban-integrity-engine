use anyhow::{bail, Context as _};

use crate::output::{print_json, OutputFormat};
use urban_integrity_core::{bfs_traversal, config::AppConfig, resolve_entity, PathStep, MAX_HOPS};

/// Arguments for the `graph-path` subcommand.
#[derive(Debug, clap::Args)]
#[command(
    about = "Find the shortest path between two entities in the HIN graph",
    long_about = "Find the shortest path between two entities in the HIN graph.\n\n\
                  EXAMPLES:\n\n  \
                  # Trace a company to a councillor\n  \
                  urban-integrity graph-path \"Apex Build Ltd\" \"John Smith\"\n\n  \
                  # Use slugs for exact disambiguation\n  \
                  urban-integrity graph-path GB-COH:12345678 director:john-smith\n\n  \
                  # JSON output\n  \
                  urban-integrity graph-path \"Apex Build Ltd\" \"John Smith\" --output json"
)]
pub struct GraphPathArgs {
    /// Starting entity (name or slug)
    pub from: String,
    /// Target entity (name or slug)
    pub to: String,
    /// Output format
    #[arg(long, value_enum)]
    pub output: Option<OutputFormat>,
}

/// JSON-serialisable path node.
#[derive(Debug, serde::Serialize)]
struct PathNode {
    id: String,
    name: String,
}

/// JSON-serialisable path edge.
#[derive(Debug, serde::Serialize)]
struct PathEdge {
    #[serde(rename = "type")]
    edge_type: String,
}

/// JSON representation of a found path.
#[derive(Debug, serde::Serialize)]
struct PathResult {
    nodes: Vec<PathNode>,
    edges: Vec<PathEdge>,
}

pub async fn run(args: GraphPathArgs) -> anyhow::Result<()> {
    let fmt = args.output.unwrap_or_else(OutputFormat::auto);
    let config = AppConfig::load().context("Config load failed")?;
    let db = crate::db::connect_db(&config).await?;

    // Resolve both endpoints via the canonical seam (D-09).
    let src_ids = resolve_entity(&db, &args.from)
        .await
        .context("resolve_entity (from) failed")?;
    let dst_ids = resolve_entity(&db, &args.to)
        .await
        .context("resolve_entity (to) failed")?;

    if src_ids.is_empty() {
        bail!("Entity not found: '{}'", args.from);
    }
    if dst_ids.is_empty() {
        bail!("Entity not found: '{}'", args.to);
    }

    // Disambiguation: multiple matches → list candidates and bail.
    if src_ids.len() > 1 {
        eprintln!(
            "Multiple entities match '{}' — re-run with a slug/CH-number:",
            args.from
        );
        for id in &src_ids {
            eprintln!("  {id}");
        }
        bail!("Ambiguous source entity — use a slug or CH number for exact lookup");
    }
    if dst_ids.len() > 1 {
        eprintln!(
            "Multiple entities match '{}' — re-run with a slug/CH-number:",
            args.to
        );
        for id in &dst_ids {
            eprintln!("  {id}");
        }
        bail!("Ambiguous target entity — use a slug or CH number for exact lookup");
    }

    let src = src_ids.into_iter().next().unwrap();
    let dst = dst_ids.into_iter().next().unwrap();

    // Short-circuit: same entity (D-04 / REVIEWS short-circuit).
    if src == dst {
        println!(
            "No path needed: '{}' and '{}' resolve to the same entity",
            args.from, args.to
        );
        return Ok(());
    }

    // ── BFS via shared traversal library (D-05/D-06) ─────────────────────────
    let path_steps = bfs_traversal(&db, &src, &dst, MAX_HOPS)
        .await
        .context("BFS traversal failed")?;

    if path_steps.is_empty() {
        println!("No path found between {} and {}", args.from, args.to);
        return Ok(());
    }

    match fmt {
        OutputFormat::Json => {
            let result = build_path_json(&path_steps);
            print_json(&result)?;
        }
        OutputFormat::Table => {
            let rendered = render_path(&path_steps);
            println!("{rendered}");
        }
        OutputFormat::Markdown | OutputFormat::Pdf => {
            bail!("--format md/pdf is only supported by the report command");
        }
    }

    Ok(())
}

/// Render a path as the human-readable string:
/// `"Name → EDGETYPE → Name → EDGETYPE → Name"`.
///
/// Display names come from `PathStep.node_label`.
/// Edge type is already uppercased by `build_path_steps`.
pub(crate) fn render_path(steps: &[PathStep]) -> String {
    if steps.is_empty() {
        return String::new();
    }
    if steps.len() == 1 {
        return steps[0].node_label.clone();
    }

    let mut parts: Vec<String> = Vec::new();
    for step in steps {
        // edge_type is the incoming edge (arrives at this step), so it precedes the label
        if let Some(ref et) = step.edge_type {
            parts.push(format!("\u{2192} {et} \u{2192}"));
        }
        parts.push(step.node_label.clone());
    }

    parts.join(" ")
}

/// Build a JSON-serialisable path result from `Vec<PathStep>`.
fn build_path_json(steps: &[PathStep]) -> PathResult {
    let nodes: Vec<PathNode> = steps
        .iter()
        .map(|s| PathNode {
            id: s.node_id.clone(),
            name: s.node_label.clone(),
        })
        .collect();

    let edges: Vec<PathEdge> = steps
        .iter()
        .filter_map(|s| {
            s.edge_type.as_ref().map(|et| PathEdge {
                edge_type: et.clone(),
            })
        })
        .collect();

    PathResult { nodes, edges }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_steps(data: &[(&str, &str, Option<&str>)]) -> Vec<PathStep> {
        data.iter()
            .map(|(id, label, et)| PathStep {
                node_id: id.to_string(),
                node_label: label.to_string(),
                edge_type: et.map(str::to_string),
            })
            .collect()
    }

    // SURF-04: OwnsProperty must render as OWNS_PROPERTY (not OWNSPROPERTY) in graph-path output.
    #[test]
    fn render_path_owns_property_edge() {
        let steps = make_steps(&[
            ("GB-COH:12345678", "Apex Build Ltd", None),
            ("property:12345678_LN1234", "LN1234", Some("OWNS_PROPERTY")),
        ]);
        let rendered = render_path(&steps);
        assert!(
            rendered.contains("OWNS_PROPERTY"),
            "OwnsProperty edge must render as OWNS_PROPERTY, got: {rendered}"
        );
    }

    // render_path_labels: confirm "DIRECTS" uppercased in rendered output.
    #[test]
    fn render_path_labels() {
        let steps = make_steps(&[
            ("node-a", "Company A", None),
            ("node-b", "John Smith", Some("DIRECTS")),
            ("node-c", "Company C", Some("DIRECTS")),
        ]);
        let rendered = render_path(&steps);

        assert!(
            rendered.contains("DIRECTS"),
            "Rendered path must contain uppercased 'DIRECTS', got: {rendered}"
        );
        assert!(
            rendered.contains("Company A"),
            "Rendered path must contain 'Company A', got: {rendered}"
        );
        assert!(
            rendered.contains("John Smith"),
            "Rendered path must contain 'John Smith', got: {rendered}"
        );
        assert!(
            rendered.contains("Company C"),
            "Rendered path must contain 'Company C', got: {rendered}"
        );
        // Confirm the → arrow separator is present
        assert!(
            rendered.contains('\u{2192}'),
            "Rendered path must contain → arrows, got: {rendered}"
        );
    }
}
