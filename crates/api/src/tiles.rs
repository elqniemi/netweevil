//! Network debug vector tiles: the edges a profile can traverse, as Mapbox
//! Vector Tiles with their class, speed, access and oneway attributes.
//!
//! `GET /v1/tiles/{z}/{x}/{y}.mvt?profile_id=...` serves one `network`
//! layer. Two-way segments are one feature drawn in the direction of the
//! lower edge index; one-way segments are drawn in their travel direction.
//! Low zooms keep only major classes so a tile stays small.

use std::f64::consts::PI;
use std::sync::Arc;

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use mvt::{GeomEncoder, GeomType, Tile};
use netweevil_core::{AccessMask, CompiledProfileBundle, RoadClass, TopologyBundle};
use netweevil_query::PreparedRoutingEngine;
use serde::Deserialize;

use crate::error::ApiError;
use crate::network::{enum_label, mode_bit, profile_values};
use crate::state::{ApiState, execute_on_routing_worker, load_edge_names, resolve_profile};

pub(crate) const TILE_EXTENT: u32 = 4096;
/// Tile-space buffer around each tile so lines join across tile edges.
const TILE_BUFFER: f64 = 64.0;
const MAX_TILE_ZOOM: u8 = 22;
/// Features per tile; the response carries `X-NetWeevil-Truncated` beyond.
const MAX_TILE_FEATURES: usize = 100_000;
pub(crate) const LAYER_NAME: &str = "network";

#[derive(Debug, Deserialize)]
pub(crate) struct TileQuery {
    #[serde(default)]
    pub(crate) profile_id: Option<String>,
}

pub(crate) async fn network_tile_handler(
    State(state): State<ApiState>,
    UrlPath((z, x, tile)): UrlPath<(u8, u32, String)>,
    Query(query): Query<TileQuery>,
) -> Result<Response, ApiError> {
    let y = tile
        .strip_suffix(".mvt")
        .filter(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|digits| digits.parse::<u32>().ok())
        .ok_or_else(|| ApiError::bad_request("tile name must be {y}.mvt"))?;
    if z > MAX_TILE_ZOOM || x >= (1_u32 << z) || y >= (1_u32 << z) {
        return Err(ApiError::bad_request(format!(
            "tile {z}/{x}/{y} is outside the zoom 0..={MAX_TILE_ZOOM} tile grid"
        )));
    }
    let runtime = state.runtime()?;
    let profile = resolve_profile(&runtime, query.profile_id.as_deref())?;
    // Names only decorate results; a dataset without the name bundle still
    // locates and renders.
    let names = load_edge_names(&runtime).unwrap_or_default();
    let engine = Arc::clone(&profile.engine);
    let topology = runtime.topology.clone();
    let mode = mode_bit(profile);
    let etag = format!(
        "\"{}-{}-{z}-{x}-{y}\"",
        profile.manifest.bundle.bundle_id.0, profile.manifest.created_at
    );
    let (bytes, truncated) = execute_on_routing_worker(&runtime, move || {
        network_tile(&topology, &engine, &names.names, mode, TileId { z, x, y })
    })
    .await
    .map_err(ApiError::from_execution_error)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/vnd.mapbox-vector-tile"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        [
            (header::ETAG, etag),
            (
                header::HeaderName::from_static("x-netweevil-truncated"),
                truncated.to_string(),
            ),
        ],
        bytes,
    )
        .into_response())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TileId {
    pub(crate) z: u8,
    pub(crate) x: u32,
    pub(crate) y: u32,
}

impl TileId {
    /// Tile-space coordinates (0..extent, y down) of a lon/lat position.
    pub(crate) fn project(self, lon: f64, lat: f64) -> (f64, f64) {
        let scale = f64::from(1_u32 << self.z);
        let lat = lat.clamp(-85.051_128_78, 85.051_128_78).to_radians();
        let world_x = (lon + 180.0) / 360.0;
        let world_y = (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / PI) / 2.0;
        (
            (world_x * scale - f64::from(self.x)) * f64::from(TILE_EXTENT),
            (world_y * scale - f64::from(self.y)) * f64::from(TILE_EXTENT),
        )
    }

    /// `[west, south, east, north]` of the tile grown by `buffer` tile units.
    pub(crate) fn bbox(self, buffer: f64) -> [f64; 4] {
        let scale = f64::from(1_u32 << self.z);
        let pad = buffer / f64::from(TILE_EXTENT);
        let lon = |x: f64| x / scale * 360.0 - 180.0;
        let lat = |y: f64| (PI * (1.0 - 2.0 * y / scale)).sinh().atan().to_degrees();
        [
            lon(f64::from(self.x) - pad),
            lat(f64::from(self.y) + 1.0 + pad),
            lon(f64::from(self.x) + 1.0 + pad),
            lat(f64::from(self.y) - pad),
        ]
    }
}

/// Classes drawn at zoom `z`: everything from 14, then progressively only
/// the major road classes.
pub(crate) fn class_visible(class: RoadClass, z: u8) -> bool {
    match z {
        14.. => true,
        12..=13 => !matches!(
            class,
            RoadClass::Service | RoadClass::Track | RoadClass::Path | RoadClass::Unknown
        ),
        10..=11 => matches!(
            class,
            RoadClass::Motorway | RoadClass::Trunk | RoadClass::Primary | RoadClass::Ferry
        ),
        _ => matches!(class, RoadClass::Motorway | RoadClass::Trunk),
    }
}

