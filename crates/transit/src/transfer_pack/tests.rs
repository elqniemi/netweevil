use std::sync::Arc;

use super::*;
use crate::gtfs::{GtfsFiles, build_bundle_from_files};
use crate::model::{
    TransitImportOptions, TransitLegType, TransitModeOptions, TransitOutcome, TransitPoint,
    TransitQueryTime, TransitReturnOptions, TransitRouteRequest, TransitWalkingGeometry,
};
use crate::{PreparedTransitRouter, network_transfer};

fn path(
    edges: &[u32],
    geometry: &[[f64; 3]],
    distance_m: Option<f64>,
    components: &[(&str, f64)],
) -> TransitStreetPath {
    TransitStreetPath {
        travel_time_s: 60 + edges.len() as u32,
        distance_m,
        edge_path: edges.to_vec(),
        geometry: geometry.to_vec(),
        components: components
            .iter()
            .map(|(key, value)| (key.to_string(), *value))
            .collect(),
    }
}

fn table(transfers: Vec<TransitNetworkTransfer>) -> TransitTransferTable {
    TransitTransferTable {
        schema_version: TRANSIT_TRANSFER_TABLE_SCHEMA_VERSION,
        feed_id: "feed".to_string(),
        dataset_id: "dataset".to_string(),
        profile_id: "walk".to_string(),
        max_transfer_distance_m: 500.0,
        transfers,
    }
}

#[test]
fn packing_reproduces_every_path_bit_for_bit() {
    let p = |x: f64| [6.0 + x * 1e-4, 53.0 + x * 1e-5, x * 0.1 - 3.3];
    let original = table(vec![
        // Two trie paths sharing a two-edge prefix.
        network_transfer(
            "A",
            "B",
            10.5,
            path(
                &[5, 9, 2],
                &[p(0.0), p(1.0), p(2.0), p(3.0)],
                Some(42.0),
                &[
                    ("ascent_m", 0.262_000_001_966_953_3),
                    ("sun_time", 31.843_463_662_100_8),
                    ("uncovered_time", 31.843_463_662_100_8),
                ],
            ),
        ),
        network_transfer(
            "A",
            "C",
            11.0,
            path(
                &[5, 9, 4_000_000_000],
                &[p(0.0), p(1.0), p(2.0), p(7.5)],
                Some(0.0),
                &[("ascent_m", -0.0), ("lift_wait", 0.0)],
            ),
        ),
        // A path that starts and ends on the same point.
        network_transfer("A", "D", 12.0, path(&[], &[p(0.0)], Some(1.5), &[])),
        // Geometry that does not follow the edges falls back to raw lists.
        network_transfer(
            "B",
            "A",
            10.5,
            path(
                &[3, 1, 2],
                &[p(9.0), p(0.0)],
                None,
                &[("big", 9.1e15), ("tiny", 1e-300), ("quarter", 0.25)],
            ),
        ),
        network_transfer("B", "C", 4.0, TransitStreetPath::time_only(17)),
    ]);

    let packed = PackedTransitTransferTable::from_table(&original).unwrap();
    assert_eq!(packed.len(), 5);
    assert!(packed.to_table().unwrap() == original);
    for (index, transfer) in original.transfers.iter().enumerate() {
        let decoded = packed.path(index as u32).unwrap();
        assert_eq!(decoded, transfer.path);
        for (left, right) in decoded
            .components
            .values()
            .zip(transfer.path.components.values())
        {
            assert_eq!(left.to_bits(), right.to_bits());
        }
    }

    let file = std::env::temp_dir().join(format!(
        "netweevil-packed-transfers-{}.bin",
        std::process::id()
    ));
    write_packed_transit_transfer_table(&file, &packed).unwrap();
    assert!(read_packed_file(&file).unwrap() == packed);
    let bytes = fs::read(&file).unwrap();
    fs::write(&file, &bytes[..bytes.len() - 3]).unwrap();
    assert!(read_packed_file(&file).is_err());
    fs::remove_file(&file).unwrap();
}

