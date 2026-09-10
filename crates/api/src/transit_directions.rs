//! Journey instructions derived from the same scheduled route used by the map.
use std::collections::BTreeMap;

use axum::{Json, extract::State};
use netweevil_query::{EngineMode, LabeledPoint, RouteRequest, SnapOptions};
use netweevil_transit::{
    AccessMode, GtfsStopContext, TransitLeg, TransitLegType, TransitOutcome, TransitRouteResult,
    TransitTimeContext, read_gtfs_stop_context,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::dto::{TransitExecutionContext, TransitRouteExecutionRequest};
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
    directions: Vec<TransitDirection>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    directions_diagnostics: Vec<String>,
}

pub(crate) async fn transit_directions_handler(
    State(state): State<ApiState>,
    Json(mut payload): Json<TransitRouteExecutionRequest>,
) -> Result<Json<TransitDirectionsResponse>, ApiError> {
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
        let contexts = read_gtfs_stop_context(&source_path).unwrap_or_else(|error| {
            diagnostics.push(format!(
                "Station/platform display metadata unavailable; using stored stop names: {error}"
            ));
            BTreeMap::new()
        });
        let edge_names = load_edge_names(&worker_runtime).ok();
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
                            edge_names.as_deref().unwrap_or(&[]),
                            index,
                            &mut diagnostics,
                        )
                    })
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        let directions = build_directions(&response.result, &contexts, &walking)?;
        Ok(TransitDirectionsResponse {
            service: response.service,
            result: response.result,
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
            let towards = leg
                .headsign
                .as_ref()
                .filter(|v| !v.is_empty())
                .map(|v| format!(" toward {v}"))
                .unwrap_or_default();
            if leg.departure_s > previous_arrival {
                steps.push(direction(
                    "wait",
                    index,
                    format!("Wait at {from} for {line}{towards}."),
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
                format!("Board {line}{towards} at {from}."),
                leg,
                leg.departure_s,
                leg.departure_s,
                &result.time_context,
                stops,
            )?);
            steps.push(direction(
                "ride",
                index,
                format!("Take {line} from {from} to {to}."),
                leg,
                leg.departure_s,
                leg.arrival_s,
                &result.time_context,
                stops,
            )?);
            steps.push(direction(
                "alight",
                index,
                format!("Alight at {to}."),
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
                    format!("Wait at {from}."),
                    leg,
                    previous_arrival,
                    leg.departure_s,
                    &result.time_context,
                    stops,
                )?);
            }
            let (kind, verb) = match leg.leg_type {
                TransitLegType::Access => ("access", "Travel"),
                TransitLegType::Transfer => ("transfer", "Transfer"),
                TransitLegType::Egress => ("egress", "Continue"),
                TransitLegType::Transit => unreachable!(),
            };
            let travel = match leg.street_mode.unwrap_or(AccessMode::Walk) {
                AccessMode::Walk => "on foot",
                AccessMode::Bicycle => "by bicycle",
                AccessMode::Car => "by car",
            };
            let mut step = direction(
                kind,
                index,
                format!("{verb} {travel} from {from} to {to}."),
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
    names: &[String],
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
    // Reuse the street instruction engine only if it reproduces the exact
    // routed edge sequence. Otherwise retain source facility descriptions;
    // never attach turns from a different walk to the scheduled journey.
    let mut maneuvers = match profile
        .engine
        .execute_directions(&request, names, EngineMode::Auto)
    {
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
    let mut previous_facility = None;
    for (position, &edge_index) in path.edge_path.iter().enumerate() {
        if edge_index as usize >= topology.edge_count() {
            continue;
        }
        let edge = topology.routing_edge(edge_index as usize);
        let facility = profile
            .document
            .facilities
            .classes
            .iter()
            .find(|(class, _)| {
                topology.edge_attribute_matches(
                    edge_index as usize,
                    &profile.document.facilities.attribute,
                    class,
                )
            });
        let Some((class, _)) = facility else {
            previous_facility = None;
            continue;
        };
        if previous_facility == Some((edge.feature_row, class.as_str())) {
            continue;
        }
        previous_facility = Some((edge.feature_row, class.as_str()));
        let from = &topology.nodes[edge.from.0 as usize];
        let to = &topology.nodes[edge.to.0 as usize];
        let vertical = if to.z > from.z + 0.01 {
            " up"
        } else if to.z < from.z - 0.01 {
            " down"
        } else {
            ""
        };
        let label = class.replace('_', " ");
        maneuvers.push(json!({"kind":class,"instruction":format!("Take the {label}{vertical}."),"location":source_location(from.lon,from.lat,from.elevation_m()),"begin_edge_index":position,"end_edge_index":position+1,"source":"network_facility_attribute","from_elevation_m":from.elevation_m(),"to_elevation_m":to.elevation_m()}));
    }
    maneuvers.sort_by_key(|value| {
        value
            .get("begin_edge_index")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    });
    maneuvers
}

fn source_location(lon: f64, lat: f64, elevation: Option<f64>) -> Value {
    match elevation {
        Some(z) => json!([lon, lat, z]),
        None => json!([lon, lat]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_locations_preserve_known_elevation_without_inventing_a_floor() {
        assert_eq!(
            source_location(114.1, 22.3, Some(-30.93)),
            json!([114.1, 22.3, -30.93])
        );
        assert_eq!(source_location(114.1, 22.3, None), json!([114.1, 22.3]));
    }

    fn two_ride_journey() -> TransitRouteResult {
        serde_json::from_value(json!({
            "route_id":"station-transfer", "time_context":{"agency_timezone":"Asia/Hong_Kong","time_origin_unix_s":1788969600},
            "outcome":"scheduled", "summary":{"departure_s":0,"arrival_s":120,"total_travel_time_s":120,"transit_time_s":70,"access_egress_time_s":20,"transfer_time_s":15,"wait_time_s":15,"boarding_count":2},
            "diagnostics":[], "legs":[
                {"leg_type":"access","from_id":"O","to_id":"P1","from_name":"Origin","to_name":"Platform 1","departure_s":0,"arrival_s":10,"street_mode":"walk","geometry":[[114.1,22.3,10],[114.1,22.3,-12]]},
                {"leg_type":"transit","from_id":"P1","to_id":"P2","from_name":"Platform 1","to_name":"Platform 2","departure_s":20,"arrival_s":50,"mode":"subway","route_id":"R1","route_short_name":"Island","trip_id":"T1","headsign":"Harbour","geometry":[[114.1,22.3,-12],[114.2,22.3,-12]]},
                {"leg_type":"transfer","from_id":"P2","to_id":"P3","from_name":"Platform 2","to_name":"Tram platform","departure_s":50,"arrival_s":65,"street_mode":"walk","geometry":[[114.2,22.3,-12],[114.2,22.3,4]]},
                {"leg_type":"transit","from_id":"P3","to_id":"P4","from_name":"Tram platform","to_name":"Pier","departure_s":70,"arrival_s":110,"mode":"tram","route_id":"R2","route_short_name":"T2","trip_id":"T2","headsign":"Pier","geometry":[[114.2,22.3,4],[114.3,22.3,4]]},
                {"leg_type":"egress","from_id":"P4","to_id":"D","from_name":"Pier","to_name":"Destination","departure_s":110,"arrival_s":120,"street_mode":"walk","geometry":[[114.3,22.3,4],[114.3,22.31,4]]}
            ]
        })).unwrap()
    }

    #[test]
    fn orders_two_rides_station_transfer_and_source_walking_instructions() {
        let result = two_ride_journey();
        let stops = BTreeMap::from([(
            "P1".into(),
            GtfsStopContext {
                stop_name: "Platform 1".into(),
                station_name: Some("Central".into()),
                platform_code: Some("1".into()),
            },
        )]);
        let mut walking = vec![Vec::new(); result.legs.len()];
        walking[2] = vec![
            json!({"kind":"stairs","instruction":"Take the stairs up.","source":"network_facility_attribute"}),
            json!({"kind":"lift","instruction":"Take the lift up.","source":"network_facility_attribute"}),
        ];
        let steps = build_directions(&result, &stops, &walking).unwrap();
        assert_eq!(
            steps
                .iter()
                .map(|step| step.kind.as_str())
                .collect::<Vec<_>>(),
            [
                "access", "wait", "board", "ride", "alight", "transfer", "wait", "board", "ride",
                "alight", "egress"
            ]
        );
        assert_eq!(steps.iter().map(|step| step.duration_s).sum::<u32>(), 120);
        assert!(
            steps[2]
                .instruction
                .contains("Island toward Harbour at Central, Platform 1")
        );
        assert_eq!(steps[2].platform.as_deref(), Some("1"));
        assert_eq!(steps[2].location, Some([114.1, 22.3, -12.0]));
        assert_eq!(steps[5].walking_maneuvers, walking[2]);
        assert_eq!(steps[7].route_short_name.as_deref(), Some("T2"));
        assert_eq!(steps[10].arrival_s, 120);
        assert!(steps[10].arrival_datetime.ends_with("00:02:00+08:00"));
        for (index, step) in steps.iter().enumerate() {
            assert_eq!(step.sequence, index + 1);
        }
    }

    #[test]
    fn unreachable_journey_has_no_instructions_and_filtered_legs_are_not_invented() {
        let mut result = two_ride_journey();
        result.outcome = TransitOutcome::Unreachable;
        assert!(
            build_directions(&result, &BTreeMap::new(), &[])
                .unwrap()
                .is_empty()
        );
        result.outcome = TransitOutcome::Scheduled;
        result
            .legs
            .retain(|leg| leg.mode != Some(netweevil_transit::TransitMode::Tram));
        let steps = build_directions(&result, &BTreeMap::new(), &[]).unwrap();
        assert_eq!(steps.iter().filter(|step| step.kind == "board").count(), 1);
        assert!(
            steps
                .iter()
                .all(|step| step.route_id.as_deref() != Some("R2"))
        );
    }
}
