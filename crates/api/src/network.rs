//! Network explorer: the directed edges inside a map view with their source
//! attributes and what one or two compiled profiles make of them (allowed,
//! travel time, speed, generalized cost), as GeoJSON for the console.

use std::collections::HashMap;

use axum::Json;
use axum::extract::State;
use netweevil_core::{AccessMask, TopologyBundle};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tracing::info;

use crate::error::ApiError;
use crate::state::{
    ApiState, LoadedProfile, execute_on_routing_worker, load_edge_names, resolve_profile,
};

#[derive(Debug, Deserialize)]
pub(crate) struct NetworkEdgesRequest {
    /// `[west, south, east, north]`
    pub(crate) bbox: [f64; 4],
    #[serde(default)]
    pub(crate) profile_id: Option<String>,
    /// Second profile whose values are reported next to the first, with deltas.
    #[serde(default)]
    pub(crate) compare_profile_id: Option<String>,
    #[serde(default = "default_max_edges")]
    pub(crate) max_edges: usize,
}

fn default_max_edges() -> usize {
    25_000
}

#[derive(Debug, Serialize)]
pub(crate) struct NetworkEdgesResponse {
    pub(crate) r#type: &'static str,
    pub(crate) features: Vec<Value>,
    pub(crate) meta: NetworkEdgesMeta,
}

#[derive(Debug, Serialize)]
pub(crate) struct NetworkEdgesMeta {
    pub(crate) dataset_id: String,
    pub(crate) profile_id: String,
    pub(crate) compare_profile_id: Option<String>,
    pub(crate) edge_count: usize,
    pub(crate) truncated: bool,
    /// Min/max per numeric property, for colour scales.
    pub(crate) ranges: HashMap<&'static str, [f64; 2]>,
}

