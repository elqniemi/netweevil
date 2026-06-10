use std::collections::HashMap;

use anyhow::Result;

use crate::model::{
    TransitBundle, TransitConnection, TransitLeg, TransitLegType, TransitModeOptions,
    TransitRouteRequest, TransitRouteStop, TransitRouteStopSegment, TransitRouteSummary,
    TransitShape,
};
use crate::runtime::{PrevStep, StateKey, TransitRuntime};

pub(crate) fn reconstruct_legs(
    bundle: &TransitBundle,
    runtime: &TransitRuntime,
    request: &TransitRouteRequest,
    prev: &HashMap<StateKey, PrevStep>,
    final_state: StateKey,
    arrival_s: u32,
    egress_walk_s: u32,
) -> Result<Vec<TransitLeg>> {
    let final_stop = &bundle.stops[final_state.stop_index as usize];
    let mut legs = vec![TransitLeg {
        leg_type: TransitLegType::Egress,
        from_id: final_stop.stop_id.clone(),
        to_id: request.destination.id.clone(),
        from_name: final_stop.name.clone(),
        to_name: request.destination.id.clone(),
        departure_s: arrival_s.saturating_sub(egress_walk_s),
        arrival_s,
        mode: None,
        route_id: None,
        route_short_name: None,
        trip_id: None,
        headsign: None,
        geometry: geometry_if_requested(
            request,
            [final_stop.lon, final_stop.lat],
            [request.destination.lon, request.destination.lat],
        ),
    }];
    let mut cursor = final_state;
    loop {
        let Some(step) = prev.get(&cursor) else {
            break;
        };
        match step {
            PrevStep::Access {
                from_id,
                from_name,
                from_lon,
                from_lat,
                distance_m,
                departure_s,
            } => {
                let stop = &bundle.stops[cursor.stop_index as usize];
                let walk_s = seconds_for_distance(*distance_m, request.modes.walk_speed_kph);
                legs.push(TransitLeg {
                    leg_type: TransitLegType::Access,
                    from_id: from_id.clone(),
                    to_id: stop.stop_id.clone(),
                    from_name: from_name.clone(),
                    to_name: stop.name.clone(),
                    departure_s: *departure_s,
                    arrival_s: departure_s.saturating_add(walk_s),
                    mode: None,
                    route_id: None,
                    route_short_name: None,
                    trip_id: None,
                    headsign: None,
                    geometry: geometry_if_requested(
                        request,
                        [*from_lon, *from_lat],
                        [stop.lon, stop.lat],
                    ),
                });
                break;
            }
            PrevStep::Transfer {
                previous,
                distance_m,
                departure_s,
            } => {
                let from = &bundle.stops[previous.stop_index as usize];
                let to = &bundle.stops[cursor.stop_index as usize];
                let walk_s = seconds_for_distance(*distance_m, request.modes.walk_speed_kph);
                legs.push(TransitLeg {
                    leg_type: TransitLegType::Transfer,
                    from_id: from.stop_id.clone(),
                    to_id: to.stop_id.clone(),
                    from_name: from.name.clone(),
                    to_name: to.name.clone(),
                    departure_s: *departure_s,
                    arrival_s: departure_s.saturating_add(walk_s),
                    mode: None,
                    route_id: None,
                    route_short_name: None,
                    trip_id: None,
                    headsign: None,
                    geometry: geometry_if_requested(
                        request,
                        [from.lon, from.lat],
                        [to.lon, to.lat],
                    ),
                });
                cursor = *previous;
            }
            PrevStep::Transit {
                previous,
                connection,
            } => {
                let from = &bundle.stops[connection.from_stop_index as usize];
                let to = &bundle.stops[connection.to_stop_index as usize];
                let route = &bundle.routes[connection.route_index as usize];
                let trip = &bundle.trips[connection.trip_index as usize];
                let _ = runtime;
                legs.push(TransitLeg {
                    leg_type: TransitLegType::Transit,
                    from_id: from.stop_id.clone(),
                    to_id: to.stop_id.clone(),
                    from_name: from.name.clone(),
                    to_name: to.name.clone(),
                    departure_s: connection.departure_s,
                    arrival_s: connection.arrival_s,
                    mode: Some(route.mode),
                    route_id: Some(route.route_id.clone()),
                    route_short_name: Some(route.short_name.clone()),
                    trip_id: Some(trip.trip_id.clone()),
                    headsign: Some(trip.headsign.clone()),
                    geometry: transit_connection_geometry_if_requested(
                        request,
                        bundle,
                        *connection,
                    ),
                });
                cursor = *previous;
            }
        }
    }
    legs.reverse();
    Ok(legs)
}

