//! Street-network access, egress, and precomputed transfer tables supplied by
//! a host [`StreetTimeEstimator`].

use std::collections::BTreeMap;
use std::sync::Arc;

use super::fixtures::*;
use crate::*;

fn bind_stops_to_nodes(bundle: &mut TransitBundle, first_node: u32) {
    for (index, stop) in bundle.stops.iter_mut().enumerate() {
        stop.binding = Some(TransitStopBindingTarget::Node {
            node_id: index as u32 + first_node,
        });
    }
}

#[test]
fn network_street_access_prices_legs_with_the_estimator() {
    let unbound = router(import(line_feed(), 7));
    let request = |street_access| {
        let mut request = depart_at(hm(8, 0));
        request.modes.street_access = street_access;
        request
    };
    let network = request(TransitStreetAccessModel::Network);
    let fixed = FixedStreetTime::both(300);

    // Straight-line pricing is unchanged even when an estimator is present.
    let straight = unbound
        .execute_route_with_street_estimator(
            &request(TransitStreetAccessModel::StraightLine),
            Some(&fixed),
        )
        .unwrap();
    assert_eq!(straight.summary.total_travel_time_s, Some(1200));

    // Network pricing uses the estimator's times for access and egress.
    let priced = unbound
        .execute_route_with_street_estimator(&network, Some(&fixed))
        .unwrap();
    assert_eq!(priced.outcome, TransitOutcome::Scheduled);
    for leg_type in [TransitLegType::Access, TransitLegType::Egress] {
        let leg = first_leg(&priced, leg_type);
        assert_eq!(leg.arrival_s - leg.departure_s, 300);
    }

    // Network mode never teleports when the host has no estimator.
    let missing_estimator = unbound.execute_route(&network).unwrap();
    assert_eq!(missing_estimator.outcome, TransitOutcome::Unreachable);
    assert_eq!(missing_estimator.summary.total_travel_time_s, None);

    // A missing path in either direction omits the stop candidate even when
    // the other direction is routable. The fixture stops are intentionally
    // unbound, exercising the coordinate-only estimator path.
    for one_way in [
        FixedStreetTime {
            access_s: None,
            egress_s: Some(300),
        },
        FixedStreetTime {
            access_s: Some(300),
            egress_s: None,
        },
    ] {
        let result = unbound
            .execute_route_with_street_estimator(&network, Some(&one_way))
            .unwrap();
        assert_eq!(result.outcome, TransitOutcome::Unreachable);
    }

    // A coordinate-only estimator cannot bypass an explicit platform/node
    // binding. Rich binding-aware path resolution is required for bound stops.
    let mut bound = import(line_feed(), 7);
    bind_stops_to_nodes(&mut bound, 0);
    let bound_fallback = router(bound)
        .execute_route_with_street_estimator(&network, Some(&fixed))
        .unwrap();
    assert_eq!(bound_fallback.outcome, TransitOutcome::Unreachable);
}

#[test]
fn network_endpoints_preserve_z_and_cannot_fall_back_to_xy_estimators() {
    struct ThreeD;
    impl StreetTimeEstimator for ThreeD {
        fn street_time_s(
            &self,
            _: AccessMode,
            _: bool,
            _: f64,
            _: f64,
            _: f64,
            _: f64,
        ) -> Option<u32> {
            panic!("3D endpoint must not call XY fallback")
        }
        fn point_to_stop_path_3d(
            &self,
            _: AccessMode,
            from: &TransitPoint,
            _: &TransitStop,
        ) -> Option<TransitStreetPath> {
            assert_eq!(from.z, Some(-12.0));
            Some(TransitStreetPath::time_only(1))
        }
        fn stop_to_point_path_3d(
            &self,
            _: AccessMode,
            _: &TransitStop,
            to: &TransitPoint,
        ) -> Option<TransitStreetPath> {
            assert_eq!(to.z, Some(7.0));
            Some(TransitStreetPath::time_only(1))
        }
    }
    let router = router(import(line_feed(), 7));
    for arrive_by in [false, true] {
        let mut request = timed_request(arrive_by, hm(7, 55), hm(8, 30));
        request.modes.street_access = TransitStreetAccessModel::Network;
        request.origin.z = Some(-12.0);
        request.destination.z = Some(7.0);
        // Elevation has to survive the JSON API boundary.
        let decoded: TransitRouteRequest =
            serde_json::from_str(&serde_json::to_string(&request).unwrap()).unwrap();
        assert_eq!(decoded.origin.z, request.origin.z);
        assert_eq!(
            router
                .execute_route_with_street_estimator(&decoded, Some(&FixedStreetTime::both(1)))
                .unwrap()
                .outcome,
            TransitOutcome::Unreachable
        );
        assert_eq!(
            router
                .execute_route_with_street_estimator(&decoded, Some(&ThreeD))
                .unwrap()
                .outcome,
            TransitOutcome::Scheduled
        );
    }
}

