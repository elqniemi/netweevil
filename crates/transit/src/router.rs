use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;

use crate::legs::{
    build_transit_route_stop_segments, build_transit_route_stops, coalesce_transit_legs,
    reconstruct_legs, seconds_for_distance, summarize_legs, transit_leg_minimums_enabled,
    transit_legs_satisfy_minimums,
};
use crate::model::{
    AccessMode, TransitBundle, TransitConnection, TransitLeg, TransitOutcome,
    TransitRouteAlternative, TransitRouteRequest, TransitRouteResult, TransitRouteSummary,
    TransitServiceAreaRequest, TransitServiceAreaResult,
};
use crate::runtime::{
    PrevStep, StateKey, StopSpatialIndex, TransitRuntime, build_departures_by_stop,
    can_finish_with_egress, can_start_transfer_walk, relax_state,
};
use crate::service_area::execute_transit_service_area_with_runtime;

pub struct PreparedTransitRouter {
    bundle: Arc<TransitBundle>,
    departures_by_stop: Vec<Vec<TransitConnection>>,
    stop_index: StopSpatialIndex,
}

impl PreparedTransitRouter {
    pub fn new(bundle: Arc<TransitBundle>) -> Self {
        let departures_by_stop = build_departures_by_stop(&bundle);
        let stop_index = StopSpatialIndex::new(&bundle.stops);
        Self {
            bundle,
            departures_by_stop,
            stop_index,
        }
    }

    pub fn bundle(&self) -> &TransitBundle {
        self.bundle.as_ref()
    }

    pub fn execute_route(&self, request: &TransitRouteRequest) -> Result<TransitRouteResult> {
        let runtime = TransitRuntime::new(
            self.bundle.as_ref(),
            &self.departures_by_stop,
            &self.stop_index,
            &request.modes,
        );
        execute_transit_route_with_runtime(&runtime, request)
    }

    pub fn execute_service_area(
        &self,
        request: &TransitServiceAreaRequest,
    ) -> Result<TransitServiceAreaResult> {
        let runtime = TransitRuntime::new(
            self.bundle.as_ref(),
            &self.departures_by_stop,
            &self.stop_index,
            &request.modes,
        );
        execute_transit_service_area_with_runtime(&runtime, request)
    }
}

pub fn execute_transit_route(
    bundle: &TransitBundle,
    request: &TransitRouteRequest,
) -> Result<TransitRouteResult> {
    let departures_by_stop = build_departures_by_stop(bundle);
    let stop_index = StopSpatialIndex::new(&bundle.stops);
    let runtime = TransitRuntime::new(bundle, &departures_by_stop, &stop_index, &request.modes);
    execute_transit_route_with_runtime(&runtime, request)
}

