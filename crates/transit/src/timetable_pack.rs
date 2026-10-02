//! Compact persisted form of a bundle's scheduled connections.
//!
//! A feed's connections repeat a small set of stop patterns: every service
//! date and every frequency departure of a trip produces another vehicle run
//! with the same stops and the same relative times. Large feeds therefore
//! carry millions of connections but only tens of thousands of patterns.
//!
//! The packed section stores each distinct run pattern once (stops, pickup
//! and drop-off flags, and times relative to the run's first departure) and
//! each run as `(run_index, pattern, first departure)`, all varint and delta
//! coded. Loading expands the runs in run order and stably sorts them by
//! departure, which reproduces the bundle order exactly when that order is
//! `(departure_s, run_index)` with ties kept in run order, the order GTFS
//! import produces. Bundles in any other order, or with runs that change trip
//! or route midway, are stored as plain bincode instead.

use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};

use anyhow::{Result, anyhow, bail, ensure};
use rayon::prelude::*;

use crate::model::TransitConnection;

const KIND_BINCODE: u8 = 0;
const KIND_PATTERNS: u8 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct PatternStep {
    from_stop_index: u32,
    to_stop_index: u32,
    pickup_allowed: bool,
    drop_off_allowed: bool,
    departure_offset_s: u32,
    arrival_s_minus_departure_s: i64,
}

struct Pattern {
    trip_index: u32,
    route_index: u32,
    steps: Vec<PatternStep>,
}

pub(crate) fn encode_connections(connections: &[TransitConnection]) -> Result<Vec<u8>> {
    if let Some(packed) = encode_patterns(connections) {
        return Ok(packed);
    }
    let mut out = vec![KIND_BINCODE];
    bincode::serialize_into(&mut out, connections)?;
    Ok(out)
}

/// Bounds used to validate indexes while decoding.
pub(crate) struct ConnectionBounds {
    pub(crate) stops: usize,
    pub(crate) routes: usize,
    pub(crate) trips: usize,
}

pub(crate) fn decode_connections(
    bytes: &[u8],
    bounds: &ConnectionBounds,
) -> Result<Vec<TransitConnection>> {
    let (&kind, payload) = bytes
        .split_first()
        .ok_or_else(|| anyhow!("connection section is empty"))?;
    let connections: Vec<TransitConnection> = match kind {
        KIND_BINCODE => bincode::deserialize(payload)?,
        KIND_PATTERNS => decode_patterns(payload)?,
        other => bail!("unknown connection encoding {other}"),
    };
    ensure!(
        connections.iter().all(|connection| {
            (connection.from_stop_index as usize) < bounds.stops
                && (connection.to_stop_index as usize) < bounds.stops
                && (connection.route_index as usize) < bounds.routes
                && (connection.trip_index as usize) < bounds.trips
        }),
        "connection references an unknown stop, route, or trip"
    );
    Ok(connections)
}

