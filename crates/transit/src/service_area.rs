use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};

use anyhow::{Result, bail};
use rayon::prelude::*;

use crate::backward::{
    BackwardIndex, BackwardQueueEntry, BackwardStance, BackwardStateKey, BackwardStep,
    arrivals_at_or_before, previous_run_connection, relax_backward_state, reverse_transfer_walks,
};
use crate::legs::{ShapePointIndexCache, seconds_for_distance, transit_connection_geometry_cached};
use crate::model::{
    AccessMode, TransitBundle, TransitCatchmentMode, TransitConnection, TransitOutcome,
    TransitPoint, TransitServiceAreaRequest, TransitServiceAreaResult, TransitServiceAreaSegment,
    TransitServiceAreaStop,
};
use crate::runtime::{
    PrevStep, QueueEntry, StateKey, StopSpatialIndex, TransitRuntime, best_street_candidates,
    boarding_slack_s, build_departures_by_stop, build_transfer_candidates, can_start_transfer_walk,
    relax_state,
};

/// Time anchor a service area is measured against: the query departure for a
/// depart-after sweep, or the arrival deadline for an arrive-by sweep. Both
/// report the clock time at each stop plus the travel time between that stop
/// and the anchor, so the result shape is identical.
#[derive(Debug, Clone, Copy)]
enum ServiceAreaAnchor {
    DepartAfter { departure_s: u32 },
    ArriveBy { deadline_s: u32 },
}

impl ServiceAreaAnchor {
    fn travel_time_s(self, stop_time_s: u32) -> u32 {
        match self {
            Self::DepartAfter { departure_s } => stop_time_s.saturating_sub(departure_s),
            Self::ArriveBy { deadline_s } => deadline_s.saturating_sub(stop_time_s),
        }
    }

    /// Travel time attributed to a ridden connection: from the query anchor to
    /// the connection's far end.
    fn connection_travel_time_s(self, connection: TransitConnection) -> u32 {
        match self {
            Self::DepartAfter { .. } => self.travel_time_s(connection.arrival_s),
            Self::ArriveBy { .. } => self.travel_time_s(connection.departure_s),
        }
    }
}

const STOPS_LIMIT_ADVICE: &str = "disable returns.include_stops, reduce max_travel_time_s/search_window_s, or raise returns.max_stops explicitly";
const SEGMENTS_LIMIT_ADVICE: &str = "disable returns.include_stop_segments, reduce max_travel_time_s/search_window_s, or raise returns.max_stop_segments explicitly";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TransitServiceAreaSegmentRef {
    origin_index: usize,
    trip_index: u32,
    run_index: u32,
    pickup_allowed: bool,
    drop_off_allowed: bool,
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
            run_index: self.run_index,
            pickup_allowed: self.pickup_allowed,
            drop_off_allowed: self.drop_off_allowed,
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
    if let Some(profile_id) = request.modes.transfer_profile_id.as_deref() {
        bail!(
            "transit transfer profile '{}' requires PreparedTransitRouter::new_with_transfer_tables",
            profile_id
        );
    }
    let departures_by_stop = build_departures_by_stop(bundle);
    let stop_index = StopSpatialIndex::new(&bundle.stops);
    let transfer_candidates =
        build_transfer_candidates(bundle, &stop_index, request.modes.max_transfer_distance_m);
    let runtime = TransitRuntime::new(
        bundle,
        &departures_by_stop,
        &stop_index,
        &transfer_candidates,
        &request.modes,
        None,
    );
    execute_transit_service_area_with_runtime(&runtime, request)
}

pub(crate) fn execute_transit_service_area_with_runtime(
    runtime: &TransitRuntime<'_>,
    request: &TransitServiceAreaRequest,
) -> Result<TransitServiceAreaResult> {
    if request.catchment_mode == TransitCatchmentMode::StreetIsochrone
        && runtime.street_estimator.is_none()
    {
        bail!("street_isochrone requires a loaded street-network service-area engine");
    }
    let mut network_request;
    let request = if request.catchment_mode == TransitCatchmentMode::StreetIsochrone {
        network_request = request.clone();
        network_request.modes.street_access = crate::TransitStreetAccessModel::Network;
        &network_request
    } else {
        request
    };
    let mut result = search_transit_service_area_with_runtime(runtime, request)?;
    result
        .diagnostics
        .extend(runtime.bundle.import_diagnostics.iter().cloned());
    Ok(result)
}

