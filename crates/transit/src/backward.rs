//! Latest-departure ("arrive by") transit search.
//!
//! The forward scan settles states by earliest arrival; this module mirrors it
//! in reverse, settling states by the latest clock time at which a traveller
//! can still be at a stop and reach the requested target by the deadline. The
//! two scans share the same connection arrays, transfer candidates, street
//! access estimator, and slack rules, so an arrive-by journey obeys exactly
//! the transfer and boarding constraints a depart-after journey does.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

use anyhow::Result;

use crate::legs::{
    build_transit_route_stop_segments, build_transit_route_stops, coalesce_transit_legs,
    reconstruct_arrive_by_legs, seconds_for_distance, summarize_legs, transit_leg_minimums_enabled,
    transit_legs_satisfy_minimums,
};
use crate::model::{
    AccessMode, TransitBundle, TransitConnection, TransitLeg, TransitOutcome,
    TransitRouteAlternative, TransitRouteRequest, TransitRouteResult, TransitRouteSummary,
    TransitStreetPath,
};
use crate::router::{transit_leg_signature, unreachable_transit_route_diagnostic};
use crate::runtime::{StopCandidate, TransitRuntime, best_street_candidates, build_run_links};

/// Egress, transfer, and access legs may sit outside the connection search
/// window; the scan keeps expanding states this far below the window floor,
/// mirroring the forward scan's tail allowance above its window ceiling.
const BACKWARD_WINDOW_TAIL_S: u32 = 6 * 60 * 60;

/// Timetable and transfer indexes the latest-departure scan needs. Connections
/// are referenced by index into `TransitBundle::connections`, so the forward
/// scan's arrays and the bundle itself are never duplicated.
pub(crate) struct BackwardIndex {
    /// Per stop, connections ending there, ordered by arrival time.
    arrivals_by_stop: Vec<Vec<u32>>,
    /// Previous connection on the same vehicle, or u32::MAX at its first stop.
    previous_connection: Vec<u32>,
    /// Per stop, the transfers that end there. Transfer tables are directed,
    /// so the reverse scan cannot read the forward adjacency.
    reverse_transfers: Vec<Vec<ReverseTransfer>>,
}

#[derive(Debug, Clone, Copy)]
struct ReverseTransfer {
    from_stop_index: u32,
    /// Slot inside `transfer_candidates[from_stop_index]`, so the walk time
    /// and the network path are read from the forward table without copying.
    slot: u32,
    distance_m: f64,
}

impl BackwardIndex {
    pub(crate) fn build(
        bundle: &TransitBundle,
        transfer_candidates: &[Vec<StopCandidate>],
    ) -> Self {
        let mut arrivals_by_stop = vec![Vec::<u32>::new(); bundle.stops.len()];
        for (connection_index, connection) in bundle.connections.iter().enumerate() {
            let connection_index = connection_index as u32;
            if let Some(arrivals) = arrivals_by_stop.get_mut(connection.to_stop_index as usize) {
                arrivals.push(connection_index);
            }
        }
        for arrivals in &mut arrivals_by_stop {
            arrivals.sort_unstable_by_key(|index| bundle.connections[*index as usize].arrival_s);
        }
        let previous_connection = build_run_links(bundle).0;
        Self {
            arrivals_by_stop,
            previous_connection,
            reverse_transfers: build_reverse_transfers(transfer_candidates),
        }
    }
}

fn build_reverse_transfers(
    transfer_candidates: &[Vec<StopCandidate>],
) -> Vec<Vec<ReverseTransfer>> {
    let mut reverse = vec![Vec::<ReverseTransfer>::new(); transfer_candidates.len()];
    for (from_stop_index, candidates) in transfer_candidates.iter().enumerate() {
        for (slot, candidate) in candidates.iter().enumerate() {
            if candidate.stop_index as usize == from_stop_index {
                continue;
            }
            let Some(incoming) = reverse.get_mut(candidate.stop_index as usize) else {
                continue;
            };
            incoming.push(ReverseTransfer {
                from_stop_index: from_stop_index as u32,
                slot: slot as u32,
                distance_m: candidate.distance_m,
            });
        }
    }
    for incoming in &mut reverse {
        incoming.sort_by(|left, right| left.distance_m.total_cmp(&right.distance_m));
    }
    reverse
}

