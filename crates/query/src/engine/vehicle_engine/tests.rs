use std::sync::Arc;

use netweevil_core::VehicleParams;

use crate::tests::fixtures::*;
use crate::*;

fn height(height_m: f64) -> VehicleParams {
    VehicleParams {
        height_m: Some(height_m),
        ..Default::default()
    }
}

/// Request vehicles of every kind relative to a 4 m truck profile: one
/// with the same exclusions, two that only exclude more, and two that
/// re-enable edges the profile excludes.
fn request_vehicles() -> Vec<VehicleParams> {
    vec![
        height(3.9),
        height(4.4),
        VehicleParams {
            weight_t: Some(12.0),
            ..Default::default()
        },
        height(3.5),
        VehicleParams {
            height_m: Some(2.5),
            hazmat: Some(true),
            ..Default::default()
        },
    ]
}

/// Every grid node paired with a node a few columns and rows away.
fn grid_pairs() -> Vec<(LabeledPoint, LabeledPoint)> {
    let span = f64::from(TRUCK_GRID);
    (0..TRUCK_GRID * TRUCK_GRID)
        .map(|id| {
            let (x, y) = (f64::from(id % TRUCK_GRID), f64::from(id / TRUCK_GRID));
            (
                truck_grid_point("o", x, y),
                truck_grid_point("d", (x + 3.0) % span, (y + 2.0) % span),
            )
        })
        .collect()
}

fn route(
    origin: LabeledPoint,
    destination: LabeledPoint,
    vehicle: Option<VehicleParams>,
) -> RouteRequest {
    let mut request = route_request("truck", origin, destination);
    request.snap = snap_within(150.0);
    request.returns = full_geometry();
    request.temporal.vehicle = vehicle;
    request
}

/// Edge path and generalized cost bits, or the error text.
fn outcome(result: anyhow::Result<RouteResult>) -> Result<(Vec<u32>, u64), String> {
    result
        .map(|route| {
            (
                route.edge_path,
                route.summary.total_generalized_cost.to_bits(),
            )
        })
        .map_err(|error| error.to_string())
}

/// A request vehicle on the 4 m truck's CCH engine routes exactly like an
/// engine compiled for the merged vehicle, exact or accelerated, whichever
/// path serves it (the profile's engine, the avoid engines, or a derived
/// vehicle engine).
#[test]
fn request_vehicles_route_like_profiles_compiled_with_them() {
    let topology = truck_restricted_grid();
    let profile_vehicle = height(4.0);
    let truck = compile_truck_profile(&topology, Some(profile_vehicle));
    let engine = cch_engine(topology.clone(), &truck);
    let mut changed_routes = 0;
    for request_vehicle in request_vehicles() {
        let merged = profile_vehicle.merged(&request_vehicle);
        let fresh = compile_truck_profile(&topology, Some(merged));
        let fresh_exact = exact_engine(topology.clone(), fresh.clone());
        let fresh_cch = cch_engine(topology.clone(), &fresh);
        for (origin, destination) in grid_pairs() {
            let reference = route(origin.clone(), destination.clone(), None);
            let actual =
                outcome(engine.execute_route(&route(origin, destination, Some(request_vehicle))));
            let exact = outcome(fresh_exact.execute_route(&reference));
            let accelerated = outcome(fresh_cch.execute_route(&reference));
            assert_eq!(
                actual, exact,
                "{request_vehicle:?} against the exact engine"
            );
            assert_eq!(actual, accelerated, "{request_vehicle:?} against the CCH");
            changed_routes += usize::from(actual != outcome(engine.execute_route(&reference)));
        }
    }
    assert!(changed_routes > 10, "only {changed_routes} routes changed");
}