fn two_platform_bundle() -> crate::model::TransitBundle {
    let mut files = GtfsFiles::default();
    files.insert(
        "agency.txt",
        "agency_id,agency_name,agency_url,agency_timezone\nA,F,https://example.test,Europe/Amsterdam\n"
            .to_string(),
    );
    files.insert(
        "stops.txt",
        "stop_id,stop_name,stop_lat,stop_lon\nA,A,53.0,6.0\nB1,B1,53.0,6.01\nB2,B2,53.0,6.0101\nC,C,53.0,6.02\n"
            .to_string(),
    );
    files.insert(
        "routes.txt",
        "route_id,route_short_name,route_long_name,route_type\nR1,1,L1,1\nR2,2,L2,1\n".to_string(),
    );
    files.insert(
        "trips.txt",
        "route_id,service_id,trip_id,trip_headsign\nR1,S,T1,B1\nR2,S,T2,C\n".to_string(),
    );
    files.insert(
        "calendar.txt",
        "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date\nS,1,1,1,1,1,1,1,20260501,20260531\n"
            .to_string(),
    );
    files.insert(
        "stop_times.txt",
        "trip_id,arrival_time,departure_time,stop_id,stop_sequence\nT1,08:05:00,08:05:00,A,1\nT1,08:10:00,08:10:00,B1,2\nT2,08:15:00,08:15:00,B2,1\nT2,08:25:00,08:25:00,C,2\n"
            .to_string(),
    );
    build_bundle_from_files(
        files,
        TransitImportOptions {
            name: "platforms".to_string(),
            source_label: "platforms".to_string(),
            service_start_date: "2026-05-11".to_string(),
            service_days: 1,
        },
    )
    .unwrap()
}

#[test]
fn routes_expose_the_exact_packed_transfer_path_in_both_directions() {
    let bundle = Arc::new(two_platform_bundle());
    let walk = path(
        &[11, 12],
        &[
            [6.01, 53.0, -12.5],
            [6.01005, 53.00001, -7.25],
            [6.0101, 53.0, -2.0],
        ],
        Some(95.0),
        &[("lift_wait", 20.0), ("stair_burden", 0.1)],
    );
    let mut transfers = table(vec![network_transfer("B1", "B2", 7.0, walk.clone())]);
    transfers.feed_id = bundle.feed_id.clone();
    let router =
        PreparedTransitRouter::new_with_transfer_tables(Arc::clone(&bundle), vec![transfers])
            .unwrap();
    for (arrive_by, datetime) in [
        (false, "2026-05-11T08:00:00+02:00"),
        (true, "2026-05-11T08:30:00+02:00"),
    ] {
        let result = router
            .execute_route(&TransitRouteRequest {
                route_id: "platform-change".to_string(),
                origin: TransitPoint {
                    id: "origin".to_string(),
                    lon: 6.0,
                    lat: 53.0,
                    z: None,
                },
                destination: TransitPoint {
                    id: "destination".to_string(),
                    lon: 6.02,
                    lat: 53.0,
                    z: None,
                },
                time: TransitQueryTime {
                    datetime: datetime.to_string(),
                    arrive_by,
                    search_window_s: 3_600,
                },
                modes: TransitModeOptions {
                    max_access_distance_m: 50.0,
                    max_egress_distance_m: 50.0,
                    transfer_profile_id: Some("walk".to_string()),
                    transfer_slack_s: 0,
                    ..TransitModeOptions::default()
                },
                returns: TransitReturnOptions {
                    include_geometry: true,
                    walking_geometry: TransitWalkingGeometry::Network,
                    ..TransitReturnOptions::default()
                },
                alternatives: Default::default(),
            })
            .unwrap();
        assert_eq!(
            result.outcome,
            TransitOutcome::Scheduled,
            "arrive_by={arrive_by}"
        );
        let leg = result
            .legs
            .iter()
            .find(|leg| leg.leg_type == TransitLegType::Transfer)
            .expect("transfer leg");
        assert_eq!(leg.network_path.as_ref(), Some(&walk));
        assert_eq!(leg.geometry, walk.geometry);
        assert_eq!(leg.arrival_s - leg.departure_s, walk.travel_time_s);
    }
}
