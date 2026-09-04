use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use netweevil_core::{TopologyBundle, TravelMode};
use netweevil_profile::{ReturnConfig, ReturnGeometry};
use netweevil_query::{
    LabeledPoint, PreparedRoutingEngine, RouteRequest, SnapOptions, SnappedPoint,
};
use netweevil_transit::{
    AccessMode, StreetTimeEstimator, TransitLeg, TransitLegType, TransitModeOptions,
    TransitRouteResult, TransitStop, TransitStopBindingTarget, TransitStreetAccessModel,
    TransitStreetPath, TransitWalkingGeometry,
};
use tracing::{info, warn};

use crate::dto::{
    ResponseFormatQuery, TransitExecutionContext, TransitRouteExecutionRequest,
    TransitRouteExecutionResponse, TransitServiceAreaExecutionRequest,
    TransitServiceAreaExecutionResponse,
};
use crate::error::ApiError;
use crate::geojson::{geojson_response, transit_service_area_result_geojson, wants_geojson};
use crate::state::{
    ApiState, LoadedProfile, ServiceRuntime, execute_on_routing_worker, resolve_profile,
};

pub(crate) async fn transit_route_handler(
    State(state): State<ApiState>,
    Json(payload): Json<TransitRouteExecutionRequest>,
) -> Result<Json<TransitRouteExecutionResponse>, ApiError> {
    let feed = state
        .service
        .transit_feeds
        .get(&payload.feed_id)
        .ok_or_else(|| {
            ApiError::not_found(format!("unknown transit feed '{}'", payload.feed_id))
        })?;
    info!(
        endpoint = "transit_route",
        feed_id = %payload.feed_id,
        route_id = %payload.request.route_id,
        "request"
    );
    let mut request = payload.request;
    let transfer_profile_id = request.modes.transfer_profile_id.clone();
    let walking_geometry = request.returns.walking_geometry;
    let network_street_access = matches!(
        request.modes.street_access,
        TransitStreetAccessModel::Network
    );
    let street_engines =
        if matches!(walking_geometry, TransitWalkingGeometry::Network) || network_street_access {
            if matches!(walking_geometry, TransitWalkingGeometry::Network) {
                request.returns.include_geometry = true;
            }
            Some(resolve_transit_street_engines(
                state.service.as_ref(),
                payload.pedestrian_profile_id.as_deref(),
                payload.access_profile_id.as_deref(),
                payload.egress_profile_id.as_deref(),
                &request.modes,
            )?)
        } else {
            None
        };
    let router = Arc::clone(&feed.router);
    let manifest = feed.manifest.clone();
    let route_id = request.route_id.clone();
    let pedestrian_profile_id = street_engines
        .as_ref()
        .map(|engines| engines.walk.0.clone());
    let access_profile_id = street_engines
        .as_ref()
        .and_then(|engines| side_profile_ids(&engines.access));
    let egress_profile_id = street_engines
        .as_ref()
        .and_then(|engines| side_profile_ids(&engines.egress));
    let replace_geometry = matches!(walking_geometry, TransitWalkingGeometry::Network);
    let result = execute_on_routing_worker(state.service.as_ref(), move || {
        let estimator = street_engines
            .as_ref()
            .filter(|_| network_street_access)
            .map(StreetEngineTimeEstimator::new);
        let mut result = router.execute_route_with_street_estimator(
            &request,
            estimator
                .as_ref()
                .map(|estimator| estimator as &dyn StreetTimeEstimator),
        )?;
        if replace_geometry && let Some(engines) = street_engines.as_ref() {
            replace_transit_street_leg_geometries(&mut result, engines);
        }
        Ok(result)
    })
    .await
    .map_err(|error| {
        warn!(endpoint = "transit_route", route_id = %route_id, %error, "request failed");
        ApiError::bad_request(error.to_string())
    })?;
    Ok(Json(TransitRouteExecutionResponse {
        service: TransitExecutionContext {
            feed_id: manifest.feed_id,
            service_start_date: manifest.service_start_date,
            service_days: manifest.service_days,
            route_engine: "scheduled_connection_scan_street_transit".to_string(),
            walking_geometry: match walking_geometry {
                TransitWalkingGeometry::StraightLine => "straight_line".to_string(),
                TransitWalkingGeometry::Network => "network".to_string(),
            },
            transfer_profile_id,
            pedestrian_profile_id,
            access_profile_id,
            egress_profile_id,
        },
        result,
    }))
}