fn execute_transit_route_with_runtime(
    runtime: &TransitRuntime<'_>,
    request: &TransitRouteRequest,
) -> Result<TransitRouteResult> {
    let bundle = runtime.bundle;
    if request.time.arrive_by {
        return Ok(TransitRouteResult {
            route_id: request.route_id.clone(),
            outcome: TransitOutcome::NotImplemented,
            summary: TransitRouteSummary::default(),
            legs: Vec::new(),
            stops: Vec::new(),
            stop_segments: Vec::new(),
            diagnostics: vec![
                "arrive_by transit searches are not implemented yet; use depart-after timing"
                    .to_string(),
            ],
            alternatives: Vec::new(),
        });
    }
    if !request.modes.access.contains(&AccessMode::Walk)
        || !request.modes.egress.contains(&AccessMode::Walk)
    {
        return Ok(TransitRouteResult {
            route_id: request.route_id.clone(),
            outcome: TransitOutcome::NotImplemented,
            summary: TransitRouteSummary::default(),
            legs: Vec::new(),
            stops: Vec::new(),
            stop_segments: Vec::new(),
            diagnostics: vec![
                "only pedestrian access and egress are implemented; bicycle and car access are reserved in the request schema".to_string(),
            ],
            alternatives: Vec::new(),
        });
    }

    let departure_s = runtime.request_departure_seconds(&request.time.datetime)?;
    let search_end_s = departure_s.saturating_add(request.time.search_window_s);
    let access = runtime.nearby_access_stops(
        request.origin.lon,
        request.origin.lat,
        request.modes.max_access_distance_m,
    );
    let egress = runtime.nearby_access_stops(
        request.destination.lon,
        request.destination.lat,
        request.modes.max_egress_distance_m,
    );
    if access.is_empty() || egress.is_empty() {
        return Ok(TransitRouteResult {
            route_id: request.route_id.clone(),
            outcome: TransitOutcome::Unreachable,
            summary: TransitRouteSummary {
                departure_s,
                ..TransitRouteSummary::default()
            },
            legs: Vec::new(),
            stops: Vec::new(),
            stop_segments: Vec::new(),
            diagnostics: vec![format!(
                "no stop found within access/egress limits (access candidates {}, egress candidates {})",
                access.len(),
                egress.len()
            )],
            alternatives: Vec::new(),
        });
    }

    let mut heap = BinaryHeap::new();
    let mut best = HashMap::<StateKey, u32>::new();
    let mut prev = HashMap::<StateKey, PrevStep>::new();
    for candidate in access {
        let access_time_s =
            seconds_for_distance(candidate.distance_m, request.modes.walk_speed_kph);
        let state = StateKey {
            stop_index: candidate.stop_index,
            boardings: 0,
            trip_index: u32::MAX,
        };
        let arrival_s = departure_s.saturating_add(access_time_s);
        relax_state(
            &mut heap,
            &mut best,
            &mut prev,
            state,
            arrival_s,
            PrevStep::Access {
                from_id: request.origin.id.clone(),
                from_name: request.origin.id.clone(),
                from_lon: request.origin.lon,
                from_lat: request.origin.lat,
                distance_m: candidate.distance_m,
                departure_s,
            },
        );
    }

    let egress_by_stop = egress
        .into_iter()
        .map(|candidate| (candidate.stop_index, candidate.distance_m))
        .collect::<HashMap<_, _>>();
    let mut best_final: Option<(u32, StateKey, u32)> = None;
    let mut final_candidates = Vec::<(u32, StateKey, u32)>::new();
    let collect_alternatives = request.alternatives.max_routes > 1;
    let collect_final_candidates =
        collect_alternatives || transit_leg_minimums_enabled(&request.modes);

    while let Some(entry) = heap.pop() {
        if best
            .get(&entry.state)
            .is_some_and(|known| *known < entry.time_s)
        {
            continue;
        }
        if !collect_alternatives {
            if let Some((arrival_s, _, _)) = best_final {
                if entry.time_s >= arrival_s {
                    continue;
                }
            }
        } else if let Some((arrival_s, _, _)) = best_final {
            let max_arrival_s = transit_alternative_arrival_limit(departure_s, arrival_s, request);
            if entry.time_s > max_arrival_s {
                continue;
            }
        }
        if entry.time_s > search_end_s.saturating_add(6 * 60 * 60) {
            continue;
        }

        if can_finish_with_egress(entry.state) {
            if let Some(distance_m) = egress_by_stop.get(&entry.state.stop_index).copied() {
                let walk_s = seconds_for_distance(distance_m, request.modes.walk_speed_kph);
                let arrival_s = entry.time_s.saturating_add(walk_s);
                if collect_final_candidates {
                    final_candidates.push((arrival_s, entry.state, walk_s));
                }
                if best_final
                    .as_ref()
                    .is_none_or(|(best_arrival_s, _, _)| arrival_s < *best_arrival_s)
                {
                    best_final = Some((arrival_s, entry.state, walk_s));
                }
            }
        }

        if can_start_transfer_walk(entry.state) {
            let transfer_departure_s = entry.time_s.saturating_add(request.modes.transfer_slack_s);
            for transfer in runtime.nearby_stop_indexes(
                entry.state.stop_index,
                request.modes.max_transfer_distance_m,
            ) {
                if transfer.stop_index == entry.state.stop_index {
                    continue;
                }
                let walk_s =
                    seconds_for_distance(transfer.distance_m, request.modes.walk_speed_kph);
                let next_state = StateKey {
                    stop_index: transfer.stop_index,
                    boardings: entry.state.boardings,
                    trip_index: u32::MAX,
                };
                relax_state(
                    &mut heap,
                    &mut best,
                    &mut prev,
                    next_state,
                    transfer_departure_s.saturating_add(walk_s),
                    PrevStep::Transfer {
                        previous: entry.state,
                        distance_m: transfer.distance_m,
                        departure_s: transfer_departure_s,
                    },
                );
            }
        }

        if let Some(departures) = runtime
            .departures_by_stop
            .get(entry.state.stop_index as usize)
        {
            let start = departures.partition_point(|connection| {
                let slack = if entry.state.trip_index == connection.trip_index {
                    0
                } else {
                    request.modes.board_slack_s
                };
                connection.departure_s < entry.time_s.saturating_add(slack)
            });
            for connection in departures[start..].iter() {
                if connection.departure_s > search_end_s {
                    break;
                }
                if !runtime.allowed_routes[connection.route_index as usize] {
                    continue;
                }
                let same_trip = entry.state.trip_index == connection.trip_index;
                let next_boardings = if same_trip {
                    entry.state.boardings
                } else {
                    entry.state.boardings.saturating_add(1)
                };
                if next_boardings > request.modes.max_transfers.saturating_add(1) {
                    continue;
                }
                let next_state = StateKey {
                    stop_index: connection.to_stop_index,
                    boardings: next_boardings,
                    trip_index: connection.trip_index,
                };
                relax_state(
                    &mut heap,
                    &mut best,
                    &mut prev,
                    next_state,
                    connection.arrival_s,
                    PrevStep::Transit {
                        previous: entry.state,
                        connection: *connection,
                    },
                );
            }
        }
    }

    let found_final_candidate = best_final.is_some();
    let Some((arrival_s, _final_state, _egress_walk_s, mut legs)) = select_transit_final_candidate(
        bundle,
        runtime,
        request,
        &prev,
        best_final,
        &mut final_candidates,
    )?
    else {
        return Ok(TransitRouteResult {
            route_id: request.route_id.clone(),
            outcome: TransitOutcome::Unreachable,
            summary: TransitRouteSummary {
                departure_s,
                ..TransitRouteSummary::default()
            },
            legs: Vec::new(),
            stops: Vec::new(),
            stop_segments: Vec::new(),
            diagnostics: vec![unreachable_transit_route_diagnostic(
                request,
                found_final_candidate,
            )],
            alternatives: Vec::new(),
        });
    };
    let stops = if request.returns.include_stops {
        build_transit_route_stops(&legs)
    } else {
        Default::default()
    };
    let stop_segments = if request.returns.include_stop_segments {
        build_transit_route_stop_segments(&legs)
    } else {
        Default::default()
    };
    coalesce_transit_legs(&mut legs);
    let summary = summarize_legs(departure_s, arrival_s, &legs);
    let alternatives = build_transit_alternatives(
        bundle,
        runtime,
        request,
        &prev,
        departure_s,
        &summary,
        &legs,
        final_candidates,
    )?;
    Ok(TransitRouteResult {
        route_id: request.route_id.clone(),
        outcome: TransitOutcome::Scheduled,
        summary,
        legs,
        stops,
        stop_segments,
        diagnostics: Vec::new(),
        alternatives,
    })
}