/// How the traveller occupies a stop at the state's clock time. The three
/// stances mirror the forward scan's state kinds: a foot arrival may only
/// board, a vehicle arrival may transfer or finish, and an on-board state
/// carries its run so staying aboard stays free of boarding slack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum BackwardStance {
    /// Arrived on foot (access or transfer walk); the only continuation is
    /// boarding a vehicle here.
    OnFoot,
    /// Alighted from a vehicle; may transfer on foot or finish with egress.
    Alighted,
    /// Aboard the indexed connection. Distinguishes repeated visits to a stop.
    Aboard(u32),
}

/// `boardings` counts the vehicle boardings between this state and the target,
/// the mirror of the forward scan's boardings-since-origin counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct BackwardStateKey {
    pub(crate) stop_index: u32,
    pub(crate) boardings: u8,
    pub(crate) stance: BackwardStance,
    /// Departing vehicle retained while searching backward across a walk.
    pub(crate) transfer_to_connection: u32,
}

/// The step that carries a state onward to the target. Chains are stored in
/// target-ward order, so leg reconstruction walks them chronologically.
#[derive(Debug, Clone)]
pub(crate) enum BackwardStep {
    /// The traveller waits at this stop and boards `next`.
    Board { next: BackwardStateKey },
    Ride {
        connection: TransitConnection,
        next: BackwardStateKey,
    },
    Transfer {
        next: BackwardStateKey,
        travel_time_s: u32,
        network_path: Option<TransitStreetPath>,
    },
    Egress {
        time_s: u32,
        mode: AccessMode,
        network_path: Option<TransitStreetPath>,
    },
}

/// Priority queue entry ordered so the latest clock time is settled first.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct BackwardQueueEntry {
    pub(crate) time_s: u32,
    pub(crate) state: BackwardStateKey,
}

impl Ord for BackwardQueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.time_s
            .cmp(&other.time_s)
            .then_with(|| other.state.cmp(&self.state))
    }
}

impl PartialOrd for BackwardQueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

pub(crate) fn relax_backward_state(
    heap: &mut BinaryHeap<BackwardQueueEntry>,
    best: &mut HashMap<BackwardStateKey, u32>,
    next: &mut HashMap<BackwardStateKey, BackwardStep>,
    state: BackwardStateKey,
    time_s: u32,
    step: BackwardStep,
) {
    if best.get(&state).is_none_or(|known| time_s > *known) {
        best.insert(state, time_s);
        next.insert(state, step);
        heap.push(BackwardQueueEntry { time_s, state });
    }
}

/// Connections ending at `stop_index` no later than `limit_s`, latest first.
pub(crate) fn arrivals_at_or_before<'a>(
    index: &'a BackwardIndex,
    bundle: &'a TransitBundle,
    stop_index: u32,
    limit_s: u32,
) -> impl Iterator<Item = (u32, &'a TransitConnection)> {
    let arrivals = index
        .arrivals_by_stop
        .get(stop_index as usize)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let end =
        arrivals.partition_point(|index| bundle.connections[*index as usize].arrival_s <= limit_s);
    arrivals[..end]
        .iter()
        .rev()
        .map(move |index| (*index, &bundle.connections[*index as usize]))
}

/// The adjacent connection on the vehicle, independent of repeated stop IDs.
pub(crate) fn previous_run_connection<'a>(
    index: &BackwardIndex,
    bundle: &'a TransitBundle,
    connection_index: u32,
) -> Option<(u32, &'a TransitConnection)> {
    let previous = *index.previous_connection.get(connection_index as usize)?;
    bundle
        .connections
        .get(previous as usize)
        .map(|connection| (previous, connection))
}

pub(crate) struct ReverseTransferWalk<'a> {
    pub(crate) from_stop_index: u32,
    pub(crate) travel_time_s: u32,
    pub(crate) network_path: Option<&'a TransitStreetPath>,
}

