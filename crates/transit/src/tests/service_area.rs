use std::sync::Mutex;

use super::fixtures::*;
use crate::*;

/// Depart-after stop catchment from the origin at 08:00 with 100 m access and
/// transfer radii and a 30 minute budget.
fn morning_area() -> TransitServiceAreaRequest {
    let modes = TransitModeOptions {
        max_access_distance_m: 100.0,
        max_transfer_distance_m: 100.0,
        ..TransitModeOptions::default()
    };
    let mut request = stops_area(point("origin", 6.0), depart_at(hm(8, 0)).time, modes);
    request.max_travel_time_s = 1800;
    request
}

#[test]
fn executes_transit_service_area_to_reachable_stops_and_segments() {
    let bundle = import(line_feed(), 7);
    let request = morning_area();

    let result = execute_transit_service_area(&bundle, &request).expect("service area");

    assert_eq!(result.outcome, TransitOutcome::Scheduled);
    assert_eq!(result.processed_origin_count, 1);
    assert!(area_stop(&result, "C").is_some());
    assert!(
        result
            .stop_segments
            .iter()
            .any(|segment| segment.from_stop_id == "B" && segment.to_stop_id == "C")
    );
    assert!(
        result
            .stops
            .iter()
            .all(|stop| stop.travel_time_s <= request.max_travel_time_s)
    );
}

#[test]
fn rejects_transit_service_area_above_stop_segment_limit() {
    let bundle = import(line_feed(), 7);
    let mut request = morning_area();
    request.returns.max_stop_segments = 0;

    let error = execute_transit_service_area(&bundle, &request)
        .expect_err("stop segment cap should reject large output");

    assert!(error.to_string().contains("max_stop_segments"));
}

#[test]
fn car_access_extends_transit_service_area_reach() {
    let bundle = import(line_feed(), 1);
    // Origin is ~10 km from stop A: only car access can bridge the first mile.
    let walk_request = stops_area(
        point("park_and_ride", 5.85),
        depart_at(hm(8, 0)).time,
        TransitModeOptions::default(),
    );
    let car_request = TransitServiceAreaRequest {
        modes: TransitModeOptions {
            access: vec![AccessMode::Car],
            egress: vec![AccessMode::Car],
            ..TransitModeOptions::default()
        },
        ..walk_request.clone()
    };

    let walk_result = execute_transit_service_area(&bundle, &walk_request).expect("walk run");
    assert_eq!(walk_result.outcome, TransitOutcome::Unreachable);
    assert_eq!(walk_result.skipped_origin_count, 1);

    let car_result = execute_transit_service_area(&bundle, &car_request).expect("car run");
    assert_eq!(car_result.outcome, TransitOutcome::Scheduled);
    assert_eq!(car_result.processed_origin_count, 1);
    let stop_a = area_stop(&car_result, "A").expect("stop A reachable by car access");
    assert_eq!(stop_a.access_mode, Some(AccessMode::Car));
}

#[test]
fn street_isochrones_seed_only_legal_stops_even_without_stop_returns() {
    /// Records the stops each origin's isochrone was seeded from.
    #[derive(Default)]
    struct IsochroneEstimator(Mutex<Vec<(String, Vec<(String, u32)>)>>);
    impl StreetTimeEstimator for IsochroneEstimator {
        fn street_time_s(
            &self,
            _: AccessMode,
            _: bool,
            from_lon: f64,
            _: f64,
            to_lon: f64,
            _: f64,
        ) -> Option<u32> {
            (from_lon == to_lon).then_some(0)
        }
        fn street_isochrone(
            &self,
            origin: &TransitPoint,
            stops: &[(&TransitStop, u32)],
            request: &TransitServiceAreaRequest,
        ) -> anyhow::Result<Vec<TransitIsochroneFeature>> {
            assert_eq!(
                request.modes.street_access,
                TransitStreetAccessModel::Network
            );
            self.0.lock().unwrap().push((
                origin.id.clone(),
                stops
                    .iter()
                    .map(|(stop, elapsed)| (stop.stop_id.clone(), *elapsed))
                    .collect(),
            ));
            Ok(vec![TransitIsochroneFeature {
                origin_id: origin.id.clone(),
                mode: AccessMode::Walk,
                geometry_type: "network".to_string(),
                threshold_limit: f64::from(request.max_travel_time_s),
                threshold_metric: "travel_time_s".to_string(),
                reachable_network_length_m: 100.0,
                reachable_edge_count: 1,
                geometry: None,
            }])
        }
    }
    let mut bundle = import(line_feed(), 1);
    // B is a through stop: passengers may stay aboard, but cannot get on/off.
    bundle.connections[0].drop_off_allowed = false;
    bundle.connections[1].pickup_allowed = false;
    let router = router(bundle);
    for (arrive_by, lon, datetime, expected_stop) in [
        (false, 6.0, "2026-05-11T08:00:00+02:00", "C"),
        (true, 6.02, "2026-05-11T08:30:00+02:00", "A"),
    ] {
        let request: TransitServiceAreaRequest = serde_json::from_value(serde_json::json!({
            "analysis_id": "isochrone", "catchment_mode": "street_isochrone",
            "origins": [{"id": "near", "lon": lon, "lat": 53.0}, {"id": "no_stops", "lon": 10.0, "lat": 53.0}],
            "time": {"datetime": datetime, "arrive_by": arrive_by, "search_window_s": 3600},
            "modes": {"max_access_distance_m": 100, "max_egress_distance_m": 100, "max_transfer_distance_m": 0},
            "max_travel_time_s": 1800,
            "returns": {"include_stops": false, "include_stop_segments": false, "max_stops": 0}
        })).unwrap();
        assert!(
            router
                .execute_service_area(&request)
                .unwrap_err()
                .to_string()
                .contains("street-network")
        );
        let estimator = IsochroneEstimator::default();
        let result = router
            .execute_service_area_with_street_estimator(&request, Some(&estimator))
            .unwrap();
        assert!(result.stops.is_empty());
        assert!(result.stop_segments.is_empty());
        assert_eq!(result.features.len(), 2);
        assert_eq!(result.processed_origin_count, 2);
        assert_eq!(result.skipped_origin_count, 0);
        let calls = estimator.0.lock().unwrap();
        let seeds = |id: &str| &calls.iter().find(|(origin, _)| origin == id).unwrap().1;
        assert_eq!(seeds("near"), &vec![(expected_stop.to_string(), 1200)]);
        assert!(seeds("no_stops").is_empty());
    }
}