fn search_transit_service_area_with_runtime(
    runtime: &TransitRuntime<'_>,
    request: &TransitServiceAreaRequest,
) -> Result<TransitServiceAreaResult> {
    if request.time.arrive_by {
        return execute_transit_service_area_arrive_by(runtime, request);
    }
    let access_modes = request.modes.validated_access_modes()?;
    let max_access_distance_m = access_modes
        .iter()
        .map(|mode| mode.max_access_distance_m(&request.modes))
        .fold(0.0_f64, f64::max);

    let departure_s = runtime.request_time_seconds(&request.time.datetime)?;
    let search_end_s = departure_s.saturating_add(request.time.search_window_s);
    let time_limit_s = departure_s.saturating_add(request.max_travel_time_s);
    let origin_outputs = request
        .origins
        .par_iter()
        .enumerate()
        .map_init(
            OriginSearchScratch::default,
            |scratch, (origin_index, origin)| {
                search_service_area_origin(
                    runtime,
                    request,
                    &access_modes,
                    max_access_distance_m,
                    departure_s,
                    search_end_s,
                    time_limit_s,
                    origin_index,
                    origin,
                    scratch,
                )
            },
        )
        .collect::<Result<Vec<_>>>()?;

    assemble_transit_service_area_result(
        runtime,
        request,
        ServiceAreaAnchor::DepartAfter { departure_s },
        origin_outputs,
    )
}

/// Latest-departure sweep: for every stop, the latest clock time at which a
/// traveller can leave it and still reach one of the request's points by the
/// deadline. Travel times are measured back from the deadline.
fn execute_transit_service_area_arrive_by(
    runtime: &TransitRuntime<'_>,
    request: &TransitServiceAreaRequest,
) -> Result<TransitServiceAreaResult> {
    let egress_modes = request.modes.validated_egress_modes()?;
    let max_egress_distance_m = egress_modes
        .iter()
        .map(|mode| mode.max_egress_distance_m(&request.modes))
        .fold(0.0_f64, f64::max);

    let deadline_s = runtime.request_time_seconds(&request.time.datetime)?;
    let search_start_s = deadline_s.saturating_sub(request.time.search_window_s);
    let time_floor_s = deadline_s.saturating_sub(request.max_travel_time_s);
    let owned_index;
    let index = match runtime.backward_index {
        Some(index) => index,
        None => {
            owned_index = BackwardIndex::build(runtime.bundle, runtime.all_transfer_candidates());
            &owned_index
        }
    };
    let origin_outputs = request
        .origins
        .par_iter()
        .enumerate()
        .map_init(
            BackwardOriginSearchScratch::default,
            |scratch, (origin_index, origin)| {
                search_service_area_target(
                    runtime,
                    index,
                    request,
                    &egress_modes,
                    max_egress_distance_m,
                    deadline_s,
                    search_start_s,
                    time_floor_s,
                    origin_index,
                    origin,
                    scratch,
                )
            },
        )
        .collect::<Result<Vec<_>>>()?;

    assemble_transit_service_area_result(
        runtime,
        request,
        ServiceAreaAnchor::ArriveBy { deadline_s },
        origin_outputs,
    )
}