/// Transfer walks that end at `stop_index`, nearest first.
pub(crate) fn reverse_transfer_walks<'a>(
    index: &'a BackwardIndex,
    transfer_candidates: &'a [Vec<StopCandidate>],
    stop_index: u32,
    max_transfer_distance_m: f64,
    walk_speed_kph: f64,
) -> impl Iterator<Item = ReverseTransferWalk<'a>> {
    index
        .reverse_transfers
        .get(stop_index as usize)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .take_while(move |transfer| transfer.distance_m <= max_transfer_distance_m)
        .map(move |transfer| {
            let candidate =
                &transfer_candidates[transfer.from_stop_index as usize][transfer.slot as usize];
            ReverseTransferWalk {
                from_stop_index: transfer.from_stop_index,
                travel_time_s: candidate
                    .transfer_time_s
                    .unwrap_or_else(|| seconds_for_distance(candidate.distance_m, walk_speed_kph)),
                network_path: candidate.network_path.as_ref(),
            }
        })
}

/// Completed journey candidate: the foot state where the traveller boards
/// first, plus the access leg that reaches it from the origin.
#[derive(Debug, Clone)]
struct BackwardFinalCandidate {
    departure_s: u32,
    state: BackwardStateKey,
    access_time_s: u32,
    access_mode: AccessMode,
    access_network_path: Option<TransitStreetPath>,
}

