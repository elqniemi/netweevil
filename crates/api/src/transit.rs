use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use netweevil_core::TravelMode;
use netweevil_profile::{ReturnConfig, ReturnGeometry};
use netweevil_query::{LabeledPoint, PreparedRoutingEngine, RouteRequest};
use netweevil_transit::{TransitLeg, TransitLegType, TransitRouteResult, TransitWalkingGeometry};
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
    let pedestrian_profile = if matches!(walking_geometry, TransitWalkingGeometry::Network) {
        request.returns.include_geometry = true;
        Some(resolve_transit_pedestrian_profile(
            state.service.as_ref(),
            payload.pedestrian_profile_id.as_deref(),
        )?)
    } else {
        None
    };
    let router = Arc::clone(&feed.router);
    let manifest = feed.manifest.clone();
    let route_id = request.route_id.clone();
    let pedestrian_profile_id =
        pedestrian_profile.map(|profile| profile.document.profile.id.clone());
    let walk_engine = pedestrian_profile.map(|profile| {
        (
            profile.document.profile.id.clone(),
            Arc::clone(&profile.engine),
        )
    });
    let result = execute_on_routing_worker(state.service.as_ref(), move || {
        let mut result = router.execute_route(&request)?;
        if let Some((profile_id, engine)) = walk_engine.as_ref() {
            replace_transit_walking_leg_geometries(&mut result, engine, profile_id);
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
            route_engine: "scheduled_connection_scan_pedestrian_transit".to_string(),
            walking_geometry: match walking_geometry {
                TransitWalkingGeometry::StraightLine => "straight_line".to_string(),
                TransitWalkingGeometry::Network => "network".to_string(),
            },
            pedestrian_profile_id,
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
    let result = execute_on_routing_worker(state.service.as_ref(), move || {
        router.execute_service_area(&payload.request)
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

fn replace_transit_walking_leg_geometries(
    result: &mut TransitRouteResult,
    pedestrian_engine: &PreparedRoutingEngine,
    pedestrian_profile_id: &str,
) {
    replace_transit_walking_leg_geometries_for_legs(
        &result.route_id,
        &mut result.legs,
        &mut result.diagnostics,
        pedestrian_engine,
        pedestrian_profile_id,
    );
    for alternative in &mut result.alternatives {
        replace_transit_walking_leg_geometries_for_legs(
            &format!("{}_alternative_{}", result.route_id, alternative.rank),
            &mut alternative.legs,
            &mut result.diagnostics,
            pedestrian_engine,
            pedestrian_profile_id,
        );
    }
}

fn replace_transit_walking_leg_geometries_for_legs(
    route_id: &str,
    legs: &mut [TransitLeg],
    diagnostics: &mut Vec<String>,
    pedestrian_engine: &PreparedRoutingEngine,
    pedestrian_profile_id: &str,
) {
    for (index, leg) in legs.iter_mut().enumerate() {
        if !matches!(
            leg.leg_type,
            TransitLegType::Access | TransitLegType::Transfer | TransitLegType::Egress
        ) {
            continue;
        }
        let (Some(first), Some(last)) =
            (leg.geometry.first().copied(), leg.geometry.last().copied())
        else {
            diagnostics.push(format!(
                "network walking geometry skipped for leg {} because straight-line endpoints were not returned",
                index + 1
            ));
            continue;
        };
        let route_request = RouteRequest {
            route_id: format!("{}_walk_leg_{}", route_id, index + 1),
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
        match pedestrian_engine.execute_route(&route_request) {
            Ok(route) => {
                if let Some(geometry) = route.geometry {
                    leg.geometry = geometry;
                }
            }
            Err(error) => {
                diagnostics.push(format!(
                    "network walking geometry failed for leg {} using profile '{}': {}",
                    index + 1,
                    pedestrian_profile_id,
                    error
                ));
            }
        }
    }
}