pub(crate) async fn transit_service_area_handler(
    State(state): State<ApiState>,
    Query(query): Query<ResponseFormatQuery>,
    Json(payload): Json<TransitServiceAreaExecutionRequest>,
) -> Result<Response, ApiError> {
    let feed = state
        .service
        .transit_feeds
        .get(&payload.feed_id)
        .ok_or_else(|| {
            ApiError::not_found(format!("unknown transit feed '{}'", payload.feed_id))
        })?;
    info!(
        endpoint = "transit_service_area",
        feed_id = %payload.feed_id,
        analysis_id = %payload.request.analysis_id,
        origins = payload.request.origins.len(),
        max_travel_time_s = payload.request.max_travel_time_s,
        format = query.format.as_deref().unwrap_or("json"),
        "request"
    );
    let manifest = feed.manifest.clone();
    let router = Arc::clone(&feed.router);
    let request = payload.request;
    let transfer_profile_id = request.modes.transfer_profile_id.clone();
    let network_street_access = matches!(
        request.modes.street_access,
        TransitStreetAccessModel::Network
    );
    let street_engines = if network_street_access {
        Some(resolve_transit_street_engines(
            state.service.as_ref(),
            None,
            None,
            None,
            &request.modes,
        )?)
    } else {
        None
    };
    let result = execute_on_routing_worker(state.service.as_ref(), move || {
        let estimator = street_engines.as_ref().map(StreetEngineTimeEstimator::new);
        router.execute_service_area_with_street_estimator(
            &request,
            estimator
                .as_ref()
                .map(|estimator| estimator as &dyn StreetTimeEstimator),
        )
    })
    .await
    .map_err(|error| {
        warn!(endpoint = "transit_service_area", %error, "request failed");
        ApiError::from_execution_error(error)
    })?;
    let service = TransitExecutionContext {
        feed_id: manifest.feed_id,
        service_start_date: manifest.service_start_date,
        service_days: manifest.service_days,
        route_engine: "scheduled_connection_scan_transit_service_area".to_string(),
        walking_geometry: "straight_line".to_string(),
        transfer_profile_id,
        pedestrian_profile_id: None,
        access_profile_id: None,
        egress_profile_id: None,
    };
    info!(
        endpoint = "transit_service_area",
        analysis_id = %result.analysis_id,
        stops = result.stops.len(),
        stop_segments = result.stop_segments.len(),
        "response"
    );
    if wants_geojson(&query) {
        return geojson_response(transit_service_area_result_geojson(&service, &result));
    }
    Ok(Json(TransitServiceAreaExecutionResponse { service, result }).into_response())
}

fn resolve_transit_pedestrian_profile<'a>(
    service: &'a ServiceRuntime,
    requested_profile_id: Option<&str>,
) -> Result<&'a LoadedProfile, ApiError> {
    if let Some(profile_id) = requested_profile_id {
        let profile = resolve_profile(service, Some(profile_id))?;
        if profile.document.profile.mode != TravelMode::Foot {
            return Err(ApiError::bad_request(format!(
                "pedestrian_profile_id '{}' uses mode {:?}; network walking geometry requires a foot profile",
                profile_id, profile.document.profile.mode
            )));
        }
        return Ok(profile);
    }

    service
        .profiles
        .values()
        .find(|profile| profile.document.profile.mode == TravelMode::Foot)
        .ok_or_else(|| {
            ApiError::bad_request(
                "network walking geometry requires a loaded foot profile; start the API with --profile <pedestrian-profile> or pass pedestrian_profile_id".to_string(),
            )
        })
}

/// Routing engines used to replace straight-line street legs with network
/// geometry: a foot engine for walking legs and transfers, plus optional
/// per-mode engines for non-walk access and egress legs.
struct TransitStreetEngines {
    walk: (String, Arc<PreparedRoutingEngine>),
    access: HashMap<AccessMode, (String, Arc<PreparedRoutingEngine>)>,
    egress: HashMap<AccessMode, (String, Arc<PreparedRoutingEngine>)>,
}

fn street_travel_mode(mode: AccessMode) -> TravelMode {
    match mode {
        AccessMode::Walk => TravelMode::Foot,
        AccessMode::Bicycle => TravelMode::Bicycle,
        AccessMode::Car => TravelMode::Car,
    }
}