pub(crate) fn coalesce_transit_legs(legs: &mut Vec<TransitLeg>) {
    let mut coalesced = Vec::<TransitLeg>::new();
    for leg in legs.drain(..) {
        if let Some(last) = coalesced.last_mut() {
            if last.leg_type == TransitLegType::Transit
                && leg.leg_type == TransitLegType::Transit
                && last.trip_id == leg.trip_id
                && last.to_id == leg.from_id
            {
                last.to_id = leg.to_id;
                last.to_name = leg.to_name;
                last.arrival_s = leg.arrival_s;
                last.geometry.extend(leg.geometry.into_iter().skip(1));
                continue;
            }
        }
        coalesced.push(leg);
    }
    *legs = coalesced;
}

pub(crate) fn build_transit_route_stops(legs: &[TransitLeg]) -> Vec<TransitRouteStop> {
    let mut stops = Vec::new();
    for leg in legs
        .iter()
        .filter(|leg| leg.leg_type == TransitLegType::Transit)
    {
        let Some((&from, &to)) = leg.geometry.first().zip(leg.geometry.last()) else {
            continue;
        };
        if stops
            .last()
            .is_none_or(|stop: &TransitRouteStop| stop.stop_id != leg.from_id)
        {
            stops.push(TransitRouteStop {
                sequence: stops.len() as u32 + 1,
                stop_id: leg.from_id.clone(),
                stop_name: leg.from_name.clone(),
                lon: from[0],
                lat: from[1],
                arrival_s: None,
                departure_s: Some(leg.departure_s),
                mode: leg.mode,
                route_id: leg.route_id.clone(),
                route_short_name: leg.route_short_name.clone(),
                trip_id: leg.trip_id.clone(),
                headsign: leg.headsign.clone(),
            });
        } else if let Some(previous) = stops.last_mut() {
            previous.departure_s = Some(leg.departure_s);
            previous.mode = leg.mode;
            previous.route_id = leg.route_id.clone();
            previous.route_short_name = leg.route_short_name.clone();
            previous.trip_id = leg.trip_id.clone();
            previous.headsign = leg.headsign.clone();
        }
        stops.push(TransitRouteStop {
            sequence: stops.len() as u32 + 1,
            stop_id: leg.to_id.clone(),
            stop_name: leg.to_name.clone(),
            lon: to[0],
            lat: to[1],
            arrival_s: Some(leg.arrival_s),
            departure_s: None,
            mode: leg.mode,
            route_id: leg.route_id.clone(),
            route_short_name: leg.route_short_name.clone(),
            trip_id: leg.trip_id.clone(),
            headsign: leg.headsign.clone(),
        });
    }
    stops
}