fn transit_alternative_arrival_limit(
    departure_s: u32,
    best_arrival_s: u32,
    request: &TransitRouteRequest,
) -> u32 {
    let best_duration = best_arrival_s.saturating_sub(departure_s).max(1);
    let ratio_limit = departure_s
        .saturating_add((best_duration as f64 * request.alternatives.max_time_ratio) as u32);
    let extra_limit = request
        .alternatives
        .max_extra_time_s
        .map(|extra| best_arrival_s.saturating_add(extra))
        .unwrap_or(u32::MAX);
    ratio_limit.min(extra_limit)
}

fn select_transit_final_candidate(
    bundle: &TransitBundle,
    runtime: &TransitRuntime<'_>,
    request: &TransitRouteRequest,
    prev: &HashMap<StateKey, PrevStep>,
    best_final: Option<(u32, StateKey, u32)>,
    final_candidates: &mut [(u32, StateKey, u32)],
) -> Result<Option<(u32, StateKey, u32, Vec<TransitLeg>)>> {
    if !transit_leg_minimums_enabled(&request.modes) {
        let Some((arrival_s, final_state, egress_walk_s)) = best_final else {
            return Ok(None);
        };
        let legs = reconstruct_legs(
            bundle,
            runtime,
            request,
            prev,
            final_state,
            arrival_s,
            egress_walk_s,
        )?;
        return Ok(Some((arrival_s, final_state, egress_walk_s, legs)));
    }

    final_candidates.sort_by_key(|(arrival_s, state, _)| (*arrival_s, state.boardings));
    for &(arrival_s, final_state, egress_walk_s) in final_candidates.iter() {
        let legs = reconstruct_legs(
            bundle,
            runtime,
            request,
            prev,
            final_state,
            arrival_s,
            egress_walk_s,
        )?;
        let mut coalesced = legs.clone();
        coalesce_transit_legs(&mut coalesced);
        if transit_legs_satisfy_minimums(bundle, &coalesced, &request.modes) {
            return Ok(Some((arrival_s, final_state, egress_walk_s, legs)));
        }
    }

    Ok(None)
}