fn side_profile_ids(
    engines: &HashMap<AccessMode, (String, Arc<PreparedRoutingEngine>)>,
) -> Option<String> {
    let mut ids = engines
        .values()
        .map(|(profile_id, _)| profile_id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        None
    } else {
        Some(ids.join(","))
    }
}

fn resolve_transit_street_engines(
    service: &ServiceRuntime,
    pedestrian_profile_id: Option<&str>,
    access_profile_id: Option<&str>,
    egress_profile_id: Option<&str>,
    modes: &TransitModeOptions,
) -> Result<TransitStreetEngines, ApiError> {
    let walk_profile = resolve_transit_pedestrian_profile(service, pedestrian_profile_id)?;
    let access_modes = modes
        .validated_access_modes()
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let egress_modes = modes
        .validated_egress_modes()
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    Ok(TransitStreetEngines {
        walk: (
            walk_profile.document.profile.id.clone(),
            Arc::clone(&walk_profile.engine),
        ),
        access: resolve_street_side_engines(
            service,
            access_profile_id,
            &access_modes,
            "access_profile_id",
        )?,
        egress: resolve_street_side_engines(
            service,
            egress_profile_id,
            &egress_modes,
            "egress_profile_id",
        )?,
    })
}

fn resolve_street_side_engines(
    service: &ServiceRuntime,
    explicit_profile_id: Option<&str>,
    side_modes: &[AccessMode],
    field: &str,
) -> Result<HashMap<AccessMode, (String, Arc<PreparedRoutingEngine>)>, ApiError> {
    let mut engines = HashMap::new();
    if let Some(profile_id) = explicit_profile_id {
        let profile = resolve_profile(service, Some(profile_id))?;
        let matched = side_modes.iter().copied().find(|mode| {
            *mode != AccessMode::Walk && street_travel_mode(*mode) == profile.document.profile.mode
        });
        let Some(mode) = matched else {
            return Err(ApiError::bad_request(format!(
                "{field} '{}' uses mode {:?}, which does not match any non-walk mode in the request ({:?})",
                profile_id,
                profile.document.profile.mode,
                side_modes
                    .iter()
                    .map(|mode| mode.label())
                    .collect::<Vec<_>>()
            )));
        };
        engines.insert(
            mode,
            (
                profile.document.profile.id.clone(),
                Arc::clone(&profile.engine),
            ),
        );
    }
    for &mode in side_modes {
        if mode == AccessMode::Walk || engines.contains_key(&mode) {
            continue;
        }
        if let Some(profile) = service
            .profiles
            .values()
            .find(|profile| profile.document.profile.mode == street_travel_mode(mode))
        {
            engines.insert(
                mode,
                (
                    profile.document.profile.id.clone(),
                    Arc::clone(&profile.engine),
                ),
            );
        }
    }
    Ok(engines)
}

/// Prices transit access/egress legs with real network travel times using
/// the loaded street engines, for requests that opt into
/// `modes.street_access = "network"`. Unreachable pairs or modes without a
/// loaded profile return `None`, which makes that stop candidate unreachable.
/// Straight-line pricing is available only through explicit straight-line
/// street-access mode.
struct StreetEngineTimeEstimator<'a> {
    engines: &'a TransitStreetEngines,
    stop_candidates: Mutex<HashMap<(AccessMode, bool, String), Vec<SnappedPoint>>>,
}

impl<'a> StreetEngineTimeEstimator<'a> {
    fn new(engines: &'a TransitStreetEngines) -> Self {
        Self {
            engines,
            stop_candidates: Mutex::new(HashMap::new()),
        }
    }

    fn engine(
        &self,
        mode: AccessMode,
        egress: bool,
    ) -> Option<&(String, Arc<PreparedRoutingEngine>)> {
        if mode == AccessMode::Walk {
            Some(&self.engines.walk)
        } else if egress {
            self.engines.egress.get(&mode)
        } else {
            self.engines.access.get(&mode)
        }
    }

    fn candidates_for_stop(
        &self,
        mode: AccessMode,
        stop: &TransitStop,
        is_origin: bool,
    ) -> Option<Vec<SnappedPoint>> {
        let key = (mode, is_origin, stop.stop_id.clone());
        if let Some(cached) = self.stop_candidates.lock().ok()?.get(&key).cloned() {
            return (!cached.is_empty()).then_some(cached);
        }
        let (_, engine) = self.engine(mode, is_origin)?;
        let candidates = resolve_api_stop_candidates(engine, stop, is_origin).unwrap_or_default();
        self.stop_candidates
            .lock()
            .ok()?
            .insert(key, candidates.clone());
        (!candidates.is_empty()).then_some(candidates)
    }

