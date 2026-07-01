use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use netweevil_core::TravelMode;
use netweevil_profile::{ReturnConfig, ReturnGeometry};
use netweevil_query::{LabeledPoint, PreparedRoutingEngine, RouteRequest};
use netweevil_transit::{
    AccessMode, StreetTimeEstimator, TransitLeg, TransitLegType, TransitModeOptions,
    TransitRouteResult, TransitStreetAccessModel, TransitWalkingGeometry,
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
            .map(|engines| StreetEngineTimeEstimator { engines });
        let mut result = router.execute_route_with_street_estimator(
            &request,
            estimator
                .as_ref()
                .map(|estimator| estimator as &dyn StreetTimeEstimator),
        )?;
        if replace_geometry {
            if let Some(engines) = street_engines.as_ref() {
                replace_transit_street_leg_geometries(&mut result, engines);
            }
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
    let network_street_access = matches!(
        payload.request.modes.street_access,
        TransitStreetAccessModel::Network
    );
    let street_engines = if network_street_access {
        Some(resolve_transit_street_engines(
            state.service.as_ref(),
            None,
            None,
            None,
            &payload.request.modes,
        )?)
    } else {
        None
    };
    let result = execute_on_routing_worker(state.service.as_ref(), move || {
        let estimator = street_engines
            .as_ref()
            .map(|engines| StreetEngineTimeEstimator { engines });
        router.execute_service_area_with_street_estimator(
            &payload.request,
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
/// loaded profile return `None`, which the transit search treats as a
/// straight-line fallback per candidate.
struct StreetEngineTimeEstimator<'a> {
    engines: &'a TransitStreetEngines,
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
        let (_, engine) = if mode == AccessMode::Walk {
            &self.engines.walk
        } else if egress {
            self.engines.egress.get(&mode)?
        } else {
            self.engines.access.get(&mode)?
        };
        let request = RouteRequest {
            route_id: "transit_street_access".to_string(),
            origin: LabeledPoint {
                id: "from".to_string(),
                lon: from_lon,
                lat: from_lat,
            },
            destination: LabeledPoint {
                id: "to".to_string(),
                lon: to_lon,
                lat: to_lat,
            },
            snap: Default::default(),
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig::default(),
            alternatives: Default::default(),
        };
        let route = engine.execute_route(&request).ok()?;
        Some((route.summary.total_travel_time_s.ceil() as u32).max(1))
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
            },
            destination: LabeledPoint {
                id: leg.to_id.clone(),
                lon: last[0],
                lat: last[1],
            },
            snap: Default::default(),
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: ReturnConfig {
                geometry: ReturnGeometry::Full,
                ..ReturnConfig::default()
            },
            alternatives: Default::default(),
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
