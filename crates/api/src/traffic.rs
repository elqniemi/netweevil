//! Traffic-aware analyses and live traffic updates.
//!
//! The runtime owns one [`TrafficService`] covering every motorized profile.
//! Live batches go through a single background worker that coalesces queued
//! batches, customizes the new engines and only then publishes the snapshot,
//! so requests never wait for a customization they did not ask for. The same
//! worker customizes the engines for upcoming bucket boundaries and live
//! expiries shortly before they take effect.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, Weak};

use anyhow::{Context, Result, bail};
use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use netweevil_core::TopologyBundle;
use netweevil_persist::{
    WorkspaceConfig, WorkspacePaths, read_traffic_bundle, read_traffic_manifest,
};
use netweevil_query::{
    EffectiveEngineDescription, EngineMode, LiveSwapReport, LiveTrafficUpdate, OdPairsDocument,
    PointSetDocument, RouteRequest, ServiceAreaRequest, TemporalRequestOptions, TrafficRouter,
    TrafficSelection, TrafficService, TrafficStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::dto::{ExecutionContext, ExecutionResponse, ResponseFormatQuery};
use crate::error::ApiError;
use crate::geojson::{
    geojson_response, matrix_result_geojson, od_result_geojson, route_result_geojson,
    service_area_result_geojson, wants_geojson,
};
use crate::handlers::{execution_context, run_analysis, run_inline_analysis};
use crate::state::{ApiState, LoadedProfile, ServiceRuntime, resolve_profile};

/// How long before a bucket boundary or live expiry its engines are built.
const PREWARM_LEAD: time::Duration = time::Duration::seconds(60);

pub(crate) struct TrafficRuntime {
    pub(crate) service: Arc<TrafficService>,
    worker: OnceLock<mpsc::UnboundedSender<LiveJob>>,
}

struct LiveJob {
    updates: Vec<LiveTrafficUpdate>,
    replace: bool,
    done: Option<oneshot::Sender<Result<LiveSwapReport, String>>>,
}

/// Builds the traffic service of a runtime: the selected historical set (if
/// any) plus live updates for every motorized profile. `None` when no loaded
/// profile is motorized and no historical set was selected.
pub(crate) fn load_traffic_runtime(
    paths: &WorkspacePaths,
    selection: &WorkspaceConfig,
    topology: &Arc<TopologyBundle>,
    profiles: &BTreeMap<String, LoadedProfile>,
) -> Result<Option<TrafficRuntime>> {
    let historical = selection
        .traffic
        .as_deref()
        .map(|traffic_id| {
            let manifest = read_traffic_manifest(paths, traffic_id)?;
            if manifest.dataset_id != selection.dataset_id {
                warn!(
                    traffic_id,
                    imported_for = %manifest.dataset_id,
                    dataset_id = %selection.dataset_id,
                    "traffic set was imported for another dataset; binding by source feature id"
                );
            }
            read_traffic_bundle(&manifest.bundle.path)
                .with_context(|| format!("reading traffic bundle {}", manifest.bundle.path))
        })
        .transpose()?;
    let capacity = match std::env::var("NETWEEVIL_TRAFFIC_CACHE_ENTRIES") {
        Ok(value) => value
            .parse::<usize>()
            .ok()
            .filter(|value| (1..=64).contains(value))
            .context("NETWEEVIL_TRAFFIC_CACHE_ENTRIES must be an integer from 1 to 64")?,
        Err(_) => 4,
    };
    let service = TrafficService::new(
        Arc::clone(topology),
        historical.as_ref(),
        profiles
            .iter()
            .map(|(profile_id, profile)| (profile_id.clone(), Arc::clone(&profile.engine))),
        capacity,
    )?;
    if service.routers().next().is_none() {
        if historical.is_some() {
            bail!("a traffic set needs at least one loaded car, HGV or transit profile");
        }
        return Ok(None);
    }
    if let Some(historical) = service.historical() {
        if historical.matched_edge_count() == 0 {
            bail!(
                "traffic set '{}' matches no edge of dataset '{}'",
                historical.traffic_id(),
                selection.dataset_id
            );
        }
        info!(
            traffic_id = historical.traffic_id(),
            matched_edges = historical.matched_edge_count(),
            distinct_bucket_metrics = historical.distinct_bucket_metrics(),
            cache_entries = capacity,
            "traffic ready"
        );
    }
    Ok(Some(TrafficRuntime {
        service: Arc::new(service),
        worker: OnceLock::new(),
    }))
}

impl TrafficRuntime {
    /// The live-update queue; starts the worker on first use. Must run
    /// inside the Tokio runtime.
    fn worker(&self) -> &mpsc::UnboundedSender<LiveJob> {
        self.worker.get_or_init(|| {
            let (sender, receiver) = mpsc::unbounded_channel();
            tokio::spawn(run_worker(Arc::downgrade(&self.service), receiver));
            sender
        })
    }

    /// Re-applies the live state of the runtime being replaced.
    pub(crate) fn carry_live_from(&self, previous: &TrafficService) {
        match self
            .service
            .carry_live_from(previous, OffsetDateTime::now_utc())
        {
            Ok(Some(report)) => info!(
                entries = report.entry_count,
                swap_ms = report.swap_ms,
                "live traffic carried over to the reloaded runtime"
            ),
            Ok(None) => {}
            Err(error) => warn!(%error, "live traffic could not be carried over"),
        }
    }
}

async fn run_worker(service: Weak<TrafficService>, mut jobs: mpsc::UnboundedReceiver<LiveJob>) {
    let mut prewarmed_until = OffsetDateTime::now_utc();
    loop {
        let now = OffsetDateTime::now_utc();
        let next_event = match service.upgrade() {
            Some(service) => service.next_event_after(prewarmed_until.max(now)),
            None => return,
        };
        let sleep = next_event
            .map(|event| (event - PREWARM_LEAD - now).max(time::Duration::ZERO))
            .unwrap_or(time::Duration::hours(1));
        let sleep = std::time::Duration::try_from(sleep).unwrap_or_default();
        tokio::select! {
            job = jobs.recv() => {
                let Some(job) = job else { return };
                let mut batch = vec![job];
                while let Ok(job) = jobs.try_recv() {
                    batch.push(job);
                }
                let Some(service) = service.upgrade() else { return };
                // A replacing batch discards everything queued before it.
                let start = batch.iter().rposition(|job| job.replace);
                let replace = start.is_some();
                let updates = batch[start.unwrap_or(0)..]
                    .iter()
                    .flat_map(|job| job.updates.iter().cloned())
                    .collect::<Vec<_>>();
                let result = tokio::task::spawn_blocking(move || {
                    service.apply_live(&updates, replace, OffsetDateTime::now_utc())
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| format!("{error:#}")));
                match &result {
                    Ok(report) => info!(
                        version = report.version,
                        updates = report.update_count,
                        entries = report.entry_count,
                        swap_ms = report.swap_ms,
                        "live traffic published"
                    ),
                    Err(error) => warn!(%error, "live traffic update failed"),
                }
                for job in batch {
                    if let Some(done) = job.done {
                        let _ = done.send(result.clone());
                    }
                }
            }
            _ = tokio::time::sleep(sleep), if next_event.is_some() => {
                let (Some(event), Some(service)) = (next_event, service.upgrade()) else {
                    return;
                };
                let built = tokio::task::spawn_blocking(move || service.prewarm(event)).await;
                match built {
                    Ok(Ok(built)) if built > 0 => info!(engines = built, at = %event, "traffic engines prepared ahead"),
                    Ok(Err(error)) => warn!(%error, "traffic prewarm failed"),
                    _ => {}
                }
                prewarmed_until = event;
            }
        }
    }
}

/// The traffic router for a request, when traffic applies to it.
pub(crate) fn router_for(
    runtime: &ServiceRuntime,
    profile_id: &str,
    temporal: &TemporalRequestOptions,
    engine_mode: EngineMode,
) -> Result<Option<Arc<TrafficRouter>>, ApiError> {
    let Some(traffic) = runtime.traffic.as_ref() else {
        return Ok(None);
    };
    let Some(router) = traffic.service.router(profile_id) else {
        return Ok(None);
    };
    if !router.applies_to(temporal) {
        return Ok(None);
    }
    if engine_mode != EngineMode::Auto {
        return Err(ApiError::bad_request(
            "traffic-aware requests support engine_mode auto only; set traffic: off for other modes",
        ));
    }
    // Requests are the first traffic activity: start prewarming.
    traffic.worker();
    Ok(Some(router))
}

#[derive(Serialize)]
struct TrafficExecutionContext {
    #[serde(flatten)]
    base: ExecutionContext,
    traffic: TrafficSelection,
}

fn traffic_context(
    runtime: &ServiceRuntime,
    profile_id: &str,
    selection: TrafficSelection,
) -> Result<TrafficExecutionContext, ApiError> {
    let profile = resolve_profile(runtime, Some(profile_id))?;
    let exact = selection.engine == "time_dependent_exact";
    let engine = EffectiveEngineDescription {
        route_engine: if exact {
            "time_dependent_nondominated_label_setting_with_traffic"
        } else {
            "traffic_departure_bucket_cch"
        },
        batch_engine: "traffic_departure_bucket_cch_batch_reuse",
        acceleration: if exact {
            "spatial_index+edge_phantoms+turn_automaton"
        } else {
            "spatial_index+edge_phantoms+cch_customized_per_traffic_bucket"
        },
    };
    Ok(TrafficExecutionContext {
        base: execution_context(runtime, profile, engine),
        traffic: selection,
    })
}

fn traffic_response<T: Serialize>(
    query: &ResponseFormatQuery,
    service: TrafficExecutionContext,
    result: T,
    to_geojson: impl FnOnce(&ExecutionContext, &T) -> Value,
) -> Result<Response, ApiError> {
    if wants_geojson(query) {
        let mut geojson = to_geojson(&service.base, &result);
        geojson["traffic"] = serde_json::to_value(&service.traffic)
            .map_err(|error| ApiError::internal(error.to_string()))?;
        return geojson_response(geojson);
    }
    Ok(Json(ExecutionResponse { service, result }).into_response())
}

pub(crate) async fn route_response(
    state: &ApiState,
    runtime: Arc<ServiceRuntime>,
    router: Arc<TrafficRouter>,
    query: &ResponseFormatQuery,
    request: RouteRequest,
    edge_names: Option<Arc<netweevil_core::EdgeNameBundle>>,
) -> Result<Response, ApiError> {
    let route_id = request.route_id.clone();
    let profile_id = router.profile_id().to_string();
    let (result, selection) = run_inline_analysis(state, "route", Some(&route_id), move || {
        router
            .execute_route(
                &request,
                edge_names.as_deref().map(|names| names.names.as_slice()),
            )?
            .context("traffic does not apply to this request")
    })
    .await?;
    info!(
        endpoint = "route",
        route_id = %result.route_id,
        engine = selection.engine,
        bucket = selection.bucket.as_ref().map(|bucket| bucket.label.as_str()).unwrap_or("-"),
        engine_cache = selection.engine_cache,
        time_s = result.summary.total_travel_time_s,
        "traffic response"
    );
    let service = traffic_context(runtime.as_ref(), &profile_id, selection)?;
    traffic_response(query, service, result, |context, result| {
        route_result_geojson(runtime.as_ref(), context, result)
    })
}

pub(crate) async fn od_response(
    state: &ApiState,
    runtime: Arc<ServiceRuntime>,
    router: Arc<TrafficRouter>,
    query: &ResponseFormatQuery,
    document: OdPairsDocument,
) -> Result<Response, ApiError> {
    let profile_id = router.profile_id().to_string();
    let (result, selection) = run_analysis(state, "od", None, move || {
        router
            .execute_od(&document)?
            .context("traffic does not apply to this request")
    })
    .await?;
    let service = traffic_context(runtime.as_ref(), &profile_id, selection)?;
    traffic_response(query, service, result, od_result_geojson)
}

pub(crate) async fn matrix_response(
    state: &ApiState,
    runtime: Arc<ServiceRuntime>,
    router: Arc<TrafficRouter>,
    query: &ResponseFormatQuery,
    origins: PointSetDocument,
    destinations: PointSetDocument,
) -> Result<Response, ApiError> {
    let profile_id = router.profile_id().to_string();
    let (result, selection) = run_analysis(state, "matrix", None, move || {
        router
            .execute_matrix(&origins, &destinations)?
            .context("traffic does not apply to this request")
    })
    .await?;
    let service = traffic_context(runtime.as_ref(), &profile_id, selection)?;
    traffic_response(query, service, result, matrix_result_geojson)
}

pub(crate) async fn service_area_response(
    state: &ApiState,
    runtime: Arc<ServiceRuntime>,
    router: Arc<TrafficRouter>,
    query: &ResponseFormatQuery,
    request: ServiceAreaRequest,
) -> Result<Response, ApiError> {
    let profile_id = router.profile_id().to_string();
    let analysis_id = request.analysis_id.clone();
    let (result, selection) = run_analysis(state, "service_area", Some(&analysis_id), move || {
        router
            .execute_service_area(&request)?
            .context("traffic does not apply to this request")
    })
    .await?;
    let service = traffic_context(runtime.as_ref(), &profile_id, selection)?;
    traffic_response(query, service, result, service_area_result_geojson)
}

#[derive(Serialize)]
pub(crate) struct TrafficStatusResponse {
    enabled: bool,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    status: Option<TrafficStatus>,
}

/// `GET /v1/traffic`: the loaded historical set, cached engines and live
/// state.
pub(crate) async fn traffic_status(
    State(state): State<ApiState>,
) -> Result<Json<TrafficStatusResponse>, ApiError> {
    let runtime = state.runtime()?;
    Ok(Json(TrafficStatusResponse {
        enabled: runtime.traffic.is_some(),
        status: runtime.traffic.as_ref().map(|traffic| {
            traffic.worker();
            traffic.service.status(OffsetDateTime::now_utc())
        }),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveTrafficRequest {
    #[serde(default)]
    updates: Vec<LiveTrafficUpdate>,
    /// Replace every live entry instead of merging into them.
    #[serde(default)]
    replace: bool,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct LiveTrafficQuery {
    /// Respond after the new engines are published instead of on receipt.
    #[serde(default)]
    wait: bool,
}

#[derive(Serialize)]
struct LiveTrafficAccepted {
    accepted: usize,
    replace: bool,
}

/// `POST /v1/traffic/live`: validate a batch now, customize and publish it
/// in the background.
pub(crate) async fn post_live_traffic(
    State(state): State<ApiState>,
    Query(query): Query<LiveTrafficQuery>,
    Json(payload): Json<LiveTrafficRequest>,
) -> Result<Response, ApiError> {
    let now = OffsetDateTime::now_utc();
    for update in &payload.updates {
        update
            .validated_expiry(now)
            .map_err(|error| ApiError::bad_request(format!("{error:#}")))?;
    }
    enqueue_live(&state, payload.updates, payload.replace, query.wait).await
}

/// `DELETE /v1/traffic/live`: drop every live entry.
pub(crate) async fn delete_live_traffic(
    State(state): State<ApiState>,
    Query(query): Query<LiveTrafficQuery>,
) -> Result<Response, ApiError> {
    enqueue_live(&state, Vec::new(), true, query.wait).await
}

async fn enqueue_live(
    state: &ApiState,
    updates: Vec<LiveTrafficUpdate>,
    replace: bool,
    wait: bool,
) -> Result<Response, ApiError> {
    let runtime = state.runtime()?;
    let traffic = runtime.traffic.as_ref().ok_or_else(|| {
        ApiError::bad_request("live traffic needs a loaded car, HGV or transit profile")
    })?;
    let accepted = updates.len();
    let (done, receiver) = if wait {
        let (sender, receiver) = oneshot::channel();
        (Some(sender), Some(receiver))
    } else {
        (None, None)
    };
    traffic
        .worker()
        .send(LiveJob {
            updates,
            replace,
            done,
        })
        .map_err(|_| ApiError::unavailable("the live traffic worker stopped"))?;
    let Some(receiver) = receiver else {
        return Ok((
            StatusCode::ACCEPTED,
            Json(LiveTrafficAccepted { accepted, replace }),
        )
            .into_response());
    };
    let report = receiver
        .await
        .map_err(|_| ApiError::unavailable("the live traffic worker stopped"))?
        .map_err(ApiError::bad_request)?;
    Ok(Json(report).into_response())
}

#[cfg(test)]
mod tests;