    fn route_with_candidates(
        &self,
        engine: &PreparedRoutingEngine,
        route_id: &str,
        origin: LabeledPoint,
        destination: LabeledPoint,
        origin_candidates: &[SnappedPoint],
        destination_candidates: &[SnappedPoint],
    ) -> Option<TransitStreetPath> {
        let request = api_street_route_request(route_id, origin, destination, true);
        let route = engine
            .execute_route_between_candidates(&request, origin_candidates, destination_candidates)
            .ok()?;
        Some(api_route_to_street_path(route))
    }
}

impl StreetTimeEstimator for StreetEngineTimeEstimator<'_> {
    fn street_time_s(
        &self,
        mode: AccessMode,
        egress: bool,
        from_lon: f64,
        from_lat: f64,
        to_lon: f64,
        to_lat: f64,
    ) -> Option<u32> {
        let (_, engine) = self.engine(mode, egress)?;
        let request = api_street_route_request(
            "transit_street_access",
            LabeledPoint {
                id: "from".to_string(),
                lon: from_lon,
                lat: from_lat,
                z: None,
            },
            LabeledPoint {
                id: "to".to_string(),
                lon: to_lon,
                lat: to_lat,
                z: None,
            },
            false,
        );
        let route = engine.execute_route(&request).ok()?;
        Some((route.summary.total_travel_time_s.ceil() as u32).max(1))
    }

    fn point_to_stop_path(
        &self,
        mode: AccessMode,
        from_lon: f64,
        from_lat: f64,
        stop: &TransitStop,
    ) -> Option<TransitStreetPath> {
        let (_, engine) = self.engine(mode, false)?;
        let origin = LabeledPoint {
            id: "transit_access_origin".to_string(),
            lon: from_lon,
            lat: from_lat,
            z: None,
        };
        let destination = api_binding_point(stop);
        let origins = engine.snap_route_candidates(&origin, 500.0, true).ok()?;
        let destinations = self.candidates_for_stop(mode, stop, false)?;
        self.route_with_candidates(
            engine,
            "transit_bound_access",
            origin,
            destination,
            &origins,
            &destinations,
        )
    }

    fn stop_to_point_path(
        &self,
        mode: AccessMode,
        stop: &TransitStop,
        to_lon: f64,
        to_lat: f64,
    ) -> Option<TransitStreetPath> {
        let (_, engine) = self.engine(mode, true)?;
        let origin = api_binding_point(stop);
        let destination = LabeledPoint {
            id: "transit_egress_destination".to_string(),
            lon: to_lon,
            lat: to_lat,
            z: None,
        };
        let origins = self.candidates_for_stop(mode, stop, true)?;
        let destinations = engine
            .snap_route_candidates(&destination, 500.0, false)
            .ok()?;
        self.route_with_candidates(
            engine,
            "transit_bound_egress",
            origin,
            destination,
            &origins,
            &destinations,
        )
    }
}

fn resolve_api_stop_candidates(
    engine: &PreparedRoutingEngine,
    stop: &TransitStop,
    is_origin: bool,
) -> Option<Vec<SnappedPoint>> {
    let point = api_binding_point(stop);
    match stop.binding.as_ref() {
        Some(TransitStopBindingTarget::Node { node_id }) => {
            api_exact_node_candidate(engine.topology(), &point, *node_id).map(|value| vec![value])
        }
        Some(TransitStopBindingTarget::Edge { edge_id, fraction }) => {
            let topology = engine.topology();
            let edge_index = api_edge_index(topology, *edge_id)?;
            api_exact_edge_candidate(
                topology,
                &point,
                edge_index,
                fraction
                    .unwrap_or_else(|| api_projected_edge_fraction(topology, edge_index, &point)),
            )
            .map(|value| vec![value])
        }
        Some(TransitStopBindingTarget::Coordinate {
            z,
            z_window_m,
            attribute_filter,
            ..
        }) => {
            let mut candidates = engine
                .snap_route_candidates_with_options(
                    &point,
                    &SnapOptions {
                        max_distance_m: 500.0,
                        z_window_m: *z_window_m,
                        attribute_filters: attribute_filter.clone(),
                    },
                    is_origin,
                )
                .ok()?;
            if z.is_some() && z_window_m.is_none() {
                retain_api_nearest_z(&mut candidates, z.unwrap_or_default());
            }
            Some(candidates)
        }
        None => engine.snap_route_candidates(&point, 500.0, is_origin).ok(),
    }
}