fn encode_patterns(connections: &[TransitConnection]) -> Option<Vec<u8>> {
    // Stable sort by departure must restore the bundle order from runs
    // expanded in run order.
    if !connections.windows(2).all(|pair| {
        (pair[0].departure_s, pair[0].run_index) <= (pair[1].departure_s, pair[1].run_index)
    }) {
        return None;
    }
    let by_run = order_by_run(connections);

    // Patterns are looked up by hash without allocating per run; equal hashes
    // are confirmed by comparing the steps.
    let mut patterns = Vec::<Pattern>::new();
    let mut pattern_ids = HashMap::<u64, Vec<u32>>::new();
    let mut runs = Vec::<(u32, u32, u32)>::new();
    let mut steps = Vec::<PatternStep>::new();
    let hasher = RandomState::new();
    for run in by_run.chunk_by(|left, right| {
        connections[*left as usize].run_index == connections[*right as usize].run_index
    }) {
        let first = connections[run[0] as usize];
        steps.clear();
        for index in run {
            let connection = connections[*index as usize];
            if connection.trip_index != first.trip_index
                || connection.route_index != first.route_index
            {
                return None;
            }
            steps.push(PatternStep {
                from_stop_index: connection.from_stop_index,
                to_stop_index: connection.to_stop_index,
                pickup_allowed: connection.pickup_allowed,
                drop_off_allowed: connection.drop_off_allowed,
                departure_offset_s: connection.departure_s - first.departure_s,
                arrival_s_minus_departure_s: i64::from(connection.arrival_s)
                    - i64::from(connection.departure_s),
            });
        }
        let hash = hasher.hash_one((first.trip_index, first.route_index, steps.as_slice()));
        let candidates = pattern_ids.entry(hash).or_default();
        let existing = candidates.iter().copied().find(|id| {
            let pattern = &patterns[*id as usize];
            pattern.trip_index == first.trip_index
                && pattern.route_index == first.route_index
                && pattern.steps == steps
        });
        let id = existing.unwrap_or_else(|| {
            let id = patterns.len() as u32;
            patterns.push(Pattern {
                trip_index: first.trip_index,
                route_index: first.route_index,
                steps: steps.clone(),
            });
            candidates.push(id);
            id
        });
        runs.push((first.run_index, id, first.departure_s));
    }

    let mut out = vec![KIND_PATTERNS];
    put_varint(&mut out, connections.len() as u64);
    put_varint(&mut out, patterns.len() as u64);
    for pattern in &patterns {
        put_varint(&mut out, u64::from(pattern.trip_index));
        put_varint(&mut out, u64::from(pattern.route_index));
        put_varint(&mut out, pattern.steps.len() as u64);
        let mut previous_to = None;
        let mut previous_arrival_offset = 0_i64;
        for step in &pattern.steps {
            let chained = previous_to == Some(step.from_stop_index);
            // Head: target stop delta, then flags for an explicit origin stop
            // (default: the previous step's target), pickup and drop-off.
            put_varint(
                &mut out,
                (zigzag(i64::from(step.to_stop_index) - i64::from(step.from_stop_index)) << 3)
                    | (u64::from(!chained) << 2)
                    | (u64::from(step.pickup_allowed) << 1)
                    | u64::from(step.drop_off_allowed),
            );
            if !chained {
                put_varint(&mut out, u64::from(step.from_stop_index));
            }
            let departure_offset = i64::from(step.departure_offset_s);
            put_varint(&mut out, zigzag(departure_offset - previous_arrival_offset));
            put_varint(&mut out, zigzag(step.arrival_s_minus_departure_s));
            previous_to = Some(step.to_stop_index);
            previous_arrival_offset = departure_offset + step.arrival_s_minus_departure_s;
        }
    }
    put_varint(&mut out, runs.len() as u64);
    let (mut previous_run, mut previous_pattern, mut previous_departure) = (0_i64, 0_i64, 0_i64);
    for (run_index, pattern, departure_s) in runs {
        put_varint(&mut out, zigzag(i64::from(run_index) - previous_run));
        put_varint(&mut out, zigzag(i64::from(pattern) - previous_pattern));
        put_varint(
            &mut out,
            zigzag(i64::from(departure_s) - previous_departure),
        );
        (previous_run, previous_pattern, previous_departure) = (
            i64::from(run_index),
            i64::from(pattern),
            i64::from(departure_s),
        );
    }
    Some(out)
}