fn unreachable_transit_route_diagnostic(
    request: &TransitRouteRequest,
    found_candidate: bool,
) -> String {
    if found_candidate && transit_leg_minimums_enabled(&request.modes) {
        return format!(
            "no scheduled journey found that satisfies minimum transit leg constraints (min duration {} s, min distance {:.0} m)",
            request.modes.min_transit_leg_duration_s, request.modes.min_transit_leg_distance_m
        );
    }
    "no scheduled journey found inside the search window".to_string()
}

fn build_transit_alternatives(
    bundle: &TransitBundle,
    runtime: &TransitRuntime<'_>,
    request: &TransitRouteRequest,
    prev: &HashMap<StateKey, PrevStep>,
    departure_s: u32,
    best_summary: &TransitRouteSummary,
    best_legs: &[TransitLeg],
    mut final_candidates: Vec<(u32, StateKey, u32)>,
) -> Result<Vec<TransitRouteAlternative>> {
    if request.alternatives.max_routes <= 1 {
        return Ok(Vec::new());
    }
    final_candidates.sort_by_key(|(arrival_s, state, _)| (*arrival_s, state.boardings));
    let mut alternatives = Vec::new();
    let mut signatures = HashSet::new();
    signatures.insert(transit_leg_signature(best_legs));
    let best_arrival_s = best_summary.arrival_s.unwrap_or(departure_s);
    let arrival_limit = transit_alternative_arrival_limit(departure_s, best_arrival_s, request);

    for (arrival_s, final_state, egress_walk_s) in final_candidates {
        if alternatives.len() + 1 >= request.alternatives.max_routes {
            break;
        }
        if arrival_s > arrival_limit {
            continue;
        }
        let mut legs = reconstruct_legs(
            bundle,
            runtime,
            request,
            prev,
            final_state,
            arrival_s,
            egress_walk_s,
        )?;
        coalesce_transit_legs(&mut legs);
        if !transit_legs_satisfy_minimums(bundle, &legs, &request.modes) {
            continue;
        }
        let signature = transit_leg_signature(&legs);
        if !signatures.insert(signature) {
            continue;
        }
        let summary = summarize_legs(departure_s, arrival_s, &legs);
        let stops = if request.returns.include_stops {
            build_transit_route_stops(&legs)
        } else {
            Default::default()
        };
        let stop_segments = if request.returns.include_stop_segments {
            build_transit_route_stop_segments(&legs)
        } else {
            Default::default()
        };
        let rank = alternatives.len() as u32 + 1;
        alternatives.push(TransitRouteAlternative {
            alternative_index: rank,
            rank,
            summary,
            legs,
            stops,
            stop_segments,
        });
    }

    Ok(alternatives)
}

fn transit_leg_signature(legs: &[TransitLeg]) -> String {
    legs.iter()
        .map(|leg| {
            format!(
                "{:?}:{}:{}:{}:{}",
                leg.leg_type,
                leg.from_id,
                leg.to_id,
                leg.route_id.as_deref().unwrap_or(""),
                leg.trip_id.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("|")
}