fn assemble_transit_service_area_result(
    runtime: &TransitRuntime<'_>,
    request: &TransitServiceAreaRequest,
    anchor: ServiceAreaAnchor,
    origin_outputs: Vec<OriginSearchOutput>,
) -> Result<TransitServiceAreaResult> {
    let bundle = runtime.bundle;
    let mut all_stops = Vec::new();
    let mut all_segment_refs = Vec::new();
    let mut features = Vec::new();
    let mut diagnostics = Vec::new();
    let mut processed_origin_count = 0_usize;
    let mut skipped_origin_count = 0_usize;
    for (origin, output) in request.origins.iter().zip(origin_outputs) {
        let street_features = if request.catchment_mode == TransitCatchmentMode::StreetIsochrone {
            let stops = output
                .street_seeds
                .iter()
                .map(|&(index, elapsed)| (&bundle.stops[index as usize], elapsed))
                .collect::<Vec<_>>();
            runtime
                .street_estimator
                .expect("validated street estimator")
                .street_isochrone(origin, &stops, request)?
        } else {
            Vec::new()
        };
        if let Some(diagnostic) = output.skipped_diagnostic {
            diagnostics.push(diagnostic);
            if street_features.is_empty() {
                skipped_origin_count += 1;
                continue;
            }
        }
        features.extend(street_features);
        processed_origin_count += 1;
        for stop in output.stops {
            ensure_transit_service_area_output_room(
                all_stops.len(),
                request.returns.max_stops,
                "stops",
                STOPS_LIMIT_ADVICE,
            )?;
            all_stops.push(stop);
        }
        for segment_ref in output.segment_refs {
            ensure_transit_service_area_output_room(
                all_segment_refs.len(),
                request.returns.max_stop_segments,
                "stop segments",
                SEGMENTS_LIMIT_ADVICE,
            )?;
            all_segment_refs.push(segment_ref);
        }
    }

    let all_segments =
        materialize_transit_service_area_segments(bundle, request, anchor, all_segment_refs)?;
    let geometry_points = all_segments
        .iter()
        .map(|segment| segment.geometry.len())
        .sum::<usize>()
        + features
            .iter()
            .filter_map(|feature| feature.geometry.as_ref())
            .map(geometry_point_count)
            .sum::<usize>();
    if geometry_points > request.returns.max_geometry_points {
        bail!(
            "transit service-area output has {} geometry points but returns.max_geometry_points is {}; reduce max_travel_time_s or raise returns.max_geometry_points",
            geometry_points,
            request.returns.max_geometry_points
        );
    }

    let outcome = if processed_origin_count == 0 {
        TransitOutcome::Unreachable
    } else {
        TransitOutcome::Scheduled
    };
    Ok(TransitServiceAreaResult {
        analysis_id: request.analysis_id.clone(),
        catchment_mode: request.catchment_mode,
        time_context: bundle.time_context(),
        outcome,
        origin_count: request.origins.len(),
        processed_origin_count,
        skipped_origin_count,
        max_travel_time_s: request.max_travel_time_s,
        stops: all_stops,
        stop_segments: all_segments,
        features,
        diagnostics,
    })
}

fn geometry_point_count(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Array(values)
            if values.first().is_some_and(serde_json::Value::is_number) =>
        {
            1
        }
        serde_json::Value::Array(values) => values.iter().map(geometry_point_count).sum(),
        serde_json::Value::Object(values) => {
            values.get("coordinates").map_or(0, geometry_point_count)
        }
        _ => 0,
    }
}

/// Per-origin search output, merged in origin order after the parallel sweep.
struct OriginSearchOutput {
    skipped_diagnostic: Option<String>,
    stops: Vec<TransitServiceAreaStop>,
    segment_refs: Vec<TransitServiceAreaSegmentRef>,
    street_seeds: Vec<(u32, u32)>,
}

/// Search state reused across origins on the same rayon worker thread.
#[derive(Default)]
struct OriginSearchScratch {
    heap: BinaryHeap<QueueEntry>,
    best: HashMap<StateKey, u32>,
    prev: HashMap<StateKey, PrevStep>,
    seen_segment_refs: HashSet<TransitServiceAreaSegmentRef>,
}

impl OriginSearchScratch {
    fn reset(&mut self) {
        self.heap.clear();
        self.best.clear();
        self.prev.clear();
        self.seen_segment_refs.clear();
    }
}

