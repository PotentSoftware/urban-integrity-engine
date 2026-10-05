//! Trunk entry point for the analyst workbench frontend (Phase 42, WBX-01).

mod abbr;
mod alert_detail_page;
mod alerts;
mod analysis_page;
mod analysis_picker;
mod app;
mod breadcrumb;
mod canvas_filters;
mod copy_button;
mod cosmograph_spike;
mod cosmograph_trait;
mod disambiguation;
mod edge_palette;
mod entity_picker;
mod entity_ref_view;
mod finding_board;
mod finding_card;
mod finding_detail_page;
mod glossary;
mod graph_deep_link;
mod graph_page;
mod graph_results_list;
mod graph_status_line;
mod help;
mod home_page;
mod investigation;
mod motif_bars;
mod motif_path;
mod motif_star;
mod motif_typed_edge;
mod named_entity_chips;
mod neighbourhood;
mod node_palette;
mod profile;
mod side_card;
mod theme;
mod timestamp;
mod tool_rail;
mod verdict_badge;

use app::App;

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}
