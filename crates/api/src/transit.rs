use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use netweevil_core::{TopologyBundle, TravelMode};
use netweevil_profile::{ReturnConfig, ReturnGeometry};
use netweevil_query::{
    LabeledPoint, PreparedRoutingEngine, RouteRequest, ServiceAreaReturnOptions, ServiceAreaSeed,
    SnapOptions, SnappedPoint,
};
use netweevil_transit::{
    AccessMode, StreetTimeEstimator, TransitCatchmentMode, TransitIsochroneFeature, TransitLeg,
    TransitLegType, TransitModeOptions, TransitPoint, TransitRouteResult,
    TransitServiceAreaRequest, TransitStop, TransitStopBindingTarget, TransitStreetAccessModel,
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

#[derive(Debug, serde::Deserialize)]
pub(crate) struct TransitBindingsQuery {
    #[serde(default)]
    pedestrian_profile_id: Option<String>,
    #[serde(default)]
    stop_id: Option<String>,
    #[serde(default = "default_binding_audit_limit")]
    limit: usize,
}

fn default_binding_audit_limit() -> usize {
    1000
}

/// Inspect explicit stop constraints against the currently loaded graph/profile.
/// Both directions are checked because one-way station access may differ.
pub(crate) async fn transit_bindings_handler(
    State(state): State<ApiState>,
    Path(feed_id): Path<String>,
    Query(query): Query<TransitBindingsQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let runtime = state.runtime()?;
    let feed = runtime
        .transit_feed(&feed_id)
        .ok_or_else(|| ApiError::not_found(format!("unknown transit feed '{feed_id}'")))?;
    let profile =
        resolve_transit_pedestrian_profile(&runtime, query.pedestrian_profile_id.as_deref())?;
    let profile_id = profile.document.profile.id.clone();
    let engine = Arc::clone(&profile.engine);
    let dataset_id = runtime.dataset_manifest.dataset_id.0.clone();
    let response = execute_on_routing_worker(&runtime, move || {
        let topology = engine.topology();
        let mut component_sizes = HashMap::<u32, usize>::new();
        for component in &topology.node_component_ids { *component_sizes.entry(*component).or_default() += 1; }
        let stops = feed.router.bundle().stops.iter().filter(|stop| stop.binding.is_some()
            && query.stop_id.as_ref().is_none_or(|id| &stop.stop_id == id)).collect::<Vec<_>>();
        let total = stops.len();
        let mut features = Vec::new();
        let mut failures = 0;
        for stop in stops.into_iter().take(query.limit.clamp(1, 10_000)) {
            let origins = resolve_api_stop_candidates(&engine, stop, true).unwrap_or_default();
            let destinations = resolve_api_stop_candidates(&engine, stop, false).unwrap_or_default();
            let resolved = !origins.is_empty() && !destinations.is_empty();
            if !resolved { failures += 1; }
            let status = match (origins.is_empty(), destinations.is_empty()) {
                (false, false) => "resolved",
                (true, false) => "no_departure_candidates",
                (false, true) => "no_arrival_candidates",
                (true, true) => "unresolved",
            };
            let candidates_json = |candidates: &[SnappedPoint]| candidates.iter().map(|candidate| serde_json::json!({
                "node_id": candidate.snapped_node_id,
                "edge_index": candidate.snapped_edge_id,
                "edge_fraction": candidate.snapped_edge_fraction,
                "coordinates": [candidate.snapped_lon, candidate.snapped_lat, candidate.snapped_z],
                "snap_distance_m": candidate.snap_distance_m,
                "component_id": candidate.component_id,
                "component_node_count": candidate.component_id.and_then(|id| component_sizes.get(&id).copied()),
            })).collect::<Vec<_>>();
            let coordinate = origins.first().or_else(|| destinations.first())
                .map(|candidate| [candidate.snapped_lon, candidate.snapped_lat, candidate.snapped_z])
                .or_else(|| bound_stop_coordinate(stop, topology));
            features.push(serde_json::json!({
                "type": "Feature",
                "geometry": coordinate.map(|coordinate| serde_json::json!({"type":"Point", "coordinates": coordinate})),
                "properties": {
                    "stop_id": stop.stop_id, "stop_name": stop.name,
                    "gtfs_lon": stop.lon, "gtfs_lat": stop.lat,
                    "binding": stop.binding, "resolution_status": status,
                    "origin_candidates": candidates_json(&origins),
                    "destination_candidates": candidates_json(&destinations),
                    "diagnostic": (!resolved).then_some("Explicit binding has no traversable candidate in one or both directions. Check the source attributes, elevation window, profile access and graph connectivity; no coordinate fallback is used."),
                }
            }));
        }
        Ok(serde_json::json!({
            "type": "FeatureCollection", "features": features,
            "metadata": {
                "feed_id": feed_id, "dataset_id": dataset_id, "profile_id": profile_id,
                "total_bound_stops": total, "checked_bound_stops": features.len(),
                "failed_bound_stops": failures, "truncated": features.len() < total,
                "connectivity_check": "weak_component_membership_only_not_a_route_proof",
            }
        }))
    }).await.map_err(ApiError::from_execution_error)?;
    Ok(Json(response))
}

pub(crate) async fn transit_route_handler(
    State(state): State<ApiState>,
    Json(payload): Json<TransitRouteExecutionRequest>,
) -> Result<Json<TransitRouteExecutionResponse>, ApiError> {
    let runtime = state.runtime()?;
    let feed = runtime.transit_feed(&payload.feed_id).ok_or_else(|| {
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
                runtime.as_ref(),
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
    let topology = runtime.topology.clone();
    let replace_geometry = matches!(walking_geometry, TransitWalkingGeometry::Network);
    let result = execute_on_routing_worker(runtime.as_ref(), move || {
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
            replace_transit_street_leg_geometries(
                &mut result,
                engines,
                &router.bundle().stops,
                &request,
            );
        }
        anchor_transit_platform_geometry(&mut result, router.bundle(), &topology);
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
            agency_timezone: manifest.agency_timezone,
            time_origin_unix_s: manifest.time_origin_unix_s,
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
    let runtime = state.runtime()?;
    let feed = runtime.transit_feed(&payload.feed_id).ok_or_else(|| {
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
    let isochrone = request.catchment_mode == TransitCatchmentMode::StreetIsochrone;
    let network_street_access = matches!(
        request.modes.street_access,
        TransitStreetAccessModel::Network
    );
    let street_engines = if network_street_access || isochrone {
        Some(resolve_transit_street_engines(
            runtime.as_ref(),
            payload.pedestrian_profile_id.as_deref(),
            payload.access_profile_id.as_deref(),
            payload.egress_profile_id.as_deref(),
            &request.modes,
        )?)
    } else {
        None
    };
    let pedestrian_profile_id = street_engines
        .as_ref()
        .map(|engines| engines.walk.0.clone());
    let access_profile_id = street_engines
        .as_ref()
        .and_then(|engines| side_profile_ids(&engines.access));
    let egress_profile_id = street_engines
        .as_ref()
        .and_then(|engines| side_profile_ids(&engines.egress));
    let result = execute_on_routing_worker(runtime.as_ref(), move || {
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
        agency_timezone: manifest.agency_timezone,
        time_origin_unix_s: manifest.time_origin_unix_s,
        route_engine: "scheduled_connection_scan_transit_service_area".to_string(),
        walking_geometry: if isochrone {
            "network"
        } else {
            "straight_line"
        }
        .to_string(),
        transfer_profile_id,
        pedestrian_profile_id,
        access_profile_id,
        egress_profile_id,
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
    fn requires_network_path(&self) -> bool {
        true
    }

    fn street_isochrone(
        &self,
        origin: &TransitPoint,
        stops: &[(&TransitStop, u32)],
        request: &TransitServiceAreaRequest,
    ) -> anyhow::Result<Vec<TransitIsochroneFeature>> {
        let reverse = request.time.arrive_by;
        let direct_modes = if reverse {
            request.modes.validated_egress_modes()?
        } else {
            request.modes.validated_access_modes()?
        };
        let stop_modes = if reverse {
            request.modes.validated_access_modes()?
        } else {
            request.modes.validated_egress_modes()?
        };
        let mut groups: Vec<(
            AccessMode,
            &str,
            &PreparedRoutingEngine,
            Vec<ServiceAreaSeed>,
        )> = Vec::new();
        for (egress, modes, direct) in
            [(reverse, direct_modes, true), (!reverse, stop_modes, false)]
        {
            for mode in modes {
                let Some((profile_id, engine)) = self.engine(mode, egress) else {
                    anyhow::bail!(
                        "street_isochrone requires a loaded {} profile",
                        mode.label()
                    );
                };
                let group_index = groups
                    .iter()
                    .position(|(known_mode, id, _, _)| *known_mode == mode && *id == profile_id)
                    .unwrap_or_else(|| {
                        groups.push((mode, profile_id, engine.as_ref(), Vec::new()));
                        groups.len() - 1
                    });
                let seeds = &mut groups[group_index].3;
                let mut add_candidates = |candidates: Vec<SnappedPoint>, elapsed: u32| {
                    seeds.extend(candidates.into_iter().map(|point| ServiceAreaSeed {
                        // Reaching the snapped street location also spends time.
                        initial_time_s: f64::from(elapsed)
                            + point.snap_distance_m / (mode.speed_kph(&request.modes) / 3.6),
                        point,
                    }));
                };
                if direct {
                    let point = LabeledPoint {
                        id: origin.id.clone(),
                        lon: origin.lon,
                        lat: origin.lat,
                        z: origin.z,
                    };
                    add_candidates(
                        api_endpoint_candidates(
                            engine,
                            &point,
                            !reverse,
                            request.modes.max_endpoint_snap_distance_m,
                        )
                        .unwrap_or_default(),
                        0,
                    );
                } else {
                    for &(stop, elapsed) in stops {
                        add_candidates(
                            self.candidates_for_stop(mode, stop, !reverse)
                                .unwrap_or_default(),
                            elapsed,
                        );
                    }
                }
            }
        }
        let mut features = Vec::new();
        for (mode, _, engine, seeds) in groups {
            let street_features = engine.execute_seeded_service_area(
                &origin.id,
                &seeds,
                f64::from(request.max_travel_time_s),
                reverse,
                &ServiceAreaReturnOptions {
                    geometry: request.returns.include_geometry,
                    max_geometry_points: request.returns.max_geometry_points,
                    ..Default::default()
                },
            )?;
            features.extend(street_features.into_iter().map(|feature| {
                TransitIsochroneFeature {
                    origin_id: origin.id.clone(),
                    mode,
                    geometry_type: match feature.geometry_type {
                        netweevil_query::ServiceAreaGeometryType::Polygon => "polygon",
                        _ => "network",
                    }
                    .to_string(),
                    threshold_limit: feature.threshold_limit,
                    threshold_metric: "travel_time_s".to_string(),
                    reachable_network_length_m: feature
                        .reachable_network_length_m
                        .unwrap_or_default(),
                    reachable_edge_count: feature.reachable_edge_count.unwrap_or_default(),
                    geometry: feature.geometry,
                }
            }));
        }
        Ok(features)
    }

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

    fn point_to_stop_path_with_snap_limit(
        &self,
        mode: AccessMode,
        from: &TransitPoint,
        stop: &TransitStop,
        max_endpoint_snap_distance_m: f64,
    ) -> Option<TransitStreetPath> {
        let (_, engine) = self.engine(mode, false)?;
        let origin = LabeledPoint {
            id: "transit_access_origin".to_string(),
            lon: from.lon,
            lat: from.lat,
            z: from.z,
        };
        let destination = api_binding_point(stop);
        let origins = api_endpoint_candidates(engine, &origin, true, max_endpoint_snap_distance_m)?;
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

    fn stop_to_point_path_with_snap_limit(
        &self,
        mode: AccessMode,
        stop: &TransitStop,
        to: &TransitPoint,
        max_endpoint_snap_distance_m: f64,
    ) -> Option<TransitStreetPath> {
        let (_, engine) = self.engine(mode, true)?;
        let origin = api_binding_point(stop);
        let destination = LabeledPoint {
            id: "transit_egress_destination".to_string(),
            lon: to.lon,
            lat: to.lat,
            z: to.z,
        };
        let origins = self.candidates_for_stop(mode, stop, true)?;
        let destinations =
            api_endpoint_candidates(engine, &destination, false, max_endpoint_snap_distance_m)?;
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

fn api_endpoint_candidates(
    engine: &PreparedRoutingEngine,
    point: &LabeledPoint,
    is_origin: bool,
    max_endpoint_snap_distance_m: f64,
) -> Option<Vec<SnappedPoint>> {
    engine
        .snap_route_candidates_with_options(
            point,
            &SnapOptions {
                max_distance_m: max_endpoint_snap_distance_m,
                z_window_m: point.z.map(|_| 1.0),
                ..SnapOptions::default()
            },
            is_origin,
        )
        .ok()
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
                        point_constraints: Default::default(),
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
    stops: &[TransitStop],
    request: &netweevil_transit::TransitRouteRequest,
) {
    replace_transit_street_leg_geometries_for_legs(
        &result.route_id,
        &mut result.legs,
        &mut result.diagnostics,
        engines,
        stops,
        request,
    );
    for alternative in &mut result.alternatives {
        replace_transit_street_leg_geometries_for_legs(
            &format!("{}_alternative_{}", result.route_id, alternative.rank),
            &mut alternative.legs,
            &mut result.diagnostics,
            engines,
            stops,
            request,
        );
    }
}

fn replace_transit_street_leg_geometries_for_legs(
    route_id: &str,
    legs: &mut [TransitLeg],
    diagnostics: &mut Vec<String>,
    engines: &TransitStreetEngines,
    stops: &[TransitStop],
    request: &netweevil_transit::TransitRouteRequest,
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
                z: if leg.leg_type == TransitLegType::Access {
                    request.origin.z
                } else {
                    None
                },
            },
            destination: LabeledPoint {
                id: leg.to_id.clone(),
                lon: last[0],
                lat: last[1],
                z: if leg.leg_type == TransitLegType::Egress {
                    request.destination.z
                } else {
                    None
                },
            },
            snap: SnapOptions {
                max_distance_m: request.modes.max_endpoint_snap_distance_m,
                z_window_m: Some(1.0),
                ..SnapOptions::default()
            },
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
            alternatives: Default::default(),
            temporal: Default::default(),
        };
        // Geometry-only network requests must honor the same platform constraints
        // as network-priced access. A fresh XY snap can jump to another floor.
        let from_stop = (leg.leg_type != TransitLegType::Access)
            .then(|| stops.iter().find(|stop| stop.stop_id == leg.from_id))
            .flatten();
        let to_stop = (leg.leg_type != TransitLegType::Egress)
            .then(|| stops.iter().find(|stop| stop.stop_id == leg.to_id))
            .flatten();
        let bound = from_stop.is_some_and(|stop| stop.binding.is_some())
            || to_stop.is_some_and(|stop| stop.binding.is_some());
        let route = if bound {
            let origins = from_stop.map_or_else(
                || {
                    api_endpoint_candidates(
                        engine,
                        &route_request.origin,
                        true,
                        request.modes.max_endpoint_snap_distance_m,
                    )
                },
                |stop| resolve_api_stop_candidates(engine, stop, true),
            );
            let destinations = to_stop.map_or_else(
                || {
                    api_endpoint_candidates(
                        engine,
                        &route_request.destination,
                        false,
                        request.modes.max_endpoint_snap_distance_m,
                    )
                },
                |stop| resolve_api_stop_candidates(engine, stop, false),
            );
            match origins.zip(destinations) {
                Some((origins, destinations)) => {
                    engine.execute_route_between_candidates(&route_request, &origins, &destinations)
                }
                None => Err(anyhow::anyhow!(
                    "platform binding has no feasible network candidates"
                )),
            }
        } else {
            engine.execute_route(&route_request)
        };
        match route {
            Ok(route) => {
                if let Some(geometry) = route.geometry {
                    leg.geometry = geometry;
                    leg.geometry_elevation_source = Some("network_source_z".into());
                }
            }
            Err(error) => {
                if bound
                    || route_request.origin.z.is_some()
                    || route_request.destination.z.is_some()
                {
                    leg.geometry.clear();
                }
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

/// Bind vehicle display geometry to the same platform coordinates as network
/// access. GTFS supplies only XY shapes; interpolated Z is labelled explicitly.
fn anchor_transit_platform_geometry(
    result: &mut TransitRouteResult,
    bundle: &netweevil_transit::TransitBundle,
    topology: &TopologyBundle,
) {
    let coordinates: HashMap<&str, [f64; 3]> = bundle
        .stops
        .iter()
        .filter_map(|stop| {
            bound_stop_coordinate(stop, topology).map(|point| (stop.stop_id.as_str(), point))
        })
        .collect();
    for leg in result.legs.iter_mut().chain(
        result
            .alternatives
            .iter_mut()
            .flat_map(|alternative| &mut alternative.legs),
    ) {
        if leg.leg_type == TransitLegType::Transit {
            leg.geometry_elevation_source = anchor_geometry(
                &mut leg.geometry,
                coordinates.get(leg.from_id.as_str()),
                coordinates.get(leg.to_id.as_str()),
            );
        } else if leg
            .network_path
            .as_ref()
            .is_some_and(|path| !path.geometry.is_empty())
        {
            leg.geometry_elevation_source = Some("network_source_z".into());
        }
    }
    for stop in result.stops.iter_mut().chain(
        result
            .alternatives
            .iter_mut()
            .flat_map(|alternative| &mut alternative.stops),
    ) {
        if let Some(point) = coordinates.get(stop.stop_id.as_str()) {
            stop.lon = point[0];
            stop.lat = point[1];
            stop.z = Some(point[2]);
        }
    }
    for segment in result.stop_segments.iter_mut().chain(
        result
            .alternatives
            .iter_mut()
            .flat_map(|alternative| &mut alternative.stop_segments),
    ) {
        segment.geometry_elevation_source = anchor_geometry(
            &mut segment.geometry,
            coordinates.get(segment.from_stop_id.as_str()),
            coordinates.get(segment.to_stop_id.as_str()),
        );
    }
    rebuild_transit_leg_geometry(&mut result.legs, &result.stop_segments);
    for alternative in &mut result.alternatives {
        rebuild_transit_leg_geometry(&mut alternative.legs, &alternative.stop_segments);
    }
}

/// Stop segments retain intermediate platform anchors lost when a whole vehicle
/// ride is coalesced into a single leg. Reuse that verified chain when returned.
fn rebuild_transit_leg_geometry(
    legs: &mut [TransitLeg],
    segments: &[netweevil_transit::TransitRouteStopSegment],
) {
    for leg in legs
        .iter_mut()
        .filter(|leg| leg.leg_type == TransitLegType::Transit)
    {
        let chain: Vec<_> = segments
            .iter()
            .filter(|segment| {
                segment.trip_id == leg.trip_id
                    && segment.departure_s >= leg.departure_s
                    && segment.arrival_s <= leg.arrival_s
            })
            .collect();
        if chain.is_empty()
            || chain
                .first()
                .is_none_or(|segment| segment.from_stop_id != leg.from_id)
            || chain
                .last()
                .is_none_or(|segment| segment.to_stop_id != leg.to_id)
            || chain.iter().any(|segment| segment.geometry.len() < 2)
            || !chain
                .windows(2)
                .all(|pair| pair[0].to_stop_id == pair[1].from_stop_id)
        {
            continue;
        }
        let mut geometry = Vec::new();
        for segment in &chain {
            for point in &segment.geometry {
                if geometry.last() != Some(point) {
                    geometry.push(*point);
                }
            }
        }
        leg.geometry = geometry;
        leg.geometry_elevation_source = if chain
            .iter()
            .all(|segment| segment.geometry_elevation_source == chain[0].geometry_elevation_source)
        {
            chain[0].geometry_elevation_source.clone()
        } else {
            Some("mixed_stop_segment_elevations".into())
        };
    }
}

fn bound_stop_coordinate(stop: &TransitStop, topology: &TopologyBundle) -> Option<[f64; 3]> {
    match stop.binding.as_ref()? {
        TransitStopBindingTarget::Coordinate {
            lon,
            lat,
            z: Some(z),
            ..
        } if z.is_finite() => Some([*lon, *lat, *z]),
        TransitStopBindingTarget::Node { node_id } => {
            let node = topology.nodes.get(*node_id as usize)?;
            Some([node.lon, node.lat, node.elevation_m()?])
        }
        TransitStopBindingTarget::Edge { edge_id, fraction } => {
            let edge_index = api_edge_index(topology, *edge_id)?;
            let point = api_binding_point(stop);
            let edge = topology.routing_edge(edge_index as usize);
            topology.nodes[edge.from.0 as usize].elevation_m()?;
            topology.nodes[edge.to.0 as usize].elevation_m()?;
            let candidate = api_exact_edge_candidate(
                topology,
                &point,
                edge_index,
                fraction
                    .unwrap_or_else(|| api_projected_edge_fraction(topology, edge_index, &point)),
            )?;
            Some([
                candidate.snapped_lon,
                candidate.snapped_lat,
                candidate.snapped_z,
            ])
        }
        _ => None,
    }
}

fn anchor_geometry(
    geometry: &mut [[f64; 3]],
    from: Option<&[f64; 3]>,
    to: Option<&[f64; 3]>,
) -> Option<String> {
    if geometry.len() < 2 {
        return None;
    }
    if let Some(from) = from {
        geometry[0] = *from;
    }
    if let Some(to) = to {
        let last = geometry.len() - 1;
        geometry[last] = *to;
    }
    if let (Some(from), Some(to)) = (from, to) {
        let distances: Vec<f64> = geometry
            .windows(2)
            .map(|pair| {
                netweevil_core::geo::haversine_meters(
                    pair[0][0], pair[0][1], pair[1][0], pair[1][1],
                )
            })
            .collect();
        let total = distances.iter().sum::<f64>();
        let last = geometry.len() - 1;
        let mut elapsed = 0.0;
        for (index, point) in geometry.iter_mut().enumerate() {
            if index > 0 {
                elapsed += distances[index - 1];
            }
            let fraction = if total > 0.0 {
                elapsed / total
            } else {
                index as f64 / last as f64
            };
            point[2] = from[2] + (to[2] - from[2]) * fraction;
        }
        Some("interpolated_between_stop_bindings".into())
    } else if from.is_some() || to.is_some() {
        Some("bound_endpoint_only_other_elevations_unknown".into())
    } else {
        Some("unknown_gtfs_elevation".into())
    }
}

#[cfg(test)]
mod platform_geometry_tests {
    use super::*;

    #[test]
    fn main_vehicle_leg_keeps_intermediate_platform_elevation() {
        let mut legs: Vec<TransitLeg> = serde_json::from_value(serde_json::json!([{
            "leg_type": "transit", "from_id": "A", "to_id": "C", "from_name": "A", "to_name": "C",
            "departure_s": 0, "arrival_s": 20, "trip_id": "T", "geometry": [[0.0,0.0,-20.0],[2.0,0.0,-20.0]]
        }])).unwrap();
        let segments: Vec<netweevil_transit::TransitRouteStopSegment> = serde_json::from_value(serde_json::json!([
            {"segment_index":1,"from_stop_id":"A","to_stop_id":"B","from_stop_name":"A","to_stop_name":"B",
             "departure_s":0,"arrival_s":10,"duration_s":10,"trip_id":"T","geometry":[[0.0,0.0,-20.0],[1.0,0.0,-5.0]],
             "geometry_elevation_source":"interpolated_between_stop_bindings"},
            {"segment_index":2,"from_stop_id":"B","to_stop_id":"C","from_stop_name":"B","to_stop_name":"C",
             "departure_s":10,"arrival_s":20,"duration_s":10,"trip_id":"T","geometry":[[1.0,0.0,-5.0],[2.0,0.0,-20.0]],
             "geometry_elevation_source":"interpolated_between_stop_bindings"}
        ])).unwrap();
        rebuild_transit_leg_geometry(&mut legs, &segments);
        assert_eq!(
            legs[0].geometry,
            vec![[0.0, 0.0, -20.0], [1.0, 0.0, -5.0], [2.0, 0.0, -20.0]]
        );
        assert_eq!(
            legs[0].geometry_elevation_source.as_deref(),
            Some("interpolated_between_stop_bindings")
        );
    }

    #[test]
    fn rail_display_joins_platforms_and_labels_interpolated_elevations() {
        let mut geometry = vec![
            [114.0, 22.0, 0.0],
            [114.005, 22.0, 0.0],
            [114.01, 22.0, 0.0],
        ];
        let from = [114.0, 22.0, -20.0];
        let to = [114.01, 22.0, -10.0];
        let source = anchor_geometry(&mut geometry, Some(&from), Some(&to));
        assert_eq!(geometry[0], from);
        assert_eq!(geometry[2], to);
        assert!((geometry[1][2] + 15.0).abs() < 1e-6);
        assert_eq!(
            source.as_deref(),
            Some("interpolated_between_stop_bindings")
        );
    }

    #[test]
    fn partial_binding_keeps_unknown_elevations_explicit() {
        let mut geometry = vec![[114.0, 22.0, 0.0], [114.01, 22.0, 0.0]];
        let source = anchor_geometry(&mut geometry, Some(&[114.001, 22.0, -15.0]), None);
        assert_eq!(geometry[0], [114.001, 22.0, -15.0]);
        assert_eq!(geometry[1][2], 0.0);
        assert_eq!(
            source.as_deref(),
            Some("bound_endpoint_only_other_elevations_unknown")
        );
    }

    #[test]
    fn coincident_platform_points_do_not_produce_nan() {
        let mut geometry = vec![[114.0, 22.0, 0.0]; 3];
        anchor_geometry(
            &mut geometry,
            Some(&[114.0, 22.0, -20.0]),
            Some(&[114.0, 22.0, -10.0]),
        );
        assert_eq!(geometry[1][2], -15.0);
    }
}
