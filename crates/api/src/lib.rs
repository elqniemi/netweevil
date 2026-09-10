//! Local Axum HTTP API exposing datasets, profiles, routing analyses, GTFS
//! transit queries, the GTFS scenario editor, the workspace setup flow, and
//! simulation control to local tools, the web console and the QGIS plugin.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use netweevil_persist::{WorkspacePaths, read_workspace_config, write_workspace_config};
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

mod console;
mod dto;
mod dynamic_profiles;
mod error;
mod geojson;
mod gtfs_editor;
mod handlers;
mod jobs;
mod locate;
mod network;
mod simulation;
mod state;
mod station_geometry;
mod terrain;
mod trace_matching;
mod transit;
mod transit_directions;
mod waypoints;
mod workspace;

use simulation::SimulationRegistry;
use state::{ApiState, Workspace, load_service_runtime};

pub use console::embedded_console_available;
pub use state::ApiServeOptions;

/// Where the built web console is looked for when `--console-dir` is not
/// given and none is embedded in the binary.
pub fn discover_console_dir(workspace_root: &Path) -> Option<PathBuf> {
    let mut candidates = vec![workspace_root.join("frontend").join("dist")];
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("console"));
        candidates.push(dir.join("..").join("..").join("frontend").join("dist"));
    }
    candidates
        .into_iter()
        .find(|dir| dir.join("index.html").is_file())
}

pub async fn serve(paths: WorkspacePaths, options: ApiServeOptions) -> Result<()> {
    let explicit = options.explicit_selection();
    let selection = match explicit.clone() {
        Some(selection) => Some(selection),
        None => read_workspace_config(&paths).context("reading .netweevil/workspace.json")?,
    };
    let runtime = match selection.as_ref() {
        Some(selection) => Some(load_service_runtime(&paths, selection, |_| {})?),
        None => None,
    };
    if let Some(mut selection) = explicit {
        selection.updated_at = netweevil_manifest::now_rfc3339().ok();
        write_workspace_config(&paths, &selection)?;
    }
    let console_dir = options.console_dir.clone().or_else(|| {
        if embedded_console_available() {
            None
        } else {
            discover_console_dir(&paths.root)
        }
    });

    let state = ApiState {
        workspace: Arc::new(Workspace::new(paths.clone(), runtime)),
        simulations: Arc::new(SimulationRegistry::default()),
    };
    let app = router(state, console_dir.as_deref());

    match selection.as_ref() {
        Some(selection) => info!(
            bind = %options.bind,
            dataset_id = %selection.dataset_id,
            default_profile = %selection.default_profile,
            "starting netweevil api"
        ),
        None => info!(
            bind = %options.bind,
            state_dir = %paths.state_dir.display(),
            "starting netweevil api in setup mode (no dataset loaded yet)"
        ),
    }
    match console_dir.as_ref() {
        Some(dir) => info!(console = %dir.display(), "serving web console at /"),
        None if embedded_console_available() => info!("serving the embedded web console at /"),
        None => warn!(
            "web console not found (build it with `pnpm build` in frontend/ or pass --console-dir); API only"
        ),
    }

    let listener = TcpListener::bind(options.bind)
        .await
        .with_context(|| format!("binding API listener on {}", options.bind))?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("running API server")
}

