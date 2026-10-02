//! Journey instructions derived from the same scheduled route used by the map.
use std::collections::BTreeMap;

use axum::{Json, extract::State};
use netweevil_core::EdgeNameBundle;
use netweevil_query::{
    DirectionsContext, DirectionsOptions, EngineMode, LabeledPoint, Phrasebook, RouteRequest,
    SnapOptions, StreetLegMode, TransitStep,
};
use netweevil_transit::{
    AccessMode, GtfsStopContext, TransitLeg, TransitLegType, TransitOutcome, TransitRouteResult,
    TransitTimeContext, read_gtfs_stop_context,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::dto::{TransitDirectionsExecutionRequest, TransitExecutionContext};
use crate::error::ApiError;
use crate::state::{ApiState, LoadedProfile, execute_on_routing_worker, load_edge_names};
use crate::transit::transit_route_handler;

#[derive(Debug, Serialize)]
pub(crate) struct TransitDirection {
    sequence: usize,
    kind: String,
    leg_index: usize,
    instruction: String,
    departure_s: u32,
    arrival_s: u32,
    duration_s: u32,
    departure_datetime: String,
    arrival_datetime: String,
    from_id: String,
    to_id: String,
    from_name: String,
    to_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    route_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    route_short_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    headsign: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transit_mode: Option<netweevil_transit::TransitMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    street_mode: Option<AccessMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    walking_maneuvers: Vec<Value>,
}

#[derive(Serialize)]
pub(crate) struct TransitDirectionsResponse {
    service: TransitExecutionContext,
    result: TransitRouteResult,
    /// Language of the instructions.
    language: String,
    directions: Vec<TransitDirection>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    directions_diagnostics: Vec<String>,
}

pub(crate) async fn transit_directions_handler(
    State(state): State<ApiState>,
    Json(payload): Json<TransitDirectionsExecutionRequest>,
) -> Result<Json<TransitDirectionsResponse>, ApiError> {
    let TransitDirectionsExecutionRequest {
        route: mut payload,
        directions_options,
    } = payload;
    // Geometry and stop returns are needed to place instructions. Requested
    // modes, routing model, endpoint constraints and timetable remain intact.
    payload.request.returns.include_geometry = true;
    payload.request.returns.include_stops = true;
    payload.request.returns.include_stop_segments = true;
    let runtime = state.runtime()?;
    let feed = runtime.transit_feed(&payload.feed_id).ok_or_else(|| {
        ApiError::not_found(format!("unknown transit feed '{}'", payload.feed_id))
    })?;
    let source_path = feed.manifest.source_path.clone();
    let Json(response) = transit_route_handler(State(state), Json(payload)).await?;
    let worker_runtime = runtime.clone();
    let response = execute_on_routing_worker(runtime.as_ref(), move || {
        let mut diagnostics = Vec::new();
        let (phrasebook, warning) =
            Phrasebook::for_language(directions_options.language.as_deref());
        diagnostics.extend(warning);
        let contexts = read_gtfs_stop_context(&source_path).unwrap_or_else(|error| {
            diagnostics.push(format!(
                "Station/platform display metadata unavailable; using stored stop names: {error}"
            ));
            BTreeMap::new()
        });
        let edge_names = load_edge_names(&worker_runtime).ok();
        let no_names = EdgeNameBundle::default();
        let names = edge_names.as_deref().unwrap_or(&no_names);
        let walking = response
            .result
            .legs
            .iter()
            .enumerate()
            .map(|(index, leg)| {
                if leg.leg_type != TransitLegType::Transit && leg.network_path.is_none() {
                    diagnostics.push(format!("Leg {index}: no routed street path was returned; this step describes the street-access estimate and has no turn instructions."));
                }
                let profile_id = if leg.street_mode.unwrap_or(AccessMode::Walk) == AccessMode::Walk
                {
                    if leg.leg_type == TransitLegType::Transfer {
                        response
                            .service
                            .transfer_profile_id
                            .as_ref()
                            .or(response.service.pedestrian_profile_id.as_ref())
                    } else {
                        response.service.pedestrian_profile_id.as_ref()
                    }
                } else if leg.leg_type == TransitLegType::Egress {
                    response.service.egress_profile_id.as_ref()
                } else {
                    response.service.access_profile_id.as_ref()
                };
                profile_id
                    .and_then(|ids| ids.split(',').filter_map(|id| worker_runtime.profiles.get(id)).find(|profile| {
                        profile.document.profile.mode == match leg.street_mode.unwrap_or(AccessMode::Walk) {
                            AccessMode::Walk => netweevil_core::TravelMode::Foot,
                            AccessMode::Bicycle => netweevil_core::TravelMode::Bicycle,
                            AccessMode::Car => netweevil_core::TravelMode::Car,
                        }
                    }))
                    .map(|profile| {
                        walking_maneuvers(
                            profile,
                            leg,
                            names,
                            &directions_options,
                            index,
                            &mut diagnostics,
                        )
                    })
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        let directions = build_directions(&response.result, &contexts, &walking, phrasebook)?;
        Ok(TransitDirectionsResponse {
            service: response.service,
            result: response.result,
            language: phrasebook.language().to_string(),
            directions,
            directions_diagnostics: diagnostics,
        })
    })
    .await
    .map_err(ApiError::from_execution_error)?;
    Ok(Json(response))
}

fn stop_name(id: &str, fallback: &str, contexts: &BTreeMap<String, GtfsStopContext>) -> String {
    let Some(context) = contexts.get(id) else {
        return fallback.to_string();
    };
    match context
        .station_name
        .as_ref()
        .filter(|station| *station != &context.stop_name)
    {
        Some(station) => format!("{station}, {}", context.stop_name),
        None => context.stop_name.clone(),
    }
}

fn direction(
    kind: &str,
    index: usize,
    instruction: String,
    leg: &TransitLeg,
    start: u32,
    end: u32,
    context: &TransitTimeContext,
    stops: &BTreeMap<String, GtfsStopContext>,
) -> anyhow::Result<TransitDirection> {
    let at_end = kind == "alight";
    let point = if at_end {
        leg.geometry.last()
    } else {
        leg.geometry.first()
    };
    let stop_id = if at_end || kind == "access" {
        &leg.to_id
    } else {
        &leg.from_id
    };
    Ok(TransitDirection {
        sequence: 0,
        kind: kind.into(),
        leg_index: index,
        instruction,
        departure_s: start,
        arrival_s: end,
        duration_s: end.saturating_sub(start),
        departure_datetime: context.datetime(start)?,
        arrival_datetime: context.datetime(end)?,
        from_id: leg.from_id.clone(),
        to_id: leg.to_id.clone(),
        from_name: stop_name(&leg.from_id, &leg.from_name, stops),
        to_name: stop_name(&leg.to_id, &leg.to_name, stops),
        route_id: leg.route_id.clone(),
        route_short_name: leg.route_short_name.clone(),
        headsign: leg.headsign.clone(),
        transit_mode: leg.mode,
        street_mode: leg.street_mode,
        platform: stops
            .get(stop_id)
            .and_then(|stop| stop.platform_code.clone()),
        location: point
            .copied()
            .filter(|p| p.iter().all(|value| value.is_finite())),
        walking_maneuvers: Vec::new(),
    })
}

fn build_directions(
    result: &TransitRouteResult,
    stops: &BTreeMap<String, GtfsStopContext>,
    walking: &[Vec<Value>],
    phrasebook: Phrasebook,
) -> anyhow::Result<Vec<TransitDirection>> {
    if result.outcome != TransitOutcome::Scheduled {
        return Ok(Vec::new());
    }
    let mut steps = Vec::new();
    let mut previous_arrival = result.summary.departure_s;
    for (index, leg) in result.legs.iter().enumerate() {
        let from = stop_name(&leg.from_id, &leg.from_name, stops);
        let to = stop_name(&leg.to_id, &leg.to_name, stops);
        if leg.leg_type == TransitLegType::Transit {
            let mode = serde_json::to_value(leg.mode)?
                .as_str()
                .unwrap_or("transit")
                .to_string();
            let line = leg
                .route_short_name
                .as_deref()
                .or(leg.route_id.as_deref())
                .unwrap_or(&mode);
            let headsign = leg.headsign.as_deref().filter(|value| !value.is_empty());
            if leg.departure_s > previous_arrival {
                steps.push(direction(
                    "wait",
                    index,
                    phrasebook.transit_step(TransitStep::Wait {
                        stop: &from,
                        line: Some(line),
                    }),
                    leg,
                    previous_arrival,
                    leg.departure_s,
                    &result.time_context,
                    stops,
                )?);
            }
            steps.push(direction(
                "board",
                index,
                phrasebook.transit_step(TransitStep::Board {
                    line,
                    stop: &from,
                    headsign,
                }),
                leg,
                leg.departure_s,
                leg.departure_s,
                &result.time_context,
                stops,
            )?);
            steps.push(direction(
                "ride",
                index,
                phrasebook.transit_step(TransitStep::Ride {
                    line,
                    from: &from,
                    to: &to,
                }),
                leg,
                leg.departure_s,
                leg.arrival_s,
                &result.time_context,
                stops,
            )?);
            steps.push(direction(
                "alight",
                index,
                phrasebook.transit_step(TransitStep::Alight { stop: &to }),
                leg,
                leg.arrival_s,
                leg.arrival_s,
                &result.time_context,
                stops,
            )?);
        } else {
            if leg.departure_s > previous_arrival {
                steps.push(direction(
                    "wait",
                    index,
                    phrasebook.transit_step(TransitStep::Wait {
                        stop: &from,
                        line: None,
                    }),
                    leg,
                    previous_arrival,
                    leg.departure_s,
                    &result.time_context,
                    stops,
                )?);
            }
            let kind = match leg.leg_type {
                TransitLegType::Access => "access",
                TransitLegType::Transfer => "transfer",
                TransitLegType::Egress => "egress",
                TransitLegType::Transit => unreachable!(),
            };
            let mode = match leg.street_mode.unwrap_or(AccessMode::Walk) {
                AccessMode::Walk => StreetLegMode::Walk,
                AccessMode::Bicycle => StreetLegMode::Bicycle,
                AccessMode::Car => StreetLegMode::Car,
            };
            let mut step = direction(
                kind,
                index,
                phrasebook.transit_step(TransitStep::Street {
                    mode,
                    from: &from,
                    to: &to,
                }),
                leg,
                leg.departure_s,
                leg.arrival_s,
                &result.time_context,
                stops,
            )?;
            step.walking_maneuvers = walking.get(index).cloned().unwrap_or_default();
            steps.push(step);
        }
        previous_arrival = leg.arrival_s;
    }
    for (index, step) in steps.iter_mut().enumerate() {
        step.sequence = index + 1;
    }
    Ok(steps)
}

fn walking_maneuvers(
    profile: &LoadedProfile,
    leg: &TransitLeg,
    names: &EdgeNameBundle,
    options: &DirectionsOptions,
    leg_index: usize,
    diagnostics: &mut Vec<String>,
) -> Vec<Value> {
    let Some(path) = leg
        .network_path
        .as_ref()
        .filter(|path| !path.edge_path.is_empty())
    else {
        return Vec::new();
    };
    let Some((first, last)) = path.geometry.first().zip(path.geometry.last()) else {
        return Vec::new();
    };
    let point = |id: &str, p: &[f64; 3]| LabeledPoint {
        id: id.into(),
        lon: p[0],
        lat: p[1],
        z: p[2].is_finite().then_some(p[2]),
    };
    let request = RouteRequest {
        route_id: format!("transit-directions-leg-{leg_index}"),
        origin: point("from", first),
        destination: point("to", last),
        snap: SnapOptions {
            max_distance_m: 0.01,
            z_window_m: Some(0.01),
            ..Default::default()
        },
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: Default::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    };
    let mut options = options.clone();
    options.use_profile_facility_attribute(&profile.document);
    // Reuse the street instruction engine only if it reproduces the exact
    // routed edge sequence. Otherwise give no turns rather than attach turns
    // from a different walk to the scheduled journey. Stairs, escalators and
    // lifts are maneuvers of their own.
    let mut maneuvers = match profile.engine.execute_directions(
        &request,
        DirectionsContext::new(names),
        &options,
        EngineMode::Auto,
    ) {
        Ok(result) if result.route.edge_path == path.edge_path => result
            .maneuvers
            .into_iter()
            .filter_map(|value| serde_json::to_value(value).ok())
            .collect::<Vec<_>>(),
        _ => {
            diagnostics.push(format!("Leg {leg_index}: turn instructions omitted because the street instruction route did not reproduce the saved network path."));
            Vec::new()
        }
    };
    let topology = profile.engine.topology();
    for maneuver in &mut maneuvers {
        let position = maneuver
            .get("begin_edge_index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let elevation = if position == 0 {
            first[2].is_finite().then_some(first[2])
        } else if position >= path.edge_path.len() {
            last[2].is_finite().then_some(last[2])
        } else {
            path.edge_path
                .get(position)
                .filter(|&&edge| (edge as usize) < topology.edge_count())
                .and_then(|&edge| {
                    topology.nodes[topology.routing_edge(edge as usize).from.0 as usize]
                        .elevation_m()
                })
        };
        if let Some(z) = elevation
            && let Some(location) = maneuver.get_mut("location").and_then(Value::as_array_mut)
            && location.len() == 2
        {
            location.push(json!(z));
        }
    }
    maneuvers
}

#[cfg(test)]
mod tests;
