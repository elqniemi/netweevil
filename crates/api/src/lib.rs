//! Local Axum HTTP API exposing datasets, profiles, routing analyses, GTFS
//! transit queries, and simulation control to local tools and the QGIS
//! plugin.

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::routing::{get, post};
use netweevil_persist::WorkspacePaths;
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing::info;

mod dto;
mod error;
mod geojson;
mod handlers;
mod simulation;
mod state;
mod transit;

pub use dto::{
    DatasetInfo, ExecutionContext, HealthResponse, MatrixExecutionRequest, MatrixExecutionResponse,
    MatrixRequest, OdExecutionRequest, OdExecutionResponse, ProfileInfo, RouteExecutionRequest,
    RouteExecutionResponse, ServiceAreaExecutionRequest, ServiceAreaExecutionResponse,
    ServiceInfoResponse, TransitExecutionContext, TransitFeedInfo, TransitRouteExecutionRequest,
    TransitRouteExecutionResponse, TransitServiceAreaExecutionRequest,
    TransitServiceAreaExecutionResponse,
};
pub use state::ApiServeOptions;

pub(crate) use error::ApiError;
pub(crate) use geojson::geojson_response;
pub(crate) use state::ApiState;

use simulation::SimulationRegistry;
use state::load_service_runtime;

pub async fn serve(paths: WorkspacePaths, options: ApiServeOptions) -> Result<()> {
    let state = ApiState {
        service: Arc::new(load_service_runtime(&paths, &options)?),
        simulations: Arc::new(SimulationRegistry::default()),
    };
    let app = router(state);

    info!(
        bind = %options.bind,
        dataset_id = %options.dataset_id,
        default_profile = %options.default_profile.display(),
        "starting netweevil api"
    );

    let listener = TcpListener::bind(options.bind)
        .await
        .with_context(|| format!("binding API listener on {}", options.bind))?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("running API server")
}

fn router(state: ApiState) -> Router {
    Router::new()
        .route("/healthz", get(handlers::healthz))
        .route("/readyz", get(handlers::readyz))
        .route("/v1/service", get(handlers::service_info))
        .route("/v1/profiles", get(handlers::list_profiles))
        .route("/v1/profiles/{profile_id}", get(handlers::get_profile))
        .route("/v1/route", post(handlers::route_handler))
        .route("/v1/transit-route", post(transit::transit_route_handler))
        .route(
            "/v1/transit-service-area",
            post(transit::transit_service_area_handler),
        )
        .route("/v1/od", post(handlers::od_handler))
        .route("/v1/matrix", post(handlers::matrix_handler))
        .route("/v1/service-area", post(handlers::service_area_handler))
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
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
