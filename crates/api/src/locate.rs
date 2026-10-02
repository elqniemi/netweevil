//! Profile-aware inspection of a point's position on the routing network:
//! the snap candidates routing would use and the k nearest road segments
//! with their edge attributes.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use netweevil_core::{AccessMask, CompiledProfileBundle, TopologyBundle};
use netweevil_query::{LabeledPoint, NearestSegment, SnapOptions, SnappedPoint};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::dto::{ExecutionResponse, ProfiledRequest};
use crate::error::ApiError;
use crate::network::{enum_label, mode_bit, profile_values};
use crate::state::{ApiState, execute_on_routing_worker, load_edge_names, resolve_profile};
use crate::tiles::reverse_edge;

const MAX_LOCATE_COUNT: usize = 100;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocateRequest {
    points: Vec<LabeledPoint>,
    #[serde(default)]
    snap: SnapOptions,
    #[serde(default)]
    direction: LocateDirection,
    /// Nearest road segments to report per point.
    #[serde(default = "default_count")]
    count: usize,
}

fn default_count() -> usize {
    1
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LocateDirection {
    #[default]
    Origin,
    Destination,
}

#[derive(Debug, Serialize)]
pub(crate) struct LocateContext {
    dataset_id: String,
    profile_id: String,
    profile_hash: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct LocatedPoint {
    point_id: String,
    /// Snap candidates a route from/to this point would start from; empty
    /// when the point does not snap within `snap.max_distance_m`.
    candidates: Vec<SnappedPoint>,
    /// Nearest road segments, nearest first.
    nearest: Vec<LocatedSegment>,
}

#[derive(Debug, Serialize)]
pub(crate) struct LocatedSegment {
    distance_m: f64,
    lon: f64,
    lat: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    z: Option<f64>,
    edges: Vec<LocatedEdge>,
}

/// One directed edge of a located segment, with what the profile makes of it.
#[derive(Debug, Serialize)]
pub(crate) struct LocatedEdge {
    edge_index: u32,
    edge_id: u32,
    from_node_id: u32,
    to_node_id: u32,
    /// Position of the projected point along this edge, 0 at `from_node_id`.
    fraction: f64,
    way_id: i64,
    name: Option<String>,
    road_class: Value,
    highway: Value,
    surface: Value,
    max_speed_kph: Option<f32>,
    lanes: Option<u8>,
    length_m: f64,
    oneway: bool,
    access_car: bool,
    access_bicycle: bool,
    access_foot: bool,
    access_bus: bool,
    access_hgv: bool,
    access_taxi: bool,
    access_motorcycle: bool,
    access_moped: bool,
    travel_time_s: Option<f64>,
    speed_kph: Option<f64>,
    generalized_cost: Option<f64>,
    component_id: Option<u32>,
}

pub(crate) async fn locate_handler(
    State(state): State<ApiState>,
    Json(payload): Json<ProfiledRequest<LocateRequest>>,
) -> Result<Json<ExecutionResponse<LocateContext, Vec<LocatedPoint>>>, ApiError> {
    let runtime = state.runtime()?;
    let profile = resolve_profile(&runtime, payload.profile_id.as_deref())?;
    let request = payload.request;
    if request.points.is_empty() {
        return Err(ApiError::bad_request("locate requires at least one point"));
    }
    if !(1..=MAX_LOCATE_COUNT).contains(&request.count) {
        return Err(ApiError::bad_request(format!(
            "count must be in [1, {MAX_LOCATE_COUNT}]"
        )));
    }
    for point in &request.points {
        if !(-180.0..=180.0).contains(&point.lon) || !(-90.0..=90.0).contains(&point.lat) {
            return Err(ApiError::bad_request(format!(
                "point '{}' must have longitude in [-180, 180] and latitude in [-90, 90]",
                point.id
            )));
        }
    }
    let service = LocateContext {
        dataset_id: runtime.dataset_manifest.dataset_id.0.clone(),
        profile_id: profile.document.profile.id.clone(),
        profile_hash: profile.manifest.profile_hash.clone(),
    };
    // Names only decorate results; a dataset without the name bundle still
    // locates and renders.
    let names = load_edge_names(&runtime).unwrap_or_default();
    let engine = Arc::clone(&profile.engine);
    let topology = runtime.topology.clone();
    let metrics = engine.metrics_arc();
    let mode = mode_bit(profile);
    let result = execute_on_routing_worker(&runtime, move || {
        let is_origin = matches!(request.direction, LocateDirection::Origin);
        request
            .points
            .into_iter()
            .map(|point| {
                // A point beyond the snap radius still reports (no) nearest
                // segments; other snapping errors are request errors.
                let candidates = engine
                    .snap_route_candidates_with_options(&point, &request.snap, is_origin)
                    .or_else(|error| {
                        if netweevil_query::analysis_failure(&error).is_some() {
                            Ok(Vec::new())
                        } else {
                            Err(error)
                        }
                    })?;
                let nearest = engine
                    .nearest_segments(&point, &request.snap, request.count)?
                    .into_iter()
                    .map(|segment| {
                        located_segment(&topology, &engine, &metrics, &names.names, mode, segment)
                    })
                    .collect();
                Ok(LocatedPoint {
                    point_id: point.id,
                    candidates,
                    nearest,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()
    })
    .await
    .map_err(ApiError::from_execution_error)?;
    Ok(Json(ExecutionResponse { service, result }))
}

fn located_segment(
    topology: &TopologyBundle,
    engine: &netweevil_query::PreparedRoutingEngine,
    metrics: &CompiledProfileBundle,
    names: &[String],
    mode: u16,
    segment: NearestSegment,
) -> LocatedSegment {
    let edges = segment
        .edges
        .iter()
        .map(|directed| {
            let edge_index = directed.edge_index as usize;
            let routing = topology.routing_edge(edge_index);
            let attributes = topology.edge_profile(edge_index);
            let length_m = f64::from(routing.length_m);
            let values = profile_values(
                &metrics.edge_metrics[edge_index],
                length_m,
                attributes.access_mask,
                mode,
            );
            LocatedEdge {
                edge_index: directed.edge_index,
                edge_id: routing.edge_id.0,
                from_node_id: routing.from.0,
                to_node_id: routing.to.0,
                fraction: directed.fraction,
                way_id: routing.source_way_id,
                name: topology
                    .edge_presentation(edge_index)
                    .name_index
                    .and_then(|index| names.get(index as usize))
                    .cloned(),
                road_class: enum_label(&attributes.road_class),
                highway: enum_label(&attributes.highway),
                surface: enum_label(&attributes.surface),
                max_speed_kph: attributes.max_speed_kph,
                lanes: attributes.lanes,
                length_m,
                // Profile-aware: the reverse direction must be traversable.
                oneway: !reverse_edge(topology, edge_index)
                    .is_some_and(|reverse| engine.is_routable_edge(reverse)),
                access_car: attributes.access_mask.contains(AccessMask::CAR),
                access_bicycle: attributes.access_mask.contains(AccessMask::BICYCLE),
                access_foot: attributes.access_mask.contains(AccessMask::FOOT),
                access_bus: attributes.access_mask.contains(AccessMask::BUS),
                access_hgv: attributes.access_mask.contains(AccessMask::HGV),
                access_taxi: attributes.access_mask.contains(AccessMask::TAXI),
                access_motorcycle: attributes.access_mask.contains(AccessMask::MOTORCYCLE),
                access_moped: attributes.access_mask.contains(AccessMask::MOPED),
                travel_time_s: values.travel_time_s,
                speed_kph: values.speed_kph,
                generalized_cost: values.cost,
                component_id: topology.edge_component_id(directed.edge_index),
            }
        })
        .collect();
    LocatedSegment {
        distance_m: segment.distance_m,
        lon: segment.lon,
        lat: segment.lat,
        z: segment.z,
        edges,
    }
}