/// OD documents, matrices, service areas and trips with a request vehicle
/// equal the same analyses on a CCH engine compiled for that vehicle.
#[test]
fn batch_analyses_honour_request_vehicles() {
    let topology = truck_restricted_grid();
    let profile_vehicle = height(4.0);
    let truck = compile_truck_profile(&topology, Some(profile_vehicle));
    let engine = cch_engine(topology.clone(), &truck);
    let points = (0..TRUCK_GRID * TRUCK_GRID)
        .step_by(3)
        .map(|id| {
            truck_grid_point(
                &format!("p{id}"),
                f64::from(id % TRUCK_GRID),
                f64::from(id / TRUCK_GRID),
            )
        })
        .collect::<Vec<_>>();
    for request_vehicle in request_vehicles() {
        let fresh = cch_engine(
            topology.clone(),
            &compile_truck_profile(&topology, Some(profile_vehicle.merged(&request_vehicle))),
        );
        let label = format!("{request_vehicle:?}");

        let mut origins = point_set(points.clone());
        origins.snap = snap_within(150.0);
        let destinations = origins.clone();
        let mut with_vehicle = origins.clone();
        with_vehicle.temporal.vehicle = Some(request_vehicle);
        let cells = |result: MatrixResult| {
            result
                .cells
                .into_iter()
                .map(|cell| (cell.status, cell.total_generalized_cost.map(f64::to_bits)))
                .collect::<Vec<_>>()
        };
        let expected = cells(fresh.execute_matrix(&origins, &destinations).unwrap());
        assert!(expected.iter().filter(|(_, cost)| cost.is_some()).count() > 20);
        assert_eq!(
            cells(engine.execute_matrix(&with_vehicle, &destinations).unwrap()),
            expected,
            "{label} matrix"
        );

        let mut od = od_pairs(
            grid_pairs()
                .into_iter()
                .enumerate()
                .map(|(index, (origin, destination))| {
                    od_pair(&format!("pair{index}"), origin, destination)
                })
                .collect(),
        );
        od.snap = snap_within(150.0);
        let pairs = |result: OdResult| {
            result
                .pairs
                .into_iter()
                .map(|pair| (pair.status, pair.total_generalized_cost.map(f64::to_bits)))
                .collect::<Vec<_>>()
        };
        let expected = pairs(fresh.execute_od(&od).unwrap());
        od.temporal.vehicle = Some(request_vehicle);
        assert_eq!(
            pairs(engine.execute_od(&od).unwrap()),
            expected,
            "{label} OD"
        );

        let mut area: ServiceAreaRequest = serde_json::from_value(serde_json::json!({
            "analysis_id": "truck-area",
            "origins": [truck_grid_point("o", 2.0, 2.0)],
            "thresholds": [{"limit": 60.0, "metric": "travel_time_s"}],
            "snap": {"max_distance_m": 150.0},
            "output_mode": "network",
            "returns": {"segments": true, "geometry": false}
        }))
        .unwrap();
        let segments = |result: ServiceAreaResult| {
            result
                .segments
                .into_iter()
                .map(|segment| (segment.edge_index, segment.end_cost.to_bits()))
                .collect::<Vec<_>>()
        };
        let expected = segments(fresh.execute_service_area(&area).unwrap());
        assert!(expected.len() > 4, "{label} service area reaches little");
        area.temporal.vehicle = Some(request_vehicle);
        assert_eq!(
            segments(engine.execute_service_area(&area).unwrap()),
            expected,
            "{label} service area"
        );

        let mut trip: TripRequest = serde_json::from_value(serde_json::json!({
            "trip_id": "truck-trip",
            "stops": points.iter().take(5).map(|point| serde_json::json!({"point": point})).collect::<Vec<_>>(),
            "snap": {"max_distance_m": 150.0}
        }))
        .unwrap();
        let tour = |result: anyhow::Result<TripResult>| {
            result
                .map(|trip| (trip.stop_order, trip.total_generalized_cost.to_bits()))
                .map_err(|error| error.to_string())
        };
        let expected = tour(fresh.execute_trip(&trip));
        trip.vehicle = Some(request_vehicle);
        assert_eq!(tour(engine.execute_trip(&trip)), expected, "{label} trip");
    }
}

/// Vehicles with the profile's exclusions run on the profile's engine;
/// vehicles that re-enable edges share one cached, accelerated engine per
/// exclusion set.
#[test]
fn vehicle_engines_are_cached_per_exclusion_set() {
    let topology = truck_restricted_grid();
    let truck = compile_truck_profile(&topology, Some(height(4.0)));
    let engine = cch_engine(topology, &truck);
    let target = |vehicle: VehicleParams| {
        engine
            .request_target(&AvoidOptions::default(), Some(&vehicle), EngineMode::Auto)
            .expect("vehicle resolves")
    };
    assert!(target(height(3.9)).is_none());
    let lower = target(height(3.5)).expect("a lower truck gets its own engine");
    // 3.1 m excludes the same 3.0 m blocks as 3.5 m.
    let same_set = target(height(3.1)).expect("same exclusions");
    assert!(Arc::ptr_eq(&lower, &same_set));
    assert_eq!(
        lower
            .effective_engine_description(EngineMode::Auto)
            .acceleration,
        "spatial_index+edge_phantoms+cch"
    );
    // A request with the derived engine's vehicle needs nothing further.
    assert!(
        lower
            .request_target(
                &AvoidOptions::default(),
                Some(&height(3.1)),
                EngineMode::Auto
            )
            .unwrap()
            .is_none()
    );
}