fn decode_patterns(bytes: &[u8]) -> Result<Vec<TransitConnection>> {
    let mut cursor = Cursor { bytes, offset: 0 };
    let connection_count = usize::try_from(cursor.varint()?)?;
    let pattern_count = cursor.length()?;
    let mut pattern_offsets = Vec::with_capacity(pattern_count + 1);
    let mut pattern_routes = Vec::with_capacity(pattern_count);
    let mut steps = Vec::<PatternStep>::new();
    pattern_offsets.push(0);
    for _ in 0..pattern_count {
        let trip_index = cursor.u32()?;
        let route_index = cursor.u32()?;
        pattern_routes.push((trip_index, route_index));
        let step_count = cursor.length()?;
        let mut previous_to = None;
        let mut previous_arrival_offset = 0_i64;
        for _ in 0..step_count {
            let head = cursor.varint()?;
            let from_stop_index = if head & 4 == 0 {
                previous_to.ok_or_else(|| anyhow!("pattern starts with a chained step"))?
            } else {
                cursor.u32()?
            };
            let to_stop_index = u32::try_from(i64::from(from_stop_index) + unzigzag(head >> 3))?;
            let departure_offset = previous_arrival_offset + unzigzag(cursor.varint()?);
            let travel = unzigzag(cursor.varint()?);
            steps.push(PatternStep {
                from_stop_index,
                to_stop_index,
                pickup_allowed: head & 2 != 0,
                drop_off_allowed: head & 1 != 0,
                departure_offset_s: u32::try_from(departure_offset)?,
                arrival_s_minus_departure_s: travel,
            });
            previous_to = Some(to_stop_index);
            previous_arrival_offset = departure_offset + travel;
        }
        pattern_offsets.push(steps.len());
    }

    let run_count = cursor.length()?;
    let mut runs = Vec::with_capacity(run_count);
    let mut expanded_count = 0_usize;
    let (mut run_index, mut pattern, mut departure) = (0_i64, 0_i64, 0_i64);
    for _ in 0..run_count {
        run_index += unzigzag(cursor.varint()?);
        pattern += unzigzag(cursor.varint()?);
        departure += unzigzag(cursor.varint()?);
        let pattern_index = usize::try_from(pattern)?;
        ensure!(
            pattern_index < pattern_count,
            "run references an unknown pattern"
        );
        expanded_count = expanded_count
            .checked_add(pattern_offsets[pattern_index + 1] - pattern_offsets[pattern_index])
            .ok_or_else(|| anyhow!("connection count overflows"))?;
        runs.push((
            u32::try_from(run_index)?,
            pattern_index,
            u32::try_from(departure)?,
        ));
    }
    ensure!(
        expanded_count == connection_count,
        "connection section expands to {expanded_count} connections; expected {connection_count}"
    );
    let mut connections = Vec::with_capacity(connection_count);
    for (run, pattern_index, base) in runs {
        let (trip_index, route_index) = pattern_routes[pattern_index];
        for step in &steps[pattern_offsets[pattern_index]..pattern_offsets[pattern_index + 1]] {
            let departure_s = base
                .checked_add(step.departure_offset_s)
                .ok_or_else(|| anyhow!("connection departure overflows"))?;
            let arrival_s =
                u32::try_from(i64::from(departure_s) + step.arrival_s_minus_departure_s)?;
            connections.push(TransitConnection {
                trip_index,
                run_index: run,
                pickup_allowed: step.pickup_allowed,
                drop_off_allowed: step.drop_off_allowed,
                route_index,
                from_stop_index: step.from_stop_index,
                to_stop_index: step.to_stop_index,
                departure_s,
                arrival_s,
            });
        }
    }
    ensure!(
        cursor.offset == bytes.len(),
        "connection section has trailing bytes"
    );
    // Runs were expanded in run order, so a stable sort restores the
    // (departure_s, run_index) bundle order with ties kept in run order.
    connections.par_sort_by_key(|connection| connection.departure_s);
    Ok(connections)
}

/// Connection indexes grouped by run in run order, keeping bundle order
/// inside each run. Dense run numbering uses a counting sort.
fn order_by_run(connections: &[TransitConnection]) -> Vec<u32> {
    let run_count = connections
        .iter()
        .map(|connection| connection.run_index as usize + 1)
        .max()
        .unwrap_or_default();
    if run_count > connections.len().saturating_mul(4).max(1024) {
        let mut order = (0..connections.len() as u32).collect::<Vec<_>>();
        order.sort_by_key(|index| connections[*index as usize].run_index);
        return order;
    }
    let mut next_slot = vec![0_u32; run_count + 1];
    for connection in connections {
        next_slot[connection.run_index as usize + 1] += 1;
    }
    for run in 1..next_slot.len() {
        next_slot[run] += next_slot[run - 1];
    }
    let mut order = vec![0_u32; connections.len()];
    for (index, connection) in connections.iter().enumerate() {
        let slot = &mut next_slot[connection.run_index as usize];
        order[*slot as usize] = index as u32;
        *slot += 1;
    }
    order
}