#[test]
fn network_endpoint_snap_limit_rejects_remote_attachments_and_accounts_for_small_gaps() {
    /// Attaches the endpoint `offset_lon` degrees east of where it was requested.
    struct AttachedPath {
        offset_lon: f64,
    }
    impl AttachedPath {
        fn path(
            &self,
            point: &TransitPoint,
            stop: &TransitStop,
            egress: bool,
        ) -> TransitStreetPath {
            let attachment = [
                point.lon + self.offset_lon,
                point.lat,
                point.z.unwrap_or(0.0),
            ];
            let mut geometry = vec![attachment, [stop.lon, stop.lat, 0.0]];
            if egress {
                geometry.reverse();
            }
            TransitStreetPath {
                travel_time_s: 1,
                distance_m: Some(1.0),
                geometry,
                edge_path: Vec::new(),
                components: BTreeMap::new(),
            }
        }
    }
    impl StreetTimeEstimator for AttachedPath {
        fn requires_network_path(&self) -> bool {
            true
        }
        fn street_time_s(
            &self,
            _: AccessMode,
            _: bool,
            _: f64,
            _: f64,
            _: f64,
            _: f64,
        ) -> Option<u32> {
            panic!("failed endpoint attachment must not retry unconstrained coordinate times")
        }
        fn point_to_stop_path_with_snap_limit(
            &self,
            _: AccessMode,
            point: &TransitPoint,
            stop: &TransitStop,
            _: f64,
        ) -> Option<TransitStreetPath> {
            Some(self.path(point, stop, false))
        }
        fn stop_to_point_path_with_snap_limit(
            &self,
            _: AccessMode,
            stop: &TransitStop,
            point: &TransitPoint,
            _: f64,
        ) -> Option<TransitStreetPath> {
            Some(self.path(point, stop, true))
        }
    }
    let router = router(import(line_feed(), 7));
    let remote = AttachedPath { offset_lon: 0.01 };
    let nearby = AttachedPath { offset_lon: 0.0001 };
    for arrive_by in [false, true] {
        let mut request = timed_request(arrive_by, hm(7, 55), hm(8, 30));
        request.modes.street_access = TransitStreetAccessModel::Network;
        assert_eq!(request.modes.max_endpoint_snap_distance_m, 50.0);
        assert_eq!(
            router
                .execute_route_with_street_estimator(&request, Some(&remote))
                .unwrap()
                .outcome,
            TransitOutcome::Unreachable
        );
        let result = router
            .execute_route_with_street_estimator(&request, Some(&nearby))
            .unwrap();
        assert_eq!(result.outcome, TransitOutcome::Scheduled);
        for leg in access_and_egress_legs(&result) {
            let path = leg.network_path.as_ref().unwrap();
            let gap_m = path.components["off_network_connection_distance_m"];
            let gap_s = path.components["off_network_connection_time_s"];
            assert!(gap_m > 1.0 && gap_m < 50.0);
            assert!(gap_s > 0.0);
            assert_eq!(path.travel_time_s, 1 + gap_s as u32);
            assert_eq!(path.distance_m, Some(1.0 + gap_m));
            assert_eq!(leg.arrival_s - leg.departure_s, path.travel_time_s);
        }
        request.modes.max_endpoint_snap_distance_m = 0.0;
        assert_eq!(
            router
                .execute_route_with_street_estimator(&request, Some(&nearby))
                .unwrap()
                .outcome,
            TransitOutcome::Unreachable
        );
        for invalid in [-1.0, f64::INFINITY, f64::NAN] {
            request.modes.max_endpoint_snap_distance_m = invalid;
            assert!(
                router
                    .execute_route_with_street_estimator(&request, Some(&nearby))
                    .is_err()
            );
        }
    }
}