fn router(state: ApiState, console_dir: Option<&Path>) -> Router {
    let mut router = Router::new()
        .route("/healthz", get(handlers::healthz))
        .route("/readyz", get(handlers::service_info))
        .route("/v1/service", get(handlers::service_info))
        .route("/v1/terrain-sources", get(terrain::terrain_sources_handler))
        .route(
            "/v1/terrain/{source}/{version}/{z}/{x}/{tile}",
            get(terrain::terrain_tile_handler),
        )
        .route("/v1/profiles", get(handlers::list_profiles))
        .route("/v1/profiles/{profile_id}", get(handlers::get_profile))
        .route("/v1/route", post(handlers::route_handler))
        .route("/v1/directions", post(handlers::directions_handler))
        .route("/v1/match", post(trace_matching::trace_match_handler))
        .route("/v1/locate", post(locate::locate_handler))
        .route("/v1/waypoints", post(waypoints::waypoints_handler))
        .route("/v1/transit-route", post(transit::transit_route_handler))
        .route(
            "/v1/transit-directions",
            post(transit_directions::transit_directions_handler),
        )
        .route(
            "/v1/transit-feeds/{feed_id}/station-geometry",
            post(station_geometry::station_geometry_handler),
        )
        .route(
            "/v1/transit-service-area",
            post(transit::transit_service_area_handler),
        )
        .route("/v1/od", post(handlers::od_handler))
        .route("/v1/matrix", post(handlers::matrix_handler))
        .route("/v1/accessibility", post(handlers::accessibility_handler))
        .route("/v1/service-area", post(handlers::service_area_handler))
        .route(
            "/v1/service-area-sequence",
            post(handlers::service_area_sequence_handler),
        )
        .route("/v1/betweenness", post(handlers::betweenness_handler))
        .route("/v1/scenario-batch", post(handlers::scenario_batch_handler))
        .route("/v1/network/edges", post(network::network_edges_handler))
        .route(
            "/v1/simulation",
            get(simulation::list_simulations).post(simulation::create_simulation),
        )
        .route(
            "/v1/simulation/{simulation_id}",
            get(simulation::get_simulation).delete(simulation::delete_simulation),
        )
        .route(
            "/v1/simulation/{simulation_id}/control",
            post(simulation::control_simulation),
        )
        .route(
            "/v1/simulation/{simulation_id}/frames",
            get(simulation::simulation_frames),
        )
        .route(
            "/v1/simulation/{simulation_id}/edges",
            get(simulation::simulation_edges),
        )
        .route(
            "/v1/simulation/{simulation_id}/temporal",
            get(simulation::simulation_temporal),
        )
        .route(
            "/v1/simulation/{simulation_id}/agents/{agent_id}",
            get(simulation::simulation_agent),
        )
        // Workspace setup: where data lives, uploads, imports, profiles, activation.
        .route("/v1/workspace", get(workspace::workspace_info))
        .route("/v1/workspace/jobs", get(workspace::list_jobs))
        .route("/v1/workspace/jobs/{job_id}", get(workspace::get_job))
        .route(
            "/v1/workspace/uploads",
            post(workspace::upload_file).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/v1/workspace/uploads/{name}",
            axum::routing::delete(workspace::delete_upload),
        )
        .route(
            "/v1/workspace/datasets/import",
            post(workspace::import_dataset_handler),
        )
        .route(
            "/v1/workspace/datasets/{dataset_id}",
            axum::routing::delete(workspace::delete_dataset),
        )
        .route(
            "/v1/workspace/profile-templates",
            get(workspace::profile_templates),
        )
        .route("/v1/workspace/profiles", post(workspace::create_profile))
        .route(
            "/v1/workspace/profiles/{name}",
            get(workspace::get_profile_file).delete(workspace::delete_profile_file),
        )
        .route(
            "/v1/workspace/transit/import",
            post(workspace::import_transit_handler),
        )
        .route(
            "/v1/workspace/transit-feeds/{feed_id}",
            axum::routing::delete(workspace::delete_transit_feed),
        )
        .route("/v1/workspace/activate", post(workspace::activate))
        // GTFS editor: scenarios on top of a feed or from scratch.
        .route(
            "/v1/gtfs-editor/scenarios",
            get(gtfs_editor::list_scenarios).post(gtfs_editor::create_scenario),
        )
        .route(
            "/v1/gtfs-editor/scenarios/{scenario_id}",
            get(gtfs_editor::get_scenario)
                .put(gtfs_editor::update_scenario)
                .delete(gtfs_editor::delete_scenario),
        )
        .route(
            "/v1/gtfs-editor/scenarios/{scenario_id}/build",
            post(gtfs_editor::build_scenario),
        )
        .route(
            "/v1/gtfs-editor/scenarios/{scenario_id}/revert",
            post(gtfs_editor::revert_scenario),
        )
        .route(
            "/v1/gtfs-editor/scenarios/{scenario_id}/export",
            get(gtfs_editor::export_scenario),
        )
        .route(
            "/v1/transit-feeds/{feed_id}/bindings",
            get(transit::transit_bindings_handler),
        )
        .route(
            "/v1/transit-feeds/{feed_id}/stops",
            get(gtfs_editor::feed_stops),
        )
        .route(
            "/v1/transit-feeds/{feed_id}/routes",
            get(gtfs_editor::feed_routes),
        )
        .route(
            "/v1/transit-feeds/{feed_id}/patterns",
            get(gtfs_editor::feed_route_patterns),
        );
    router = console::attach_console(router, console_dir);
    router
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