pub(crate) async fn network_edges_handler(
    State(state): State<ApiState>,
    Json(payload): Json<NetworkEdgesRequest>,
) -> Result<Json<NetworkEdgesResponse>, ApiError> {
    let runtime = state.runtime()?;
    let [west, south, east, north] = payload.bbox;
    if !(west < east && south < north) || !payload.bbox.iter().all(|value| value.is_finite()) {
        return Err(ApiError::bad_request(
            "bbox must be [west, south, east, north]",
        ));
    }
    let profile = resolve_profile(&runtime, payload.profile_id.as_deref())?;
    let compare = payload
        .compare_profile_id
        .as_deref()
        .map(|id| resolve_profile(&runtime, Some(id)))
        .transpose()?;
    let names = load_edge_names(&runtime)?;
    let dataset_id = runtime.dataset_manifest.dataset_id.0.clone();
    let profile_id = profile.document.profile.id.clone();
    let compare_id = compare.map(|profile| profile.document.profile.id.clone());
    info!(endpoint = "network_edges", profile_id = %profile_id, compare = ?compare_id, "request");

    let topology = runtime.topology.clone();
    let metrics_a = profile.engine.metrics_arc();
    let metrics_b = compare.map(|profile| profile.engine.metrics_arc());
    let mode_bit_a = mode_bit(profile);
    let mode_bit_b = compare.map(mode_bit);
    let max_edges = payload.max_edges.clamp(1, 200_000);
    let response = execute_on_routing_worker(&runtime, move || {
        let edges = edges_in_bbox(&topology, payload.bbox, max_edges);
        let truncated = edges.len() >= max_edges;
        let mut ranges: HashMap<&'static str, [f64; 2]> = HashMap::new();
        let mut track = |key: &'static str, value: Option<f64>| {
            if let Some(value) = value.filter(|value| value.is_finite()) {
                let entry = ranges.entry(key).or_insert([value, value]);
                entry[0] = entry[0].min(value);
                entry[1] = entry[1].max(value);
            }
        };
        let mut features = Vec::with_capacity(edges.len());
        for edge_index in edges {
            let routing = topology.routing_edge(edge_index);
            let attributes = topology.edge_profile(edge_index);
            let presentation = topology.edge_presentation(edge_index);
            let from = &topology.nodes[routing.from.0 as usize];
            let to = &topology.nodes[routing.to.0 as usize];
            let length_m = f64::from(routing.length_m);
            let mut properties = Map::new();
            properties.insert("edge_id".into(), json!(routing.edge_id.0));
            properties.insert("way_id".into(), json!(routing.source_way_id));
            properties.insert("direction".into(), json!(routing.source_direction));
            properties.insert("length_m".into(), json!(length_m));
            properties.insert("ascent_m".into(), json!(routing.ascent_m));
            properties.insert("descent_m".into(), json!(routing.descent_m));
            properties.insert("road_class".into(), enum_label(&attributes.road_class));
            properties.insert("highway".into(), enum_label(&attributes.highway));
            properties.insert("surface".into(), enum_label(&attributes.surface));
            properties.insert("smoothness".into(), enum_label(&attributes.smoothness));
            properties.insert("max_speed_kph".into(), json!(attributes.max_speed_kph));
            properties.insert("lanes".into(), json!(attributes.lanes));
            properties.insert("is_toll".into(), json!(attributes.is_toll));
            properties.insert("duration_s".into(), json!(attributes.duration_s));
            properties.insert("access_car".into(), json!(attributes.access_mask.contains(AccessMask::CAR)));
            properties.insert("access_bicycle".into(), json!(attributes.access_mask.contains(AccessMask::BICYCLE)));
            properties.insert("access_foot".into(), json!(attributes.access_mask.contains(AccessMask::FOOT)));
            properties.insert("temporal".into(), json!(routing.temporal_rule_id.is_some()));
            properties.insert("oneway".into(), json!(!has_reverse_edge(&topology, edge_index)));
            properties.insert(
                "name".into(),
                presentation
                    .name_index
                    .and_then(|index| names.get(index as usize))
                    .map_or(Value::Null, |name| json!(name)),
            );
            properties.insert("component".into(), json!(topology.edge_component_id(edge_index as u32)));
            let slope = if length_m > 0.0 {
                Some(f64::from(routing.ascent_m - routing.descent_m) / length_m * 100.0)
            } else {
                None
            };
            properties.insert("grade_pct".into(), json!(slope));
            track("length_m", Some(length_m));
            track("max_speed_kph", attributes.max_speed_kph.map(f64::from));
            track("grade_pct", slope);

            let a = profile_values(&metrics_a.edge_metrics[edge_index], length_m, attributes.access_mask, mode_bit_a);
            properties.insert("allowed".into(), json!(a.allowed));
            properties.insert("access_allowed".into(), json!(a.access));
            properties.insert("travel_time_s".into(), json!(a.travel_time_s));
            properties.insert("speed_kph".into(), json!(a.speed_kph));
            properties.insert("cost".into(), json!(a.cost));
            properties.insert("cost_per_km".into(), json!(a.cost_per_km));
            track("travel_time_s", a.travel_time_s);
            track("speed_kph", a.speed_kph);
            track("cost", a.cost);
            track("cost_per_km", a.cost_per_km);
            if let Some(metrics_b) = metrics_b.as_ref() {
                let b = profile_values(&metrics_b.edge_metrics[edge_index], length_m, attributes.access_mask, mode_bit_b.unwrap_or(0));
                properties.insert("b_allowed".into(), json!(b.allowed));
                properties.insert("b_travel_time_s".into(), json!(b.travel_time_s));
                properties.insert("b_speed_kph".into(), json!(b.speed_kph));
                properties.insert("b_cost".into(), json!(b.cost));
                properties.insert("b_cost_per_km".into(), json!(b.cost_per_km));
                let allowed_diff = match (a.allowed, b.allowed) {
                    (true, true) => "both",
                    (true, false) => "only_a",
                    (false, true) => "only_b",
                    (false, false) => "neither",
                };
                properties.insert("allowed_diff".into(), json!(allowed_diff));
                let delta = |x: Option<f64>, y: Option<f64>| match (x, y) {
                    (Some(x), Some(y)) => Some(y - x),
                    _ => None,
                };
                let delta_time = delta(a.travel_time_s, b.travel_time_s);
                let delta_speed = delta(a.speed_kph, b.speed_kph);
                let delta_cost = delta(a.cost, b.cost);
                let ratio_time = match (a.travel_time_s, b.travel_time_s) {
                    (Some(x), Some(y)) if x > 0.0 => Some(y / x),
                    _ => None,
                };
                properties.insert("delta_travel_time_s".into(), json!(delta_time));
                properties.insert("delta_speed_kph".into(), json!(delta_speed));
                properties.insert("delta_cost".into(), json!(delta_cost));
                properties.insert("time_ratio".into(), json!(ratio_time));
                track("b_travel_time_s", b.travel_time_s);
                track("b_speed_kph", b.speed_kph);
                track("b_cost", b.cost);
                track("delta_travel_time_s", delta_time);
                track("delta_speed_kph", delta_speed);
                track("delta_cost", delta_cost);
                track("time_ratio", ratio_time);
            }
            features.push(json!({
                "type": "Feature",
                "id": routing.edge_id.0,
                "geometry": {"type": "LineString", "coordinates": [[from.lon, from.lat], [to.lon, to.lat]]},
                "properties": Value::Object(properties),
            }));
        }
        Ok(NetworkEdgesResponse {
            r#type: "FeatureCollection",
            meta: NetworkEdgesMeta {
                dataset_id,
                profile_id,
                compare_profile_id: compare_id,
                edge_count: features.len(),
                truncated,
                ranges,
            },
            features,
        })
    })
    .await
    .map_err(ApiError::from_execution_error)?;
    Ok(Json(response))
}