fn search_service_area_origin(
    runtime: &TransitRuntime<'_>,
    request: &TransitServiceAreaRequest,
    access_modes: &[AccessMode],
    max_access_distance_m: f64,
    departure_s: u32,
    search_end_s: u32,
    time_limit_s: u32,
    origin_index: usize,
    origin: &TransitPoint,
    scratch: &mut OriginSearchScratch,
) -> Result<OriginSearchOutput> {
    let access = best_street_candidates(runtime, origin, access_modes, &request.modes, false);
    if access.is_empty() {
        return Ok(OriginSearchOutput {
            skipped_diagnostic: Some(format!(
                "origin '{}' had no transit stop within {:.0} m for access modes {:?}",
                origin.id,
                max_access_distance_m,
                access_modes
                    .iter()
                    .map(|mode| mode.label())
                    .collect::<Vec<_>>()
            )),
            stops: Vec::new(),
            segment_refs: Vec::new(),
            street_seeds: Vec::new(),
        });
    }

    scratch.reset();
    let OriginSearchScratch {
        heap,
        best,
        prev,
        seen_segment_refs,
    } = scratch;
    let mut segment_refs = Vec::new();

    for candidate in access {
        let arrival_s = departure_s.saturating_add(candidate.time_s);
        if arrival_s > time_limit_s {
            continue;
        }
        let state = StateKey {
            stop_index: candidate.stop_index,
            boardings: 0,
            connection_index: u32::MAX,
            can_alight: false,
            transfer_from_connection: u32::MAX,
        };
        relax_state(
            heap,
            best,
            prev,
            state,
            arrival_s,
            PrevStep::Access {
                from_id: origin.id.clone(),
                from_name: origin.id.clone(),
                from_lon: origin.lon,
                from_lat: origin.lat,
                departure_s,
                time_s: candidate.time_s,
                mode: candidate.mode,
                // Service-area output does not materialize access paths; avoid
                // retaining a potentially large path per reached state.
                network_path: None,
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
            let transfer_departure_s = entry.time_s.saturating_add(request.modes.transfer_slack_s);
            for transfer in runtime
                .transfer_candidates(entry.state.stop_index)
                .iter()
                .take_while(|transfer| transfer.distance_m <= request.modes.max_transfer_distance_m)
            {
                if transfer.stop_index == entry.state.stop_index {
                    continue;
                }
                let walk_s = transfer.transfer_time_s.unwrap_or_else(|| {
                    seconds_for_distance(transfer.distance_m, request.modes.walk_speed_kph)
                });
                let arrival_s = transfer_departure_s.saturating_add(walk_s);
                if arrival_s > time_limit_s {
                    continue;
                }
                let next_state = StateKey {
                    stop_index: transfer.stop_index,
                    boardings: entry.state.boardings,
                    connection_index: u32::MAX,
                    can_alight: false,
                    transfer_from_connection: runtime
                        .transfer_walk_context(entry.state.connection_index),
                };
                relax_state(
                    heap,
                    best,
                    prev,
                    next_state,
                    arrival_s,
                    PrevStep::Transfer {
                        previous: entry.state,
                        departure_s: transfer_departure_s,
                        travel_time_s: walk_s,
                        network_path: None,
                    },
                );
            }
        }

        if let Some(departures) = runtime
            .departures_by_stop
            .by_stop
            .get(entry.state.stop_index as usize)
        {
            // The binary-search predicate must be monotone in departure
            // time. Trip-specific slack is checked after locating the range.
            let start = departures.partition_point(|index| {
                runtime.bundle.connections[*index as usize].departure_s < entry.time_s
            });
            for index in &departures[start..] {
                let connection = &runtime.bundle.connections[*index as usize];
                if connection.departure_s > search_end_s || connection.departure_s > time_limit_s {
                    break;
                }
                if connection.arrival_s > time_limit_s {
                    continue;
                }
                if !runtime.allowed_routes[connection.route_index as usize] {
                    continue;
                }
                let same_run = runtime
                    .departures_by_stop
                    .next_connection
                    .get(entry.state.connection_index as usize)
                    .is_some_and(|next| *next == *index);
                if connection.departure_s
                    < entry.time_s.saturating_add(boarding_slack_s(
                        entry.state,
                        same_run,
                        &request.modes,
                    ))
                {
                    continue;
                }
                if !same_run
                    && (!connection.pickup_allowed
                        || (entry.state.connection_index != u32::MAX && !entry.state.can_alight))
                {
                    continue;
                }
                if !same_run && !runtime.forward_transfer_allowed(entry.state, connection)? {
                    continue;
                }
                let next_boardings = if same_run {
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
                    connection_index: *index,
                    can_alight: connection.drop_off_allowed,
                    transfer_from_connection: u32::MAX,
                };
                if best
                    .get(&next_state)
                    .is_some_and(|known| connection.arrival_s >= *known)
                {
                    continue;
                }
                relax_state(
                    heap,
                    best,
                    prev,
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
                        run_index: connection.run_index,
                        pickup_allowed: connection.pickup_allowed,
                        drop_off_allowed: connection.drop_off_allowed,
                        route_index: connection.route_index,
                        from_stop_index: connection.from_stop_index,
                        to_stop_index: connection.to_stop_index,
                        connection_departure_s: connection.departure_s,
                        connection_arrival_s: connection.arrival_s,
                        boarding_count: next_boardings,
                    };
                    if seen_segment_refs.insert(segment_ref) {
                        ensure_transit_service_area_output_room(
                            segment_refs.len(),
                            request.returns.max_stop_segments,
                            "stop segments",
                            SEGMENTS_LIMIT_ADVICE,
                        )?;
                        segment_refs.push(segment_ref);
                    }
                }
            }
        }
    }

    let mut street_seeds = BTreeMap::<u32, u32>::new();
    if request.catchment_mode == TransitCatchmentMode::StreetIsochrone {
        for (state, &arrival) in best.iter() {
            if state.boardings > 0 && state.can_alight && arrival <= time_limit_s {
                let elapsed = arrival.saturating_sub(departure_s);
                street_seeds
                    .entry(state.stop_index)
                    .and_modify(|known| *known = (*known).min(elapsed))
                    .or_insert(elapsed);
            }
        }
    }
    let mut stops = Vec::new();
    if request.returns.include_stops {
        let mut stop_best = BTreeMap::<u32, (u32, u8, StateKey)>::new();
        for (state, arrival_s) in best.iter() {
            let (state, arrival_s) = (*state, *arrival_s);
            if state.connection_index != u32::MAX && !state.can_alight {
                continue;
            }
            if arrival_s > time_limit_s {
                continue;
            }
            let entry =
                stop_best
                    .entry(state.stop_index)
                    .or_insert((arrival_s, state.boardings, state));
            if arrival_s < entry.0 || (arrival_s == entry.0 && state.boardings < entry.1) {
                *entry = (arrival_s, state.boardings, state);
            }
        }
        for (stop_index, (arrival_s, boardings, state)) in stop_best {
            let stop = &runtime.bundle.stops[stop_index as usize];
            ensure_transit_service_area_output_room(
                stops.len(),
                request.returns.max_stops,
                "stops",
                STOPS_LIMIT_ADVICE,
            )?;
            stops.push(TransitServiceAreaStop {
                origin_id: origin.id.clone(),
                stop_id: stop.stop_id.clone(),
                stop_name: stop.name.clone(),
                lon: stop.lon,
                lat: stop.lat,
                arrival_s,
                travel_time_s: arrival_s.saturating_sub(departure_s),
                boarding_count: boardings,
                access_mode: access_mode_for_state(prev, state),
            });
        }
    }

    Ok(OriginSearchOutput {
        skipped_diagnostic: None,
        stops,
        segment_refs,
        street_seeds: street_seeds.into_iter().collect(),
    })
}

