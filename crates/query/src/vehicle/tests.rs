use netweevil_core::{CompiledProfileBundle, VehicleParams};

use super::*;
use crate::tests::fixtures::*;

fn height(height_m: f64) -> VehicleParams {
    VehicleParams {
        height_m: Some(height_m),
        ..Default::default()
    }
}

/// Edges whose (time, cost) bits differ between two bundles.
fn differing_edges(left: &CompiledProfileBundle, right: &CompiledProfileBundle) -> Vec<usize> {
    let bits = |metric: &netweevil_core::CompiledEdgeMetric| {
        (
            metric.travel_time_s.map(f64::to_bits),
            metric.generalized_cost.map(f64::to_bits),
        )
    };
    left.edge_metrics
        .iter()
        .zip(&right.edge_metrics)
        .enumerate()
        .filter(|(_, (left, right))| bits(left) != bits(right))
        .map(|(edge, _)| edge)
        .collect()
}

#[test]
fn profile_compile_keeps_the_metrics_of_vehicle_excluded_edges() {
    let topology = truck_restricted_grid();
    let open = compile_truck_profile(&topology, None);
    let truck = compile_truck_profile(&topology, Some(height(4.0)));
    assert!(open.vehicle.is_empty());
    assert_eq!(truck.vehicle.vehicle, Some(height(4.0)));
    // The 3.0 m and 3.8 m blocks (two of each in each direction).
    assert_eq!(truck.vehicle.edges.len(), 20);
    assert_eq!(
        differing_edges(&open, &truck),
        truck
            .vehicle
            .edges
            .iter()
            .map(|&edge| edge as usize)
            .collect::<Vec<_>>()
    );
    for (position, &edge) in truck.vehicle.edges.iter().enumerate() {
        let metric = &open.edge_metrics[edge as usize];
        assert_eq!(
            metric.travel_time_s,
            Some(truck.vehicle.travel_time_s[position])
        );
        assert_eq!(
            metric.generalized_cost,
            Some(truck.vehicle.generalized_cost[position])
        );
        assert!(truck.edge_metrics[edge as usize].travel_time_s.is_none());
    }
}

#[test]
fn request_vehicles_resolve_by_the_edges_they_exclude() {
    let topology = truck_restricted_grid();
    let truck = compile_truck_profile(&topology, Some(height(4.0)));
    let resolve = |vehicle: Option<VehicleParams>| {
        resolve_request_vehicle(&topology, &truck, vehicle.as_ref()).expect("vehicle resolves")
    };
    assert_eq!(resolve(None), VehicleChange::Unchanged);
    // 3.9 m and 4.2 m trucks pass exactly the limits a 4 m truck passes.
    assert_eq!(resolve(Some(height(3.9))), VehicleChange::Unchanged);
    assert_eq!(resolve(Some(height(4.2))), VehicleChange::Unchanged);
    let VehicleChange::Excludes(taller) = resolve(Some(height(4.3))) else {
        panic!("a taller truck only loses edges");
    };
    assert_eq!(taller.len(), 10);
    // Weight adds to the profile's height.
    let heavy = VehicleParams {
        weight_t: Some(12.0),
        ..Default::default()
    };
    assert!(matches!(resolve(Some(heavy)), VehicleChange::Excludes(_)));
    let VehicleChange::Replaces { vehicle, edges } = resolve(Some(height(3.5))) else {
        panic!("a lower truck passes the 3.8 m limits");
    };
    assert_eq!(vehicle, height(3.5));
    assert_eq!(edges.len(), 10);
    assert!(
        resolve_request_vehicle(&topology, &truck, Some(&height(-1.0))).is_err(),
        "invalid parameters are rejected"
    );
}

#[test]
fn replacing_the_vehicle_reproduces_a_profile_compiled_with_it() {
    let topology = truck_restricted_grid();
    let truck = compile_truck_profile(&topology, Some(height(4.0)));
    for request in [
        height(3.5),
        VehicleParams {
            height_m: Some(2.5),
            hazmat: Some(true),
            weight_t: Some(18.0),
            ..Default::default()
        },
    ] {
        let VehicleChange::Replaces { vehicle, edges } =
            resolve_request_vehicle(&topology, &truck, Some(&request)).unwrap()
        else {
            panic!("{request:?} re-enables edges");
        };
        let derived = apply_vehicle(&truck, vehicle, &edges);
        let fresh = compile_truck_profile(&topology, Some(vehicle));
        assert_eq!(differing_edges(&derived, &fresh), Vec::<usize>::new());
        assert_eq!(derived.vehicle, fresh.vehicle);
        assert_eq!(derived.components.len(), fresh.components.len());
        // The derived engine resolves its own vehicle as unchanged.
        assert_eq!(
            resolve_request_vehicle(&topology, &derived, Some(&request)).unwrap(),
            VehicleChange::Unchanged
        );
    }
}

#[test]
fn request_parts_must_agree_on_the_vehicle() {
    let low = height(3.0);
    let high = height(4.0);
    assert_eq!(
        single_request_vehicle([None, Some(&low), Some(&low)]).unwrap(),
        Some(&low)
    );
    assert_eq!(single_request_vehicle([None, None]).unwrap(), None);
    assert!(single_request_vehicle([Some(&low), Some(&high)]).is_err());
}
