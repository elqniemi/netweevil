use super::*;

fn restriction(modes: u16, kind: VehicleRestrictionKind) -> VehicleRestriction {
    VehicleRestriction {
        modes: AccessMask::new(modes),
        kind,
    }
}

#[test]
fn restrictions_bind_only_set_parameters_of_scoped_modes() {
    use VehicleRestrictionKind as K;
    let truck = VehicleParams {
        height_m: Some(4.0),
        weight_t: Some(18.0),
        axle_load_t: Some(10.0),
        adr_tunnel_code: Some(AdrTunnelCategory::C),
        trailers: Some(1),
        ..VehicleParams::default()
    };
    let all = AccessMask::VEHICLES;
    let hgv = AccessMask::HGV;
    // (restriction, mode bit, excluded)
    for (restriction, mode, expected) in [
        (restriction(all, K::MaxHeightCm(400)), hgv, false),
        (restriction(all, K::MaxHeightCm(399)), hgv, true),
        (restriction(all, K::MaxWidthCm(200)), hgv, false),
        (restriction(all, K::MaxWeightKg(7_500)), hgv, true),
        (
            restriction(AccessMask::BUS, K::MaxWeightKg(7_500)),
            hgv,
            false,
        ),
        (restriction(all, K::MaxAxleLoadKg(10_000)), hgv, false),
        (restriction(all, K::NoHazmat), hgv, true),
        (restriction(all, K::NoHazmatWater), hgv, false),
        (
            restriction(all, K::AdrTunnel(AdrTunnelCategory::B)),
            hgv,
            false,
        ),
        (
            restriction(all, K::AdrTunnel(AdrTunnelCategory::C)),
            hgv,
            true,
        ),
        (
            restriction(all, K::AdrTunnel(AdrTunnelCategory::E)),
            hgv,
            true,
        ),
        (restriction(all, K::NoTrailer), hgv, true),
        (restriction(hgv, K::NoArticulated), AccessMask::CAR, false),
    ] {
        assert_eq!(
            restriction.excludes(&truck, mode),
            expected,
            "{restriction:?} mode {mode}"
        );
    }
    assert!(!restriction(all, K::NoHazmat).excludes(&VehicleParams::default(), hgv));
}

#[test]
fn request_fields_override_profile_defaults() {
    let profile = VehicleParams {
        height_m: Some(4.0),
        weight_t: Some(40.0),
        ..VehicleParams::default()
    };
    let request = VehicleParams {
        weight_t: Some(12.0),
        hazmat: Some(true),
        ..VehicleParams::default()
    };
    let merged = profile.merged(&request);
    assert_eq!(merged.height_m, Some(4.0));
    assert_eq!(merged.weight_t, Some(12.0));
    assert_eq!(merged.hazmat, Some(true));
    assert!(
        VehicleParams {
            height_m: Some(-1.0),
            ..VehicleParams::default()
        }
        .validate()
        .is_err()
    );
}

#[test]
fn table_resolves_edge_rows_and_dimension_classes() {
    use VehicleRestrictionKind as K;
    let table = VehicleRestrictionTable {
        restrictions: vec![
            restriction(AccessMask::VEHICLES, K::MaxHeightCm(300)),
            restriction(AccessMask::VEHICLES, K::MaxWeightKg(7_500)),
        ],
        edges: vec![1, 1, 4],
        edge_restrictions: vec![0, 1, 1],
    };
    table.validate(5).expect("valid table");
    assert!(table.validate(4).is_err());
    assert_eq!(table.for_edge(1).count(), 2);
    assert_eq!(table.for_edge(2).count(), 0);
    let heavy_low = VehicleParams {
        height_m: Some(2.5),
        weight_t: Some(12.0),
        ..VehicleParams::default()
    };
    let excluded = table.excluded_restrictions(&heavy_low, AccessMask::HGV);
    assert_eq!(excluded, vec![false, true]);
    assert_eq!(table.excluded_edges(&excluded), vec![1, 4]);
}