/// Backward search state reused across targets on the same rayon worker.
#[derive(Default)]
struct BackwardOriginSearchScratch {
    heap: BinaryHeap<BackwardQueueEntry>,
    best: HashMap<BackwardStateKey, u32>,
    next: HashMap<BackwardStateKey, BackwardStep>,
    seen_segment_refs: HashSet<TransitServiceAreaSegmentRef>,
}

impl BackwardOriginSearchScratch {
    fn reset(&mut self) {
        self.heap.clear();
        self.best.clear();
        self.next.clear();
        self.seen_segment_refs.clear();
    }
}

/// Latest-departure sweep towards one target point. Every settled state is the
/// latest clock time at which the traveller can be at that stop and still
/// reach the target by the deadline.
#[allow(clippy::too_many_arguments)]
fn search_service_area_target(
    runtime: &TransitRuntime<'_>,
    index: &BackwardIndex,
    request: &TransitServiceAreaRequest,
    egress_modes: &[AccessMode],
    max_egress_distance_m: f64,
    deadline_s: u32,
    search_start_s: u32,
    time_floor_s: u32,
    origin_index: usize,
    origin: &TransitPoint,
    scratch: &mut BackwardOriginSearchScratch,
) -> Result<OriginSearchOutput> {
    let egress = best_street_candidates(runtime, origin, egress_modes, &request.modes, true);
    if egress.is_empty() {
        return Ok(OriginSearchOutput {
            skipped_diagnostic: Some(format!(
                "origin '{}' had no transit stop within {:.0} m for egress modes {:?}",
                origin.id,
                max_egress_distance_m,
                egress_modes
                    .iter()
                    .map(|mode| mode.label())
                    .collect::<Vec<_>>()
            )),
            stops: Vec::new(),
            segment_refs: Vec::new(),
            street_seeds: Vec::new(),
        });
    }

    scratch.reset();
    let BackwardOriginSearchScratch {
        heap,
        best,
        next,
        seen_segment_refs,
    } = scratch;
    let mut segment_refs = Vec::new();

    for candidate in egress {
        let Some(time_s) = deadline_s.checked_sub(candidate.time_s) else {
            continue;
        };
        if time_s < time_floor_s {
            continue;
        }
        relax_backward_state(
            heap,
            best,
            next,
            BackwardStateKey {
                stop_index: candidate.stop_index,
                boardings: 0,
                stance: BackwardStance::Alighted,
                transfer_to_connection: u32::MAX,
            },
            time_s,
            BackwardStep::Egress {
                time_s: candidate.time_s,
                mode: candidate.mode,
                // Service-area output does not materialize street paths; avoid
                // retaining a potentially large path per reached state.
                network_path: None,
            },
        );
    }

    while let Some(entry) = heap.pop() {
        if best
            .get(&entry.state)
            .is_some_and(|known| *known > entry.time_s)
        {
            continue;
        }
        if entry.time_s < time_floor_s {
            continue;
        }

        match entry.state.stance {
            BackwardStance::OnFoot => {
                for walk in reverse_transfer_walks(
                    index,
                    runtime.all_transfer_candidates(),
                    entry.state.stop_index,
                    request.modes.max_transfer_distance_m,
                    request.modes.walk_speed_kph,
                ) {
                    if walk.from_stop_index == entry.state.stop_index {
                        continue;
                    }
                    let Some(arrival_limit_s) = entry
                        .time_s
                        .checked_sub(walk.travel_time_s)
                        .and_then(|time_s| time_s.checked_sub(request.modes.transfer_slack_s))
                    else {
                        continue;
                    };
                    if arrival_limit_s < time_floor_s {
                        continue;
                    }
                    relax_backward_state(
                        heap,
                        best,
                        next,
                        BackwardStateKey {
                            stop_index: walk.from_stop_index,
                            boardings: entry.state.boardings,
                            stance: BackwardStance::Alighted,
                            transfer_to_connection: entry.state.transfer_to_connection,
                        },
                        arrival_limit_s,
                        BackwardStep::Transfer {
                            next: entry.state,
                            travel_time_s: walk.travel_time_s,
                            network_path: None,
                        },
                    );
                }
            }
            BackwardStance::Alighted => {
                let boardings = entry.state.boardings.saturating_add(1);
                if boardings > request.modes.max_transfers.saturating_add(1) {
                    continue;
                }
                for (connection_index, connection) in arrivals_at_or_before(
                    index,
                    runtime.bundle,
                    entry.state.stop_index,
                    entry.time_s,
                ) {
                    if connection.arrival_s < search_start_s || connection.arrival_s < time_floor_s
                    {
                        break;
                    }
                    if connection.departure_s < time_floor_s {
                        continue;
                    }
                    if !runtime
                        .backward_transfer_allowed(connection, entry.state.transfer_to_connection)?
                    {
                        continue;
                    }
                    if !connection.drop_off_allowed
                        || !runtime.allowed_routes[connection.route_index as usize]
                    {
                        continue;
                    }
                    ride_backward_in_service_area(
                        request,
                        heap,
                        best,
                        next,
                        seen_segment_refs,
                        &mut segment_refs,
                        origin_index,
                        *connection,
                        connection_index,
                        boardings,
                        entry.state,
                    )?;
                }
            }
            BackwardStance::Aboard(connection_index) => {
                let pickup_allowed = matches!(next.get(&entry.state), Some(BackwardStep::Ride { connection, .. }) if connection.pickup_allowed);
                if pickup_allowed
                    && let Some(board_time_s) =
                        entry.time_s.checked_sub(request.modes.board_slack_s)
                    && board_time_s >= time_floor_s
                {
                    for stance in [BackwardStance::OnFoot, BackwardStance::Alighted] {
                        let slack_s = if stance == BackwardStance::Alighted {
                            request.modes.transfer_slack_s
                        } else {
                            0
                        };
                        let Some(ready_time_s) = board_time_s.checked_sub(slack_s) else {
                            continue;
                        };
                        relax_backward_state(
                            heap,
                            best,
                            next,
                            BackwardStateKey {
                                stop_index: entry.state.stop_index,
                                boardings: entry.state.boardings,
                                stance,
                                transfer_to_connection: runtime
                                    .backward_transfer_context(connection_index),
                            },
                            ready_time_s,
                            BackwardStep::Board { next: entry.state },
                        );
                    }
                }
                if let Some((previous_index, connection)) =
                    previous_run_connection(index, runtime.bundle, connection_index)
                    && connection.arrival_s >= search_start_s
                    && connection.departure_s >= time_floor_s
                {
                    ride_backward_in_service_area(
                        request,
                        heap,
                        best,
                        next,
                        seen_segment_refs,
                        &mut segment_refs,
                        origin_index,
                        *connection,
                        previous_index,
                        entry.state.boardings,
                        entry.state,
                    )?;
                }
            }
        }
    }

    let mut street_seeds = BTreeMap::<u32, u32>::new();
    if request.catchment_mode == TransitCatchmentMode::StreetIsochrone {
        for (state, &departure) in best.iter() {
            if state.boardings > 0
                && !matches!(state.stance, BackwardStance::Aboard(_))
                && departure >= time_floor_s
            {
                let elapsed = deadline_s.saturating_sub(departure);
                street_seeds
                    .entry(state.stop_index)
                    .and_modify(|known| *known = (*known).min(elapsed))
                    .or_insert(elapsed);
            }
        }
    }
    let mut stops = Vec::new();
    if request.returns.include_stops {
        let mut stop_best = BTreeMap::<u32, (u32, u8, BackwardStateKey)>::new();
        for (state, departure_s) in best.iter() {
            let (state, departure_s) = (*state, *departure_s);
            // On-board states sit a boarding slack ahead of the moment the
            // traveller has to be at the stop; the foot and alighted states
            // carry that reachable clock time.
            if matches!(state.stance, BackwardStance::Aboard(_)) || departure_s < time_floor_s {
                continue;
            }
            let entry =
                stop_best
                    .entry(state.stop_index)
                    .or_insert((departure_s, state.boardings, state));
            if departure_s > entry.0 || (departure_s == entry.0 && state.boardings < entry.1) {
                *entry = (departure_s, state.boardings, state);
            }
        }
        for (stop_index, (departure_s, boardings, state)) in stop_best {
            let stop = &runtime.bundle.stops[stop_index as usize];
            ensure_transit_service_area_output_room(
                stops.len(),
                request.returns.max_stops,
                "stops",
                STOPS_LIMIT_ADVICE,
            )?;
            stops.push(TransitServiceAreaStop {
                origin_id: origin.id.clone(),
                stop_id: stop.stop_id.clone(),
                stop_name: stop.name.clone(),
                lon: stop.lon,
                lat: stop.lat,
                arrival_s: departure_s,
                travel_time_s: deadline_s.saturating_sub(departure_s),
                boarding_count: boardings,
                access_mode: egress_mode_for_state(next, state),
            });
        }
    }

    Ok(OriginSearchOutput {
        skipped_diagnostic: None,
        stops,
        segment_refs,
        street_seeds: street_seeds.into_iter().collect(),
    })
}