fn enum_label<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn mode_bit(profile: &LoadedProfile) -> u16 {
    match profile.document.profile.mode {
        netweevil_core::TravelMode::Car => AccessMask::CAR,
        netweevil_core::TravelMode::Bicycle => AccessMask::BICYCLE,
        netweevil_core::TravelMode::Foot => AccessMask::FOOT,
        netweevil_core::TravelMode::Transit => AccessMask::TRANSIT,
        netweevil_core::TravelMode::Hgv => AccessMask::HGV,
    }
}

struct ProfileValues {
    allowed: bool,
    access: bool,
    travel_time_s: Option<f64>,
    speed_kph: Option<f64>,
    cost: Option<f64>,
    cost_per_km: Option<f64>,
}

fn profile_values(
    metric: &netweevil_core::CompiledEdgeMetric,
    length_m: f64,
    access_mask: AccessMask,
    mode_bit: u16,
) -> ProfileValues {
    let travel_time_s = metric.travel_time_s.filter(|value| value.is_finite());
    let cost = metric.generalized_cost.filter(|value| value.is_finite());
    let allowed = travel_time_s.is_some() || cost.is_some();
    ProfileValues {
        allowed,
        access: access_mask.contains(mode_bit),
        speed_kph: travel_time_s
            .filter(|time| *time > 0.0)
            .map(|time| length_m / time * 3.6),
        cost_per_km: cost
            .filter(|_| length_m > 0.0)
            .map(|cost| cost / length_m * 1000.0),
        travel_time_s,
        cost,
    }
}

/// Whether the same source way also runs from `to` back to `from`.
fn has_reverse_edge(topology: &TopologyBundle, edge_index: usize) -> bool {
    let routing = topology.routing_edge(edge_index);
    let ebt = &topology.edge_based_topology;
    let to = routing.to.0 as usize;
    if ebt.node_first_out.len() != topology.nodes.len() + 1 {
        return true;
    }
    let (first, last) = (
        ebt.node_first_out[to] as usize,
        ebt.node_first_out[to + 1] as usize,
    );
    ebt.node_edge_order[first..last].iter().any(|&other| {
        let candidate = topology.routing_edge(other as usize);
        candidate.to == routing.from && candidate.source_way_id == routing.source_way_id
    })
}

/// Edge indexes whose start node lies inside the bbox, found through the node
/// grid and the node-to-outgoing-edge table; falls back to a full scan when
/// the bundle has neither.
fn edges_in_bbox(topology: &TopologyBundle, bbox: [f64; 4], max_edges: usize) -> Vec<usize> {
    let [west, south, east, north] = bbox;
    let inside = |node: &netweevil_core::TopologyNode| {
        node.lon >= west && node.lon <= east && node.lat >= south && node.lat <= north
    };
    let mut edges = Vec::new();
    let ebt = &topology.edge_based_topology;
    let has_out_table = ebt.node_first_out.len() == topology.nodes.len() + 1;
    if let Some(index) = topology.spatial_index.as_ref().filter(|_| has_out_table)
        && index.columns > 0
        && index.rows > 0
    {
        let col_of = |lon: f64| {
            (((lon - index.bounds.min_lon) / index.cell_width_deg).floor() as i64)
                .clamp(0, i64::from(index.columns) - 1)
        };
        let row_of = |lat: f64| {
            (((lat - index.bounds.min_lat) / index.cell_height_deg).floor() as i64)
                .clamp(0, i64::from(index.rows) - 1)
        };
        let (col_start, col_end) = (col_of(west), col_of(east));
        let (row_start, row_end) = (row_of(south), row_of(north));
        'cells: for row in row_start..=row_end {
            for col in col_start..=col_end {
                let cell = &index.cells[row as usize * index.columns as usize + col as usize];
                let start = cell.node_start as usize;
                let end = start + cell.node_len as usize;
                for &node_id in &index.node_ids[start..end] {
                    let node = &topology.nodes[node_id as usize];
                    if !inside(node) {
                        continue;
                    }
                    let first = ebt.node_first_out[node_id as usize] as usize;
                    let last = ebt.node_first_out[node_id as usize + 1] as usize;
                    for &edge in &ebt.node_edge_order[first..last] {
                        edges.push(edge as usize);
                        if edges.len() >= max_edges {
                            break 'cells;
                        }
                    }
                }
            }
        }
        return edges;
    }
    for edge_index in 0..topology.edge_count() {
        let routing = topology.routing_edge(edge_index);
        if inside(&topology.nodes[routing.from.0 as usize]) {
            edges.push(edge_index);
            if edges.len() >= max_edges {
                break;
            }
        }
    }
    edges
}