fn retain_api_nearest_z(candidates: &mut Vec<SnappedPoint>, z: f64) {
    if let Some(nearest) = candidates
        .iter()
        .map(|candidate| (candidate.snapped_z - z).abs())
        .min_by(f64::total_cmp)
    {
        candidates.retain(|candidate| (candidate.snapped_z - z).abs() <= nearest + 1.0e-6);
    }
}

fn api_binding_point(stop: &TransitStop) -> LabeledPoint {
    match stop.binding.as_ref() {
        Some(TransitStopBindingTarget::Coordinate { lon, lat, z, .. }) => LabeledPoint {
            id: stop.stop_id.clone(),
            lon: *lon,
            lat: *lat,
            z: *z,
        },
        _ => LabeledPoint {
            id: stop.stop_id.clone(),
            lon: stop.lon,
            lat: stop.lat,
            z: None,
        },
    }
}

fn api_edge_index(topology: &TopologyBundle, edge_id: u32) -> Option<u32> {
    if (edge_id as usize) < topology.edge_count()
        && topology.routing_edge(edge_id as usize).edge_id.0 == edge_id
    {
        return Some(edge_id);
    }
    (0..topology.edge_count())
        .find(|&index| topology.routing_edge(index).edge_id.0 == edge_id)
        .map(|index| index as u32)
}

fn api_exact_node_candidate(
    topology: &TopologyBundle,
    point: &LabeledPoint,
    node_id: u32,
) -> Option<SnappedPoint> {
    let node = topology
        .nodes
        .get(node_id as usize)
        .filter(|node| node.node_id.0 == node_id)?;
    Some(SnappedPoint {
        point_id: point.id.clone(),
        requested_lon: point.lon,
        requested_lat: point.lat,
        snapped_node_id: node_id,
        snapped_lon: node.lon,
        snapped_lat: node.lat,
        snapped_z: node.elevation_m().unwrap_or_default(),
        snap_distance_m: 0.0,
        snapped_edge_id: None,
        snapped_edge_fraction: None,
        snapped_from_node_id: None,
        snapped_to_node_id: None,
        component_id: topology.node_component_id(node_id),
    })
}

fn api_exact_edge_candidate(
    topology: &TopologyBundle,
    point: &LabeledPoint,
    edge_index: u32,
    fraction: f64,
) -> Option<SnappedPoint> {
    if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
        return None;
    }
    let edge = topology.routing_edge(edge_index as usize);
    let from = &topology.nodes[edge.from.0 as usize];
    let to = &topology.nodes[edge.to.0 as usize];
    let z = match (from.elevation_m(), to.elevation_m()) {
        (Some(from), Some(to)) => from + (to - from) * fraction,
        _ => 0.0,
    };
    Some(SnappedPoint {
        point_id: point.id.clone(),
        requested_lon: point.lon,
        requested_lat: point.lat,
        snapped_node_id: if fraction <= 0.5 {
            edge.from.0
        } else {
            edge.to.0
        },
        snapped_lon: from.lon + (to.lon - from.lon) * fraction,
        snapped_lat: from.lat + (to.lat - from.lat) * fraction,
        snapped_z: z,
        snap_distance_m: 0.0,
        snapped_edge_id: Some(edge_index),
        snapped_edge_fraction: Some(fraction),
        snapped_from_node_id: Some(edge.from.0),
        snapped_to_node_id: Some(edge.to.0),
        component_id: topology.edge_component_id(edge_index),
    })
}

fn api_projected_edge_fraction(
    topology: &TopologyBundle,
    edge_index: u32,
    point: &LabeledPoint,
) -> f64 {
    let edge = topology.routing_edge(edge_index as usize);
    let from = &topology.nodes[edge.from.0 as usize];
    let to = &topology.nodes[edge.to.0 as usize];
    let cos_lat = from.lat.to_radians().cos().abs().max(0.01);
    let point_x = (point.lon - from.lon) * cos_lat;
    let point_y = point.lat - from.lat;
    let edge_x = (to.lon - from.lon) * cos_lat;
    let edge_y = to.lat - from.lat;
    let length_sq = edge_x.mul_add(edge_x, edge_y * edge_y);
    if length_sq <= f64::EPSILON {
        0.0
    } else {
        ((point_x * edge_x + point_y * edge_y) / length_sq).clamp(0.0, 1.0)
    }
}