fn zigzag(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

fn unzigzag(value: u64) -> i64 {
    ((value >> 1) as i64) ^ -((value & 1) as i64)
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Cursor<'_> {
    fn varint(&mut self) -> Result<u64> {
        let mut value = 0_u64;
        for shift in (0..64).step_by(7) {
            let byte = *self
                .bytes
                .get(self.offset)
                .ok_or_else(|| anyhow!("connection section is truncated"))?;
            self.offset += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        bail!("connection section has an overlong varint")
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::try_from(self.varint()?)?)
    }

    /// Element counts: each element takes at least one byte, so a count
    /// larger than the section cannot be genuine.
    fn length(&mut self) -> Result<usize> {
        let length = usize::try_from(self.varint()?)?;
        ensure!(
            length <= self.bytes.len(),
            "connection section declares an impossible length"
        );
        Ok(length)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: ConnectionBounds = ConnectionBounds {
        stops: 8,
        routes: 3,
        trips: 4,
    };

    fn connection(run: u32, trip: u32, from: u32, to: u32, departure: u32) -> TransitConnection {
        TransitConnection {
            trip_index: trip,
            run_index: run,
            pickup_allowed: from != 3,
            drop_off_allowed: to != 5,
            route_index: trip % 3,
            from_stop_index: from,
            to_stop_index: to,
            departure_s: departure,
            arrival_s: departure + 120 + from,
        }
    }

    fn sorted(mut connections: Vec<TransitConnection>) -> Vec<TransitConnection> {
        connections.sort_by_key(|connection| connection.departure_s);
        connections
    }

    fn round_trip(connections: &[TransitConnection]) -> (u8, Vec<TransitConnection>) {
        let bytes = encode_connections(connections).unwrap();
        (bytes[0], decode_connections(&bytes, &BOUNDS).unwrap())
    }

    fn same(left: &[TransitConnection], right: &[TransitConnection]) -> bool {
        bincode::serialize(left).unwrap() == bincode::serialize(right).unwrap()
    }

    #[test]
    fn repeated_runs_pack_into_patterns_and_restore_bundle_order() {
        // Run-major generation like GTFS import: two trips, several runs each,
        // with simultaneous departures, zero-length hops, and a repeated stop.
        let mut generated = Vec::new();
        for (run, start) in [0_u32, 600, 600, 7_200].into_iter().enumerate() {
            let run = run as u32;
            let trip = run / 2;
            generated.push(connection(run, trip, 0, 3, start));
            generated.push(connection(run, trip, 3, 5, start + 300));
            generated.push(connection(run, trip, 5, 3, start + 300));
        }
        generated.push(connection(4, 3, 7, 1, 600));
        let connections = sorted(generated);

        let (kind, decoded) = round_trip(&connections);
        assert_eq!(kind, KIND_PATTERNS);
        assert!(same(&decoded, &connections));
    }

    #[test]
    fn unusual_orders_and_runs_fall_back_to_plain_connections() {
        let canonical = sorted(vec![
            connection(0, 0, 0, 1, 60),
            connection(1, 1, 2, 3, 60),
            connection(0, 0, 1, 2, 300),
        ]);
        // Equal departures listed against run order.
        let mut reordered = canonical.clone();
        reordered.swap(0, 1);
        // A run that changes trip midway.
        let mut mixed = canonical.clone();
        mixed[2].trip_index = 2;
        for connections in [reordered, mixed] {
            let (kind, decoded) = round_trip(&connections);
            assert_eq!(kind, KIND_BINCODE);
            assert!(same(&decoded, &connections));
        }

        let bytes = encode_connections(&canonical).unwrap();
        assert!(decode_connections(&bytes[..bytes.len() - 1], &BOUNDS).is_err());
        let narrow = ConnectionBounds { stops: 3, ..BOUNDS };
        assert!(decode_connections(&bytes, &narrow).is_err());
    }
}