pub(crate) fn build_transit_route_stop_segments(
    legs: &[TransitLeg],
) -> Vec<TransitRouteStopSegment> {
    legs.iter()
        .filter(|leg| leg.leg_type == TransitLegType::Transit)
        .enumerate()
        .map(|(index, leg)| TransitRouteStopSegment {
            segment_index: index as u32 + 1,
            from_stop_id: leg.from_id.clone(),
            to_stop_id: leg.to_id.clone(),
            from_stop_name: leg.from_name.clone(),
            to_stop_name: leg.to_name.clone(),
            departure_s: leg.departure_s,
            arrival_s: leg.arrival_s,
            duration_s: leg.arrival_s.saturating_sub(leg.departure_s),
            mode: leg.mode,
            route_id: leg.route_id.clone(),
            route_short_name: leg.route_short_name.clone(),
            trip_id: leg.trip_id.clone(),
            headsign: leg.headsign.clone(),
            geometry: leg.geometry.clone(),
        })
        .collect()
}

pub(crate) fn transit_leg_minimums_enabled(modes: &TransitModeOptions) -> bool {
    modes.min_transit_leg_duration_s > 0 || modes.min_transit_leg_distance_m > 0.0
}

pub(crate) fn transit_legs_satisfy_minimums(
    bundle: &TransitBundle,
    legs: &[TransitLeg],
    modes: &TransitModeOptions,
) -> bool {
    legs.iter()
        .filter(|leg| leg.leg_type == TransitLegType::Transit)
        .all(|leg| transit_leg_satisfies_minimums(bundle, leg, modes))
}

fn transit_leg_satisfies_minimums(
    bundle: &TransitBundle,
    leg: &TransitLeg,
    modes: &TransitModeOptions,
) -> bool {
    if !transit_leg_minimums_enabled(modes) {
        return true;
    }

    let duration_ok = modes.min_transit_leg_duration_s == 0
        || leg.arrival_s.saturating_sub(leg.departure_s) >= modes.min_transit_leg_duration_s;
    let distance_ok = modes.min_transit_leg_distance_m <= 0.0
        || transit_leg_distance_m(bundle, leg)
            .is_some_and(|distance_m| distance_m >= modes.min_transit_leg_distance_m);

    if modes.min_transit_leg_duration_s > 0 && modes.min_transit_leg_distance_m > 0.0 {
        duration_ok || distance_ok
    } else {
        duration_ok && distance_ok
    }
}

fn transit_leg_distance_m(bundle: &TransitBundle, leg: &TransitLeg) -> Option<f64> {
    if leg.geometry.len() >= 2 {
        return Some(linestring_distance_m(&leg.geometry));
    }

    let from = bundle
        .stops
        .iter()
        .find(|stop| stop.stop_id == leg.from_id)?;
    let to = bundle.stops.iter().find(|stop| stop.stop_id == leg.to_id)?;
    Some(haversine_m(from.lon, from.lat, to.lon, to.lat))
}

fn linestring_distance_m(points: &[[f64; 2]]) -> f64 {
    points
        .windows(2)
        .map(|window| haversine_m(window[0][0], window[0][1], window[1][0], window[1][1]))
        .sum()
}

pub(crate) fn summarize_legs(
    departure_s: u32,
    arrival_s: u32,
    legs: &[TransitLeg],
) -> TransitRouteSummary {
    let mut summary = TransitRouteSummary {
        departure_s,
        arrival_s: Some(arrival_s),
        total_travel_time_s: Some(arrival_s.saturating_sub(departure_s)),
        ..TransitRouteSummary::default()
    };
    let mut previous_arrival = departure_s;
    for leg in legs {
        if leg.departure_s > previous_arrival {
            summary.wait_time_s += leg.departure_s - previous_arrival;
        }
        let duration = leg.arrival_s.saturating_sub(leg.departure_s);
        match leg.leg_type {
            TransitLegType::Access | TransitLegType::Egress => {
                summary.access_egress_time_s += duration
            }
            TransitLegType::Transfer => summary.transfer_time_s += duration,
            TransitLegType::Transit => {
                summary.transit_time_s += duration;
                summary.boarding_count += 1;
            }
        }
        previous_arrival = leg.arrival_s;
    }
    summary
}

fn geometry_if_requested(
    request: &TransitRouteRequest,
    from: [f64; 2],
    to: [f64; 2],
) -> Vec<[f64; 2]> {
    if transit_geometry_requested(request) {
        vec![from, to]
    } else {
        Vec::new()
    }
}