fn api_street_route_request(
    route_id: &str,
    origin: LabeledPoint,
    destination: LabeledPoint,
    include_path: bool,
) -> RouteRequest {
    RouteRequest {
        route_id: route_id.to_string(),
        origin,
        destination,
        snap: Default::default(),
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: if include_path {
            ReturnConfig {
                geometry: ReturnGeometry::Full,
                segment_rows: true,
                ..ReturnConfig::default()
            }
        } else {
            ReturnConfig::default()
        },
        alternatives: Default::default(),
        temporal: Default::default(),
    }
}

fn api_route_to_street_path(route: netweevil_query::RouteResult) -> TransitStreetPath {
    TransitStreetPath {
        travel_time_s: (route.summary.total_travel_time_s.ceil() as u32).max(1),
        distance_m: Some(route.summary.network_distance_m as f64),
        edge_path: route.edge_path,
        geometry: route.geometry.unwrap_or_default(),
        components: route.summary.components,
    }
}

fn replace_transit_street_leg_geometries(
    result: &mut TransitRouteResult,
    engines: &TransitStreetEngines,
) {
    replace_transit_street_leg_geometries_for_legs(
        &result.route_id,
        &mut result.legs,
        &mut result.diagnostics,
        engines,
    );
    for alternative in &mut result.alternatives {
        replace_transit_street_leg_geometries_for_legs(
            &format!("{}_alternative_{}", result.route_id, alternative.rank),
            &mut alternative.legs,
            &mut result.diagnostics,
            engines,
        );
    }
}

fn replace_transit_street_leg_geometries_for_legs(
    route_id: &str,
    legs: &mut [TransitLeg],
    diagnostics: &mut Vec<String>,
    engines: &TransitStreetEngines,
) {
    for (index, leg) in legs.iter_mut().enumerate() {
        if !matches!(
            leg.leg_type,
            TransitLegType::Access | TransitLegType::Transfer | TransitLegType::Egress
        ) {
            continue;
        }
        if let Some(path) = leg.network_path.as_ref()
            && !path.geometry.is_empty()
        {
            leg.geometry = path.geometry.clone();
            continue;
        }
        let street_mode = leg.street_mode.unwrap_or(AccessMode::Walk);
        let engine_entry = if street_mode == AccessMode::Walk {
            Some(&engines.walk)
        } else if leg.leg_type == TransitLegType::Access {
            engines.access.get(&street_mode)
        } else {
            engines.egress.get(&street_mode)
        };
        let Some((profile_id, engine)) = engine_entry else {
            diagnostics.push(format!(
                "no loaded {:?} profile for {} leg {}; keeping straight-line geometry (load one with --profile or pass access_profile_id/egress_profile_id)",
                street_travel_mode(street_mode),
                street_mode.label(),
                index + 1
            ));
            continue;
        };
        let (Some(first), Some(last)) =
            (leg.geometry.first().copied(), leg.geometry.last().copied())
        else {
            diagnostics.push(format!(
                "network street geometry skipped for leg {} because straight-line endpoints were not returned",
                index + 1
            ));
            continue;
        };
        let route_request = RouteRequest {
            route_id: format!("{}_street_leg_{}", route_id, index + 1),
            origin: LabeledPoint {
                id: leg.from_id.clone(),
                lon: first[0],
                lat: first[1],
                z: None,
            },
            destination: LabeledPoint {
                id: leg.to_id.clone(),
                lon: last[0],
                lat: last[1],
                z: None,
            },
            snap: Default::default(),
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
            alternatives: Default::default(),
            temporal: Default::default(),
        };
        match engine.execute_route(&route_request) {
            Ok(route) => {
                if let Some(geometry) = route.geometry {
                    leg.geometry = geometry;
                }
            }
            Err(error) => {
                diagnostics.push(format!(
                    "network street geometry failed for leg {} using profile '{}': {}",
                    index + 1,
                    profile_id,
                    error
                ));
            }
        }
    }
}