#[allow(clippy::too_many_arguments)]
fn ride_backward_in_service_area(
    request: &TransitServiceAreaRequest,
    heap: &mut BinaryHeap<BackwardQueueEntry>,
    best: &mut HashMap<BackwardStateKey, u32>,
    next: &mut HashMap<BackwardStateKey, BackwardStep>,
    seen_segment_refs: &mut HashSet<TransitServiceAreaSegmentRef>,
    segment_refs: &mut Vec<TransitServiceAreaSegmentRef>,
    origin_index: usize,
    connection: TransitConnection,
    connection_index: u32,
    boardings: u8,
    from_state: BackwardStateKey,
) -> Result<()> {
    let next_state = BackwardStateKey {
        stop_index: connection.from_stop_index,
        boardings,
        stance: BackwardStance::Aboard(connection_index),
        transfer_to_connection: u32::MAX,
    };
    if best
        .get(&next_state)
        .is_some_and(|known| connection.departure_s <= *known)
    {
        return Ok(());
    }
    relax_backward_state(
        heap,
        best,
        next,
        next_state,
        connection.departure_s,
        BackwardStep::Ride {
            connection,
            next: from_state,
        },
    );
    if request.returns.include_stop_segments {
        let segment_ref = TransitServiceAreaSegmentRef {
            origin_index,
            trip_index: connection.trip_index,
            run_index: connection.run_index,
            pickup_allowed: connection.pickup_allowed,
            drop_off_allowed: connection.drop_off_allowed,
            route_index: connection.route_index,
            from_stop_index: connection.from_stop_index,
            to_stop_index: connection.to_stop_index,
            connection_departure_s: connection.departure_s,
            connection_arrival_s: connection.arrival_s,
            boarding_count: boardings,
        };
        if seen_segment_refs.insert(segment_ref) {
            ensure_transit_service_area_output_room(
                segment_refs.len(),
                request.returns.max_stop_segments,
                "stop segments",
                SEGMENTS_LIMIT_ADVICE,
            )?;
            segment_refs.push(segment_ref);
        }
    }
    Ok(())
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
    anchor: ServiceAreaAnchor,
    segment_refs: Vec<TransitServiceAreaSegmentRef>,
) -> Result<Vec<TransitServiceAreaSegment>> {
    if !request.returns.include_stop_segments {
        return Ok(Vec::new());
    }

    let mut geometry_point_count = 0_usize;
    let mut segments = Vec::with_capacity(segment_refs.len());
    let mut shape_point_cache = ShapePointIndexCache::default();
    for segment_ref in segment_refs {
        let origin = &request.origins[segment_ref.origin_index];
        let segment = transit_service_area_segment(
            bundle,
            origin,
            anchor,
            segment_ref.boarding_count,
            segment_ref.connection(),
            request.returns.include_geometry,
            &mut shape_point_cache,
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

/// Street mode of the egress leg that finishes the journey at the target.
fn egress_mode_for_state(
    next: &HashMap<BackwardStateKey, BackwardStep>,
    mut state: BackwardStateKey,
) -> Option<AccessMode> {
    loop {
        match next.get(&state)? {
            BackwardStep::Egress { mode, .. } => return Some(*mode),
            BackwardStep::Board { next } => state = *next,
            BackwardStep::Ride { next, .. } => state = *next,
            BackwardStep::Transfer { next, .. } => state = *next,
        }
    }
}

fn transit_service_area_segment(
    bundle: &TransitBundle,
    origin: &TransitPoint,
    anchor: ServiceAreaAnchor,
    boarding_count: u8,
    connection: TransitConnection,
    include_geometry: bool,
    shape_point_cache: &mut ShapePointIndexCache,
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
        travel_time_s: anchor.connection_travel_time_s(connection),
        boarding_count,
        mode: Some(route.mode),
        route_id: Some(route.route_id.clone()),
        route_short_name: Some(route.short_name.clone()),
        trip_id: Some(trip.trip_id.clone()),
        geometry: if include_geometry {
            transit_connection_geometry_cached(bundle, connection, shape_point_cache)
        } else {
            Default::default()
        },
    }
}
