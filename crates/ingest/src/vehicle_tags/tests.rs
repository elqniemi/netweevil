use super::{conditional_limit, parse_length_m, parse_weight_t, way_vehicle_restrictions};
use crate::test_util::tags;
use netweevil_core::{
    AccessMask, AdrTunnelCategory, VehicleRestriction, VehicleRestrictionKind as Kind,
};

fn assert_close(actual: Option<f64>, expected: Option<f64>, input: &str) {
    match (actual, expected) {
        (Some(actual), Some(expected)) => {
            assert!(
                (actual - expected).abs() < 1e-9,
                "{input}: {actual} != {expected}"
            )
        }
        _ => assert_eq!(actual, expected, "{input}"),
    }
}

#[test]
fn parses_length_variants_seen_in_osm() {
    for (input, expected) in [
        ("3.5", Some(3.5)),
        ("3,5", Some(3.5)),
        ("2.3 m", Some(2.3)),
        ("2.3m", Some(2.3)),
        ("3.8 metres", Some(3.8)),
        ("350 cm", Some(3.5)),
        ("12'6\"", Some(12.0 * 0.3048 + 6.0 * 0.0254)),
        ("12' 6\"", Some(12.0 * 0.3048 + 6.0 * 0.0254)),
        ("12'6''", Some(12.0 * 0.3048 + 6.0 * 0.0254)),
        ("13’ 2”", Some(13.0 * 0.3048 + 2.0 * 0.0254)),
        ("14'", Some(14.0 * 0.3048)),
        ("12 ft 6 in", Some(12.0 * 0.3048 + 6.0 * 0.0254)),
        ("12 ft", Some(12.0 * 0.3048)),
        ("default", None),
        ("below_default", None),
        ("none", None),
        ("unsigned", None),
        ("3.5;4.0", None),
        ("0", None),
        ("6 in 12 ft", None),
        ("3.5 km", None),
    ] {
        assert_close(parse_length_m(input), expected, input);
    }
}

#[test]
fn parses_weight_variants_seen_in_osm() {
    for (input, expected) in [
        ("7.5", Some(7.5)),
        ("5,4", Some(5.4)),
        ("7.5 t", Some(7.5)),
        ("7.5t", Some(7.5)),
        ("30 tonnes", Some(30.0)),
        ("7500 kg", Some(7.5)),
        ("16000 lbs", Some(16_000.0 * 0.000_453_592_37)),
        ("10 st", Some(10.0 * 0.907_184_74)),
        ("10 lt", Some(10.0 * 1.016_046_908_8)),
        ("none", None),
        ("signed", None),
        ("3 axles", None),
    ] {
        assert_close(parse_weight_t(input), expected, input);
    }
}

#[test]
fn collects_scoped_physical_restrictions_from_way_tags() {
    let restriction = |modes: u16, kind: Kind| VehicleRestriction {
        modes: AccessMask::new(modes),
        kind,
    };
    let vehicles = AccessMask::VEHICLES;
    let cases: Vec<(Vec<(&str, &str)>, Vec<VehicleRestriction>)> = vec![
        (
            vec![("maxheight", "3.8"), ("maxheight:physical", "3.9")],
            vec![
                restriction(vehicles, Kind::MaxHeightCm(380)),
                restriction(vehicles, Kind::MaxHeightCm(390)),
            ],
        ),
        (
            vec![("maxheight", "default"), ("maxheight:signed", "no")],
            vec![],
        ),
        (
            vec![("maxwidth", "2.3 m"), ("maxlength", "10")],
            vec![
                restriction(vehicles, Kind::MaxWidthCm(230)),
                restriction(vehicles, Kind::MaxLengthCm(1_000)),
            ],
        ),
        (
            vec![
                ("maxweight", "5.4"),
                ("maxweight:signed", "no"),
                ("maxaxleload", "4.8"),
                ("maxweight:bus", "30"),
            ],
            vec![
                restriction(vehicles, Kind::MaxWeightKg(5_400)),
                restriction(AccessMask::BUS, Kind::MaxWeightKg(30_000)),
                restriction(vehicles, Kind::MaxAxleLoadKg(4_800)),
            ],
        ),
        (
            vec![
                ("hazmat", "no"),
                ("hazmat:water", "no"),
                ("hazmat:adr_tunnel_cat", "C"),
            ],
            vec![
                restriction(vehicles, Kind::NoHazmat),
                restriction(vehicles, Kind::NoHazmatWater),
                restriction(vehicles, Kind::AdrTunnel(AdrTunnelCategory::C)),
            ],
        ),
        (vec![("hazmat", "designated")], vec![]),
        (
            vec![("trailer", "no"), ("hgv_articulated", "no")],
            vec![
                restriction(vehicles, Kind::NoTrailer),
                restriction(AccessMask::HGV, Kind::NoArticulated),
            ],
        ),
    ];
    for (pairs, expected) in cases {
        assert_eq!(
            way_vehicle_restrictions(&tags(&pairs)),
            expected,
            "{pairs:?}"
        );
    }
}

#[test]
fn maps_vehicle_property_conditions_onto_limits() {
    assert_eq!(
        conditional_limit("weight", "7.5"),
        Some(Kind::MaxWeightKg(7_500))
    );
    assert_eq!(
        conditional_limit("height", "3.2 m"),
        Some(Kind::MaxHeightCm(320))
    );
    assert_eq!(conditional_limit("wheels", "4"), None);
}