fn transit_connection_geometry_if_requested(
    request: &TransitRouteRequest,
    bundle: &TransitBundle,
    connection: TransitConnection,
) -> Vec<[f64; 2]> {
    if transit_geometry_requested(request) {
        transit_connection_geometry(bundle, connection)
    } else {
        Vec::new()
    }
}

fn transit_geometry_requested(request: &TransitRouteRequest) -> bool {
    request.returns.include_geometry
        || request.returns.include_stops
        || request.returns.include_stop_segments
}

pub(crate) fn transit_connection_geometry(
    bundle: &TransitBundle,
    connection: TransitConnection,
) -> Vec<[f64; 2]> {
    let from = &bundle.stops[connection.from_stop_index as usize];
    let to = &bundle.stops[connection.to_stop_index as usize];
    let from_point = [from.lon, from.lat];
    let to_point = [to.lon, to.lat];
    let Some(shape_index) = bundle.trips[connection.trip_index as usize].shape_index else {
        return vec![from_point, to_point];
    };
    let Some(shape) = bundle.shapes.get(shape_index as usize) else {
        return vec![from_point, to_point];
    };
    shape_geometry_between_stops(shape, from_point, to_point)
}

fn shape_geometry_between_stops(
    shape: &TransitShape,
    from: [f64; 2],
    to: [f64; 2],
) -> Vec<[f64; 2]> {
    if shape.points.len() < 2 {
        return vec![from, to];
    }
    let from_index = nearest_shape_point_index(&shape.points, from);
    let to_index = nearest_shape_point_index(&shape.points, to);
    let mut geometry = Vec::new();
    push_unique_point(&mut geometry, from);
    if from_index <= to_index {
        for point in &shape.points[from_index..=to_index] {
            push_unique_point(&mut geometry, *point);
        }
    } else {
        for point in shape.points[to_index..=from_index].iter().rev() {
            push_unique_point(&mut geometry, *point);
        }
    }
    push_unique_point(&mut geometry, to);
    if geometry.len() < 2 {
        vec![from, to]
    } else {
        geometry
    }
}

fn nearest_shape_point_index(points: &[[f64; 2]], target: [f64; 2]) -> usize {
    points
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            point_distance_key(**left, target).total_cmp(&point_distance_key(**right, target))
        })
        .map(|(index, _)| index)
        .unwrap_or_default()
}

fn point_distance_key(left: [f64; 2], right: [f64; 2]) -> f64 {
    let mean_lat = ((left[1] + right[1]) * 0.5).to_radians();
    let lon_scale = mean_lat.cos().max(0.01);
    let dx = (left[0] - right[0]) * lon_scale;
    let dy = left[1] - right[1];
    dx.mul_add(dx, dy * dy)
}

fn push_unique_point(points: &mut Vec<[f64; 2]>, point: [f64; 2]) {
    if points
        .last()
        .is_none_or(|last| (last[0] - point[0]).abs() > 1e-10 || (last[1] - point[1]).abs() > 1e-10)
    {
        points.push(point);
    }
}

pub(crate) fn seconds_for_distance(distance_m: f64, speed_kph: f64) -> u32 {
    if speed_kph <= 0.0 {
        return u32::MAX / 4;
    }
    ((distance_m / (speed_kph * 1000.0 / 3600.0)).ceil() as u32).max(1)
}

pub(crate) fn haversine_m(lon_a: f64, lat_a: f64, lon_b: f64, lat_b: f64) -> f64 {
    let earth_radius_m = 6_371_000.0_f64;
    let lat1 = lat_a.to_radians();
    let lat2 = lat_b.to_radians();
    let dlat = (lat_b - lat_a).to_radians();
    let dlon = (lon_b - lon_a).to_radians();
    let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * earth_radius_m * a.sqrt().asin()
}
