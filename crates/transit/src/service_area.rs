use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};

use anyhow::{Result, bail};

use crate::legs::{seconds_for_distance, transit_connection_geometry};
use crate::model::{
    AccessMode, TransitBundle, TransitConnection, TransitOutcome, TransitPoint,
    TransitServiceAreaRequest, TransitServiceAreaResult, TransitServiceAreaSegment,
    TransitServiceAreaStop,
};
use crate::runtime::{
    PrevStep, StateKey, StopSpatialIndex, TransitRuntime, best_street_candidates,
    build_departures_by_stop, can_start_transfer_walk, relax_state,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TransitServiceAreaSegmentRef {
    origin_index: usize,
    trip_index: u32,
    route_index: u32,
    from_stop_index: u32,
    to_stop_index: u32,
    connection_departure_s: u32,
    connection_arrival_s: u32,
    boarding_count: u8,
}

impl TransitServiceAreaSegmentRef {
    fn connection(self) -> TransitConnection {
        TransitConnection {
            trip_index: self.trip_index,
            route_index: self.route_index,
            from_stop_index: self.from_stop_index,
            to_stop_index: self.to_stop_index,
            departure_s: self.connection_departure_s,
            arrival_s: self.connection_arrival_s,
        }
    }
}

pub fn execute_transit_service_area(
    bundle: &TransitBundle,
    request: &TransitServiceAreaRequest,
) -> Result<TransitServiceAreaResult> {
    let departures_by_stop = build_departures_by_stop(bundle);
    let stop_index = StopSpatialIndex::new(&bundle.stops);
    let runtime = TransitRuntime::new(bundle, &departures_by_stop, &stop_index, &request.modes);
    execute_transit_service_area_with_runtime(&runtime, request)
}

pub(crate) fn execute_transit_service_area_with_runtime(
    runtime: &TransitRuntime<'_>,
    request: &TransitServiceAreaRequest,
) -> Result<TransitServiceAreaResult> {
    if request.time.arrive_by {
        return Ok(TransitServiceAreaResult {
            analysis_id: request.analysis_id.clone(),
            outcome: TransitOutcome::NotImplemented,
            origin_count: request.origins.len(),
            processed_origin_count: 0,
            skipped_origin_count: request.origins.len(),
            max_travel_time_s: request.max_travel_time_s,
            stops: Vec::new(),
            stop_segments: Vec::new(),
            diagnostics: vec![
                "arrive_by transit service-area searches are not implemented yet; use depart-after timing"
                    .to_string(),
            ],
        });
    }
    let access_modes = request.modes.validated_access_modes()?;
    let max_access_distance_m = access_modes
        .iter()
        .map(|mode| mode.max_access_distance_m(&request.modes))
        .fold(0.0_f64, f64::max);

    let departure_s = runtime.request_departure_seconds(&request.time.datetime)?;
    let search_end_s = departure_s.saturating_add(request.time.search_window_s);
    let time_limit_s = departure_s.saturating_add(request.max_travel_time_s);
    let mut all_stops = Vec::new();
    let mut all_segment_refs = Vec::new();
    let mut seen_segment_refs = HashSet::new();
    let mut diagnostics = Vec::new();
    let mut processed_origin_count = 0_usize;
    let mut skipped_origin_count = 0_usize;

    for (origin_index, origin) in request.origins.iter().enumerate() {
        let access = best_street_candidates(
            runtime,
            origin.lon,
            origin.lat,
            &access_modes,
            &request.modes,
            false,
        );
        if access.is_empty() {
            skipped_origin_count += 1;
            diagnostics.push(format!(
                "origin '{}' had no transit stop within {:.0} m for access modes {:?}",
                origin.id,
                max_access_distance_m,
                access_modes
                    .iter()
                    .map(|mode| mode.label())
                    .collect::<Vec<_>>()
            ));
            continue;
        }

        processed_origin_count += 1;
        let mut heap = BinaryHeap::new();
        let mut best = HashMap::<StateKey, u32>::new();
        let mut prev = HashMap::<StateKey, PrevStep>::new();

        for candidate in access {
            let arrival_s = departure_s.saturating_add(candidate.time_s);
            if arrival_s > time_limit_s {
                continue;
            }
            let state = StateKey {
                stop_index: candidate.stop_index,
                boardings: 0,
                trip_index: u32::MAX,
            };
            relax_state(
                &mut heap,
                &mut best,
                &mut prev,
                state,
                arrival_s,
                PrevStep::Access {
                    from_id: origin.id.clone(),
                    from_name: origin.id.clone(),
                    from_lon: origin.lon,
                    from_lat: origin.lat,
                    distance_m: candidate.distance_m,
                    departure_s,
                    mode: candidate.mode,
                },
            );
        }

        while let Some(entry) = heap.pop() {
            if best
                .get(&entry.state)
                .is_some_and(|known| *known < entry.time_s)
            {
                continue;
            }
            if entry.time_s > time_limit_s {
                continue;
            }

            if can_start_transfer_walk(entry.state) {
                let transfer_departure_s =
                    entry.time_s.saturating_add(request.modes.transfer_slack_s);
                for transfer in runtime.nearby_stop_indexes(
                    entry.state.stop_index,
                    request.modes.max_transfer_distance_m,
                ) {
                    if transfer.stop_index == entry.state.stop_index {
                        continue;
                    }
                    let walk_s =
                        seconds_for_distance(transfer.distance_m, request.modes.walk_speed_kph);
                    let arrival_s = transfer_departure_s.saturating_add(walk_s);
                    if arrival_s > time_limit_s {
                        continue;
                    }
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
                        arrival_s,
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
                    if connection.departure_s > search_end_s
                        || connection.departure_s > time_limit_s
                    {
                        break;
                    }
                    if connection.arrival_s > time_limit_s {
                        continue;
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
                    if best
                        .get(&next_state)
                        .is_some_and(|known| connection.arrival_s >= *known)
                    {
                        continue;
                    }
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
                    if request.returns.include_stop_segments {
                        let segment_ref = TransitServiceAreaSegmentRef {
                            origin_index,
                            trip_index: connection.trip_index,
                            route_index: connection.route_index,
                            from_stop_index: connection.from_stop_index,
                            to_stop_index: connection.to_stop_index,
                            connection_departure_s: connection.departure_s,
                            connection_arrival_s: connection.arrival_s,
                            boarding_count: next_boardings,
                        };
                        if seen_segment_refs.insert(segment_ref) {
                            ensure_transit_service_area_output_room(
                                all_segment_refs.len(),
                                request.returns.max_stop_segments,
                                "stop segments",
                                "disable returns.include_stop_segments, reduce max_travel_time_s/search_window_s, or raise returns.max_stop_segments explicitly",
                            )?;
                            all_segment_refs.push(segment_ref);
                        }
                    }
                }
            }
        }

        if request.returns.include_stops {
            let mut stop_best = BTreeMap::<u32, (u32, u8, StateKey)>::new();
            for (state, arrival_s) in &best {
                let (state, arrival_s) = (*state, *arrival_s);
                if arrival_s > time_limit_s {
                    continue;
                }
                let entry = stop_best.entry(state.stop_index).or_insert((
                    arrival_s,
                    state.boardings,
                    state,
                ));
                if arrival_s < entry.0 || (arrival_s == entry.0 && state.boardings < entry.1) {
                    *entry = (arrival_s, state.boardings, state);
                }
            }
            for (stop_index, (arrival_s, boardings, state)) in stop_best {
                let stop = &runtime.bundle.stops[stop_index as usize];
                ensure_transit_service_area_output_room(
                    all_stops.len(),
                    request.returns.max_stops,
                    "stops",
                    "disable returns.include_stops, reduce max_travel_time_s/search_window_s, or raise returns.max_stops explicitly",
                )?;
                all_stops.push(TransitServiceAreaStop {
                    origin_id: origin.id.clone(),
                    stop_id: stop.stop_id.clone(),
                    stop_name: stop.name.clone(),
                    lon: stop.lon,
                    lat: stop.lat,
                    arrival_s,
                    travel_time_s: arrival_s.saturating_sub(departure_s),
                    boarding_count: boardings,
                    access_mode: access_mode_for_state(&prev, state),
                });
            }
        }
    }

    let all_segments = materialize_transit_service_area_segments(
        runtime.bundle,
        request,
        departure_s,
        all_segment_refs,
    )?;

    let outcome = if processed_origin_count == 0 {
        TransitOutcome::Unreachable
    } else {
        TransitOutcome::Scheduled
    };
    Ok(TransitServiceAreaResult {
        analysis_id: request.analysis_id.clone(),
        outcome,
        origin_count: request.origins.len(),
        processed_origin_count,
        skipped_origin_count,
        max_travel_time_s: request.max_travel_time_s,
        stops: all_stops,
        stop_segments: all_segments,
        diagnostics,
    })
}

fn ensure_transit_service_area_output_room(
    current_len: usize,
    max_len: usize,
    label: &str,
    advice: &str,
) -> Result<()> {
    if current_len >= max_len {
        bail!(
            "transit service-area output exceeded returns.max_{}={}; {}",
            label.replace(' ', "_"),
            max_len,
            advice
        );
    }
    Ok(())
}

fn materialize_transit_service_area_segments(
    bundle: &TransitBundle,
    request: &TransitServiceAreaRequest,
    query_departure_s: u32,
    segment_refs: Vec<TransitServiceAreaSegmentRef>,
) -> Result<Vec<TransitServiceAreaSegment>> {
    if !request.returns.include_stop_segments {
        return Ok(Vec::new());
    }

    let mut geometry_point_count = 0_usize;
    let mut segments = Vec::with_capacity(segment_refs.len());
    for segment_ref in segment_refs {
        let origin = &request.origins[segment_ref.origin_index];
        let segment = transit_service_area_segment(
            bundle,
            origin,
            query_departure_s,
            segment_ref.boarding_count,
            segment_ref.connection(),
            request.returns.include_geometry,
        );
        if request.returns.include_geometry {
            geometry_point_count = geometry_point_count.saturating_add(segment.geometry.len());
            if geometry_point_count > request.returns.max_geometry_points {
                bail!(
                    "transit service-area output exceeded returns.max_geometry_points={}; disable returns.include_geometry, reduce max_travel_time_s/search_window_s, or raise returns.max_geometry_points explicitly",
                    request.returns.max_geometry_points
                );
            }
        }
        segments.push(segment);
    }
    Ok(segments)
}

fn access_mode_for_state(
    prev: &HashMap<StateKey, PrevStep>,
    mut state: StateKey,
) -> Option<AccessMode> {
    loop {
        match prev.get(&state)? {
            PrevStep::Access { mode, .. } => return Some(*mode),
            PrevStep::Transfer { previous, .. } => state = *previous,
            PrevStep::Transit { previous, .. } => state = *previous,
        }
    }
}

fn transit_service_area_segment(
    bundle: &TransitBundle,
    origin: &TransitPoint,
    departure_s: u32,
    boarding_count: u8,
    connection: TransitConnection,
    include_geometry: bool,
) -> TransitServiceAreaSegment {
    let from_stop = &bundle.stops[connection.from_stop_index as usize];
    let to_stop = &bundle.stops[connection.to_stop_index as usize];
    let route = &bundle.routes[connection.route_index as usize];
    let trip = &bundle.trips[connection.trip_index as usize];
    TransitServiceAreaSegment {
        origin_id: origin.id.clone(),
        from_stop_id: from_stop.stop_id.clone(),
        to_stop_id: to_stop.stop_id.clone(),
        from_stop_name: from_stop.name.clone(),
        to_stop_name: to_stop.name.clone(),
        departure_s: connection.departure_s,
        arrival_s: connection.arrival_s,
        duration_s: connection.arrival_s.saturating_sub(connection.departure_s),
        travel_time_s: connection.arrival_s.saturating_sub(departure_s),
        boarding_count,
        mode: Some(route.mode),
        route_id: Some(route.route_id.clone()),
        route_short_name: Some(route.short_name.clone()),
        trip_id: Some(trip.trip_id.clone()),
        geometry: if include_geometry {
            transit_connection_geometry(bundle, connection)
        } else {
            Default::default()
        },
    }
}