pub(crate) fn execute_transit_route_arrive_by(
    runtime: &TransitRuntime<'_>,
    request: &TransitRouteRequest,
) -> Result<TransitRouteResult> {
    let bundle = runtime.bundle;
    let access_modes = request.modes.validated_access_modes()?;
    let egress_modes = request.modes.validated_egress_modes()?;

    let deadline_s = runtime.request_time_seconds(&request.time.datetime)?;
    let search_start_s = deadline_s.saturating_sub(request.time.search_window_s);
    let access = best_street_candidates(
        runtime,
        request.origin.lon,
        request.origin.lat,
        &access_modes,
        &request.modes,
        false,
    );
    let egress = best_street_candidates(
        runtime,
        request.destination.lon,
        request.destination.lat,
        &egress_modes,
        &request.modes,
        true,
    );
    if access.is_empty() || egress.is_empty() {
        return Ok(TransitRouteResult {
            route_id: request.route_id.clone(),
            time_context: bundle.time_context(),
            outcome: TransitOutcome::Unreachable,
            summary: TransitRouteSummary::default(),
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

    let owned_index;
    let index = match runtime.backward_index {
        Some(index) => index,
        None => {
            owned_index = BackwardIndex::build(bundle, runtime.all_transfer_candidates());
            &owned_index
        }
    };

    let mut heap = BinaryHeap::new();
    let mut best = HashMap::<BackwardStateKey, u32>::new();
    let mut next = HashMap::<BackwardStateKey, BackwardStep>::new();
    for candidate in egress {
        let Some(time_s) = deadline_s.checked_sub(candidate.time_s) else {
            continue;
        };
        relax_backward_state(
            &mut heap,
            &mut best,
            &mut next,
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
                network_path: candidate.network_path,
            },
        );
    }

    let access_by_stop = access
        .into_iter()
        .map(|candidate| (candidate.stop_index, candidate))
        .collect::<HashMap<_, _>>();
    let mut best_final: Option<BackwardFinalCandidate> = None;
    let mut final_candidates = Vec::<BackwardFinalCandidate>::new();
    let collect_alternatives = request.alternatives.max_routes > 1;
    let collect_final_candidates =
        collect_alternatives || transit_leg_minimums_enabled(&request.modes);

    while let Some(entry) = heap.pop() {
        if best
            .get(&entry.state)
            .is_some_and(|known| *known > entry.time_s)
        {
            continue;
        }
        if !transit_leg_minimums_enabled(&request.modes) && !collect_alternatives {
            if let Some(found) = best_final.as_ref()
                && entry.time_s <= found.departure_s
            {
                continue;
            }
        } else if !transit_leg_minimums_enabled(&request.modes)
            && let Some(found) = best_final.as_ref()
        {
            let min_departure_s =
                transit_alternative_departure_limit(deadline_s, found.departure_s, request);
            if entry.time_s < min_departure_s {
                continue;
            }
        }
        if entry.time_s.saturating_add(BACKWARD_WINDOW_TAIL_S) < search_start_s {
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
                    relax_backward_state(
                        &mut heap,
                        &mut best,
                        &mut next,
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
                            network_path: walk.network_path.cloned(),
                        },
                    );
                }
            }
            BackwardStance::Alighted => {
                let boardings = entry.state.boardings.saturating_add(1);
                if boardings > request.modes.max_transfers.saturating_add(1) {
                    continue;
                }
                for (connection_index, connection) in
                    arrivals_at_or_before(index, bundle, entry.state.stop_index, entry.time_s)
                {
                    if connection.arrival_s < search_start_s {
                        break;
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
                    relax_backward_state(
                        &mut heap,
                        &mut best,
                        &mut next,
                        BackwardStateKey {
                            stop_index: connection.from_stop_index,
                            boardings,
                            stance: BackwardStance::Aboard(connection_index),
                            transfer_to_connection: u32::MAX,
                        },
                        connection.departure_s,
                        BackwardStep::Ride {
                            connection: *connection,
                            next: entry.state,
                        },
                    );
                }
            }
            BackwardStance::Aboard(connection_index) => {
                let pickup_allowed = matches!(next.get(&entry.state), Some(BackwardStep::Ride { connection, .. }) if connection.pickup_allowed);
                if pickup_allowed
                    && let Some(board_time_s) =
                        entry.time_s.checked_sub(request.modes.board_slack_s)
                {
                    // Preserve each trip's access candidate before the on-foot
                    // label merges later departures at the same stop.
                    if let Some(access) = access_by_stop.get(&entry.state.stop_index)
                        && let Some(departure_s) = board_time_s.checked_sub(access.time_s)
                    {
                        let candidate = BackwardFinalCandidate {
                            departure_s,
                            state: entry.state,
                            access_time_s: access.time_s,
                            access_mode: access.mode,
                            access_network_path: access.network_path.clone(),
                        };
                        if collect_final_candidates {
                            final_candidates.push(candidate.clone());
                        }
                        if best_final
                            .as_ref()
                            .is_none_or(|found| departure_s > found.departure_s)
                        {
                            best_final = Some(candidate);
                        }
                    }

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
                            &mut heap,
                            &mut best,
                            &mut next,
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
                    previous_run_connection(index, bundle, connection_index)
                    && connection.arrival_s >= search_start_s
                {
                    relax_backward_state(
                        &mut heap,
                        &mut best,
                        &mut next,
                        BackwardStateKey {
                            stop_index: connection.from_stop_index,
                            boardings: entry.state.boardings,
                            stance: BackwardStance::Aboard(previous_index),
                            transfer_to_connection: u32::MAX,
                        },
                        connection.departure_s,
                        BackwardStep::Ride {
                            connection: *connection,
                            next: entry.state,
                        },
                    );
                }
            }
        }
    }

    let found_final_candidate = best_final.is_some();
    let Some((departure_s, arrival_s, mut legs)) = select_arrive_by_final_candidate(
        bundle,
        request,
        &next,
        best_final,
        &mut final_candidates,
    )?
    else {
        return Ok(TransitRouteResult {
            route_id: request.route_id.clone(),
            time_context: bundle.time_context(),
            outcome: TransitOutcome::Unreachable,
            summary: TransitRouteSummary::default(),
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
    let alternatives = build_arrive_by_alternatives(
        bundle,
        request,
        &next,
        deadline_s,
        departure_s,
        &legs,
        final_candidates,
    )?;
    Ok(TransitRouteResult {
        route_id: request.route_id.clone(),
        time_context: bundle.time_context(),
        outcome: TransitOutcome::Scheduled,
        summary,
        legs,
        stops,
        stop_segments,
        diagnostics: Vec::new(),
        alternatives,
    })
}

/// Earliest departure an alternative may use, mirroring the depart-after
/// arrival ceiling around the requested deadline.
fn transit_alternative_departure_limit(
    deadline_s: u32,
    best_departure_s: u32,
    request: &TransitRouteRequest,
) -> u32 {
    let best_duration = deadline_s.saturating_sub(best_departure_s).max(1);
    let ratio_limit = deadline_s
        .saturating_sub((best_duration as f64 * request.alternatives.max_time_ratio) as u32);
    let extra_limit = request
        .alternatives
        .max_extra_time_s
        .map(|extra| best_departure_s.saturating_sub(extra))
        .unwrap_or(0);
    ratio_limit.max(extra_limit)
}

fn select_arrive_by_final_candidate(
    bundle: &TransitBundle,
    request: &TransitRouteRequest,
    next: &HashMap<BackwardStateKey, BackwardStep>,
    best_final: Option<BackwardFinalCandidate>,
    final_candidates: &mut [BackwardFinalCandidate],
) -> Result<Option<(u32, u32, Vec<TransitLeg>)>> {
    if !transit_leg_minimums_enabled(&request.modes) {
        let Some(candidate) = best_final else {
            return Ok(None);
        };
        let (arrival_s, legs) = reconstruct_arrive_by_legs(
            bundle,
            request,
            next,
            candidate.state,
            candidate.departure_s,
            candidate.access_time_s,
            candidate.access_mode,
            candidate.access_network_path,
        )?;
        return Ok(Some((candidate.departure_s, arrival_s, legs)));
    }

    sort_arrive_by_candidates(final_candidates);
    for candidate in final_candidates.iter() {
        let (arrival_s, legs) = reconstruct_arrive_by_legs(
            bundle,
            request,
            next,
            candidate.state,
            candidate.departure_s,
            candidate.access_time_s,
            candidate.access_mode,
            candidate.access_network_path.clone(),
        )?;
        let mut coalesced = legs.clone();
        coalesce_transit_legs(&mut coalesced);
        if transit_legs_satisfy_minimums(bundle, &coalesced, &request.modes) {
            return Ok(Some((candidate.departure_s, arrival_s, legs)));
        }
    }

    Ok(None)
}

fn sort_arrive_by_candidates(candidates: &mut [BackwardFinalCandidate]) {
    candidates.sort_by_key(|candidate| {
        (
            std::cmp::Reverse(candidate.departure_s),
            candidate.state.boardings,
        )
    });
}

fn build_arrive_by_alternatives(
    bundle: &TransitBundle,
    request: &TransitRouteRequest,
    next: &HashMap<BackwardStateKey, BackwardStep>,
    deadline_s: u32,
    best_departure_s: u32,
    best_legs: &[TransitLeg],
    mut final_candidates: Vec<BackwardFinalCandidate>,
) -> Result<Vec<TransitRouteAlternative>> {
    if request.alternatives.max_routes <= 1 {
        return Ok(Vec::new());
    }
    sort_arrive_by_candidates(&mut final_candidates);
    let mut alternatives = Vec::new();
    let mut signatures = HashSet::new();
    signatures.insert(transit_leg_signature(best_legs));
    let departure_limit =
        transit_alternative_departure_limit(deadline_s, best_departure_s, request);

    for candidate in final_candidates {
        if alternatives.len() + 1 >= request.alternatives.max_routes {
            break;
        }
        if candidate.departure_s < departure_limit {
            continue;
        }
        let (arrival_s, mut legs) = reconstruct_arrive_by_legs(
            bundle,
            request,
            next,
            candidate.state,
            candidate.departure_s,
            candidate.access_time_s,
            candidate.access_mode,
            candidate.access_network_path,
        )?;
        coalesce_transit_legs(&mut legs);
        if !transit_legs_satisfy_minimums(bundle, &legs, &request.modes) {
            continue;
        }
        let signature = transit_leg_signature(&legs);
        if !signatures.insert(signature) {
            continue;
        }
        let summary = summarize_legs(candidate.departure_s, arrival_s, &legs);
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