/// Clips segment `a -> b` to the square `[low, high]^2` (Liang-Barsky).
pub(crate) fn clip_segment(
    a: (f64, f64),
    b: (f64, f64),
    low: f64,
    high: f64,
) -> Option<((f64, f64), (f64, f64))> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
    for (p, q) in [
        (-dx, a.0 - low),
        (dx, high - a.0),
        (-dy, a.1 - low),
        (dy, high - a.1),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
            continue;
        }
        let t = q / p;
        if p < 0.0 {
            t0 = t0.max(t);
        } else {
            t1 = t1.min(t);
        }
        if t0 > t1 {
            return None;
        }
    }
    Some((
        (a.0 + t0 * dx, a.1 + t0 * dy),
        (a.0 + t1 * dx, a.1 + t1 * dy),
    ))
}

/// Encodes one tile; returns the bytes and whether features were dropped.
fn network_tile(
    topology: &TopologyBundle,
    engine: &PreparedRoutingEngine,
    names: &[String],
    mode: u16,
    tile_id: TileId,
) -> anyhow::Result<(Vec<u8>, bool)> {
    let metrics = engine.metrics_arc();
    let mut tile = Tile::new(TILE_EXTENT);
    let mut layer = tile.create_layer(LAYER_NAME);
    let mut truncated = false;
    let mut count = 0_usize;
    for edge_index in engine.routable_edges_in_bbox(tile_id.bbox(TILE_BUFFER)) {
        let edge_index = edge_index as usize;
        let attributes = topology.edge_profile(edge_index);
        if !class_visible(attributes.road_class, tile_id.z) {
            continue;
        }
        let reverse =
            reverse_edge(topology, edge_index).filter(|&reverse| engine.is_routable_edge(reverse));
        if reverse.is_some_and(|reverse| reverse < edge_index) {
            continue;
        }
        let routing = topology.routing_edge(edge_index);
        let from = &topology.nodes[routing.from.0 as usize];
        let to = &topology.nodes[routing.to.0 as usize];
        let Some((start, end)) = clip_segment(
            tile_id.project(from.lon, from.lat),
            tile_id.project(to.lon, to.lat),
            -TILE_BUFFER,
            f64::from(TILE_EXTENT) + TILE_BUFFER,
        ) else {
            continue;
        };
        if (start.0.round(), start.1.round()) == (end.0.round(), end.1.round()) {
            continue;
        }
        if count == MAX_TILE_FEATURES {
            truncated = true;
            break;
        }
        count += 1;
        let geometry = GeomEncoder::new(GeomType::Linestring)
            .point(start.0, start.1)?
            .point(end.0, end.1)?
            .encode()?;
        let mut feature = layer.into_feature(geometry);
        feature.set_id(u64::from(routing.edge_id.0));
        add_edge_tags(
            &mut feature,
            topology,
            &metrics,
            names,
            mode,
            edge_index,
            reverse,
        );
        layer = feature.into_layer();
    }
    tile.add_layer(layer)?;
    Ok((tile.to_bytes()?, truncated))
}

fn add_edge_tags(
    feature: &mut mvt::Feature,
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    names: &[String],
    mode: u16,
    edge_index: usize,
    reverse: Option<usize>,
) {
    let routing = topology.routing_edge(edge_index);
    let attributes = topology.edge_profile(edge_index);
    let length_m = f64::from(routing.length_m);
    let label = |value: serde_json::Value| value.as_str().map(str::to_owned);
    feature.add_tag_uint("edge_id", u64::from(routing.edge_id.0));
    feature.add_tag_int("way_id", routing.source_way_id);
    if let Some(class) = label(enum_label(&attributes.road_class)) {
        feature.add_tag_string("road_class", &class);
    }
    if let Some(highway) = label(enum_label(&attributes.highway)) {
        feature.add_tag_string("highway", &highway);
    }
    feature.add_tag_bool("oneway", reverse.is_none());
    feature.add_tag_bool(
        "access_car",
        attributes.access_mask.contains(AccessMask::CAR),
    );
    feature.add_tag_bool(
        "access_bicycle",
        attributes.access_mask.contains(AccessMask::BICYCLE),
    );
    feature.add_tag_bool(
        "access_foot",
        attributes.access_mask.contains(AccessMask::FOOT),
    );
    if let Some(max_speed) = attributes.max_speed_kph {
        feature.add_tag_double("max_speed_kph", f64::from(max_speed));
    }
    let values = profile_values(
        &metrics.edge_metrics[edge_index],
        length_m,
        attributes.access_mask,
        mode,
    );
    if let Some(speed) = values.speed_kph {
        feature.add_tag_double("speed_kph", speed);
    }
    if let Some(reverse) = reverse {
        let reverse_values = profile_values(
            &metrics.edge_metrics[reverse],
            length_m,
            topology.edge_profile(reverse).access_mask,
            mode,
        );
        feature.add_tag_uint(
            "reverse_edge_id",
            u64::from(topology.routing_edge(reverse).edge_id.0),
        );
        if let Some(speed) = reverse_values.speed_kph {
            feature.add_tag_double("reverse_speed_kph", speed);
        }
    }
    if let Some(name) = topology
        .edge_presentation(edge_index)
        .name_index
        .and_then(|index| names.get(index as usize))
    {
        feature.add_tag_string("name", name);
    }
}

/// The edge running `to -> from` of `edge_index`, if the topology has one.
pub(crate) fn reverse_edge(topology: &TopologyBundle, edge_index: usize) -> Option<usize> {
    let routing = topology.routing_edge(edge_index);
    let ebt = &topology.edge_based_topology;
    let to = routing.to.0 as usize;
    if ebt.node_first_out.len() != topology.nodes.len() + 1 {
        return None;
    }
    let (first, last) = (
        ebt.node_first_out[to] as usize,
        ebt.node_first_out[to + 1] as usize,
    );
    ebt.node_edge_order[first..last]
        .iter()
        .map(|&other| other as usize)
        .find(|&other| topology.routing_edge(other).to == routing.from)
}

#[cfg(test)]
mod tests;