#[test]
fn transfer_builder_routes_bound_stops_directionally() {
    /// Routes only between node-bound stops; the cost grows with node distance.
    struct BindingAwareEstimator;
    impl StreetTimeEstimator for BindingAwareEstimator {
        fn street_time_s(
            &self,
            _: AccessMode,
            _: bool,
            _: f64,
            _: f64,
            _: f64,
            _: f64,
        ) -> Option<u32> {
            None
        }
        fn stop_to_stop_path(
            &self,
            _: AccessMode,
            from: &TransitStop,
            to: &TransitStop,
        ) -> Option<TransitStreetPath> {
            let TransitStopBindingTarget::Node { node_id: from_node } = from.binding.as_ref()?
            else {
                return None;
            };
            let TransitStopBindingTarget::Node { node_id: to_node } = to.binding.as_ref()? else {
                return None;
            };
            Some(TransitStreetPath {
                travel_time_s: from_node.abs_diff(*to_node) + 10,
                distance_m: Some(from_node.abs_diff(*to_node) as f64),
                edge_path: vec![*from_node, *to_node],
                geometry: Vec::new(),
                components: BTreeMap::new(),
            })
        }
    }
    let mut bundle = import(line_feed(), 1);
    bind_stops_to_nodes(&mut bundle, 100);
    let options = TransitTransferBuildOptions {
        dataset_id: "station-network".to_string(),
        profile_id: "step_free".to_string(),
        profile_hash: "profile-hash".to_string(),
        max_transfer_distance_m: 2_000.0,
        max_candidates_per_stop: 1,
    };
    let table = build_transit_transfer_table(&bundle, &options, &BindingAwareEstimator)
        .expect("transfer table builds");

    assert_eq!(table.profile_id, "step_free");
    assert_eq!(table.transfers.len(), 3);
    assert!(
        table
            .transfers
            .iter()
            .all(|transfer| transfer.path.edge_path.len() == 2)
    );
    assert!(
        table
            .transfers
            .iter()
            .any(|transfer| transfer.from_stop_id == "B" && transfer.to_stop_id == "A")
    );

    let path = temp_path("transfers");
    write_transit_transfer_table(&path, &table).unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bincode::serialize(&table).unwrap()
    );
    assert_eq!(
        read_transit_transfer_table(&path).unwrap().transfers.len(),
        table.transfers.len()
    );
    std::fs::remove_file(path).unwrap();

    let coordinate_only =
        build_transit_transfer_table(&bundle, &options, &FixedStreetTime::both(1))
            .expect("coordinate-only transfer build completes");
    assert!(
        coordinate_only.transfers.is_empty(),
        "coordinate-only fallback must not bypass explicit stop bindings"
    );
}

#[test]
fn csa_consumes_network_transfer_table_and_exposes_station_path() {
    let bundle = Arc::new(import(two_line_feed(), 1));
    let components = BTreeMap::from([
        ("ascent_m".to_string(), 12.0),
        ("lift_wait_s".to_string(), 35.0),
    ]);
    let table = TransitTransferTable {
        schema_version: TRANSIT_TRANSFER_TABLE_SCHEMA_VERSION,
        feed_id: bundle.feed_id.clone(),
        source_sha256: bundle.source_sha256.clone(),
        stop_binding_sha256: bundle.stop_binding_sha256.clone(),
        dataset_id: "station-network".to_string(),
        profile_id: "step_free".to_string(),
        profile_hash: "profile-hash".to_string(),
        max_transfer_distance_m: 100.0,
        transfers: vec![network_transfer(
            "B1",
            "B2",
            7.0,
            TransitStreetPath {
                travel_time_s: 180,
                distance_m: Some(95.0),
                edge_path: vec![41, 42, 43],
                geometry: vec![[6.01, 53.0, -12.0], [6.0101, 53.0, -2.0]],
                components: components.clone(),
            },
        )],
    };
    let router = PreparedTransitRouter::new_with_transfer_tables(bundle, vec![table])
        .expect("prepared router loads table");
    let mut request = depart_at(hm(8, 0));
    request.modes.max_access_distance_m = 50.0;
    request.modes.max_egress_distance_m = 50.0;
    request.modes.max_transfer_distance_m = 100.0;
    request.modes.transfer_profile_id = Some("step_free".to_string());
    request.modes.transfer_slack_s = 0;
    request.returns.include_geometry = true;
    request.returns.walking_geometry = TransitWalkingGeometry::Network;

    let result = router
        .execute_route(&request)
        .expect("network-transfer route executes");

    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    assert_eq!(result.summary.boarding_count, 2);
    let transfer = first_leg(&result, TransitLegType::Transfer);
    assert_eq!(transfer.arrival_s - transfer.departure_s, 180);
    let path = transfer
        .network_path
        .as_ref()
        .expect("network path exposed");
    assert_eq!(path.edge_path, vec![41, 42, 43]);
    assert_eq!(path.components, components);
    assert_eq!(path.geometry[0][2], -12.0);
    assert_eq!(transfer.geometry, path.geometry);
}
