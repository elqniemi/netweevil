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
