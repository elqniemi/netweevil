use super::{EdgeDirection, WayAccess, classify_direction, classify_way_access};
use crate::test_util::tags;
use netweevil_core::{
    AccessMask as A, EDGE_FLAG_INFERRED_BICYCLE_CONTRAFLOW, EDGE_FLAG_INFERRED_FOOT_REVERSE_ONEWAY,
    HighwayClass as H, RoadClass, TemporalEffect, VehicleRestriction, VehicleRestrictionKind,
};

fn access(highway: H, pairs: &[(&str, &str)]) -> WayAccess {
    classify_way_access(&tags(pairs), highway, highway.road_class())
}

#[test]
fn resolves_static_access_through_each_mode_hierarchy() {
    let all_roads = A::ALL;
    // (highway, tags, forward access, reverse access)
    let cases: Vec<(H, Vec<(&str, &str)>, u16, u16)> = vec![
        (H::Residential, vec![], all_roads, all_roads),
        (H::Motorway, vec![], A::MOTOR_VEHICLES & !A::MOPED, 0),
        (
            H::Cycleway,
            vec![],
            A::BICYCLE | A::FOOT,
            A::BICYCLE | A::FOOT,
        ),
        (
            H::Cycleway,
            vec![("moped", "designated")],
            A::BICYCLE | A::FOOT | A::MOPED,
            A::BICYCLE | A::FOOT | A::MOPED,
        ),
        (H::Busway, vec![], A::BUS, A::BUS),
        (
            H::Busway,
            vec![("psv", "yes")],
            A::BUS | A::TAXI,
            A::BUS | A::TAXI,
        ),
        (
            H::Residential,
            vec![("access", "no"), ("foot", "yes")],
            A::FOOT,
            A::FOOT,
        ),
        // The most specific key wins regardless of tag order.
        (
            H::Residential,
            vec![("motor_vehicle", "yes"), ("motorcar", "no")],
            all_roads & !A::CAR,
            all_roads & !A::CAR,
        ),
        (
            H::Residential,
            vec![("motor_vehicle", "no"), ("hgv", "yes"), ("psv", "yes")],
            A::HGV | A::BUS | A::TAXI | A::BICYCLE | A::FOOT,
            A::HGV | A::BUS | A::TAXI | A::BICYCLE | A::FOOT,
        ),
        (
            H::Residential,
            vec![("vehicle", "no"), ("bus", "designated")],
            A::BUS | A::FOOT,
            A::BUS | A::FOOT,
        ),
        // Direction-scoped values for one mode.
        (
            H::Residential,
            vec![("hgv:backward", "no")],
            all_roads,
            all_roads & !A::HGV,
        ),
        (
            H::Track,
            vec![("access", "agricultural"), ("foot", "yes")],
            A::FOOT,
            A::FOOT,
        ),
        (
            H::Primary,
            vec![("motorroad", "yes")],
            all_roads & !(A::FOOT | A::BICYCLE | A::MOPED),
            all_roads & !(A::FOOT | A::BICYCLE | A::MOPED),
        ),
        (H::Path, vec![("moped", "use_sidepath")], A::FOOT, A::FOOT),
    ];
    for (highway, pairs, forward, reverse) in cases {
        let way = access(highway, &pairs);
        assert_eq!(
            way.forward.access.0, forward,
            "{highway:?} {pairs:?} forward"
        );
        assert_eq!(
            way.reverse.access.0, reverse,
            "{highway:?} {pairs:?} reverse"
        );
    }
}

#[test]
fn marks_destination_only_access_per_mode() {
    let way = access(
        H::Residential,
        &[("motor_vehicle", "destination"), ("hgv", "delivery")],
    );
    assert_eq!(way.forward.destination_only.0, A::MOTOR_VEHICLES);
    assert!(way.forward.access.contains(A::HGV));
    let way = access(H::Service, &[("access", "customers"), ("foot", "yes")]);
    assert_eq!(way.forward.destination_only.0, A::VEHICLES);
    assert!(!way.forward.destination_only.contains(A::FOOT));
}

#[test]
fn applies_mode_specific_oneways() {
    let oneway = |pairs: &[(&str, &str)]| {
        let way = access(H::Residential, pairs);
        (way.forward.access.0, way.reverse.access.0)
    };
    let (_, reverse) = oneway(&[("oneway", "yes")]);
    assert_eq!(reverse & A::MOTOR_VEHICLES, 0);
    let (_, reverse) = oneway(&[("oneway", "yes"), ("oneway:bus", "no")]);
    assert_eq!(reverse & A::MOTOR_VEHICLES, A::BUS);
    let (_, reverse) = oneway(&[("oneway", "yes"), ("oneway:psv", "no")]);
    assert_eq!(reverse & A::MOTOR_VEHICLES, A::BUS | A::TAXI);
    let (_, reverse) = oneway(&[("oneway", "yes"), ("busway", "opposite_lane")]);
    assert_eq!(reverse & A::MOTOR_VEHICLES, A::BUS);
    let (forward, reverse) = oneway(&[("oneway", "-1"), ("oneway:moped", "no")]);
    assert_eq!(forward & A::MOTOR_VEHICLES, A::MOPED);
    assert_eq!(reverse & A::MOTOR_VEHICLES, A::MOTOR_VEHICLES);
}

#[test]
fn infers_oneway_behavior() {
    let direction =
        |pairs: &[(&str, &str)]| classify_direction(&tags(pairs), RoadClass::Residential);
    assert_eq!(direction(&[("oneway", "-1")]), EdgeDirection::ReverseOnly);
    assert_eq!(
        direction(&[("junction", "roundabout")]),
        EdgeDirection::ForwardOnly
    );
    assert_eq!(direction(&[]), EdgeDirection::Both);
}

#[test]
fn keeps_foot_and_bicycle_counterflow_inference() {
    let way = access(H::Residential, &[("oneway", "yes")]);
    assert!(way.forward.access.contains(A::CAR));
    assert!(!way.reverse.access.contains(A::CAR));
    assert!(way.reverse.access.contains(A::FOOT));
    assert!(way.reverse.access.contains(A::BICYCLE));
    assert_ne!(
        way.reverse.flags & EDGE_FLAG_INFERRED_FOOT_REVERSE_ONEWAY,
        0
    );
    assert_ne!(way.reverse.flags & EDGE_FLAG_INFERRED_BICYCLE_CONTRAFLOW, 0);

    let way = access(H::Cycleway, &[("oneway", "yes")]);
    assert!(way.forward.access.contains(A::BICYCLE));
    assert!(!way.reverse.access.contains(A::BICYCLE));
    assert!(way.reverse.access.contains(A::FOOT));
}

#[test]
fn maps_time_conditionals_onto_mode_scoped_rules() {
    // (tags, mode, static allowed, conditional access, rule effect)
    let cases: Vec<(Vec<(&str, &str)>, u16, bool, bool, Option<TemporalEffect>)> = vec![
        // Closures of statically open roads.
        (
            vec![("hgv:conditional", "no @ (Mo-Fr 07:00-18:00)")],
            A::HGV,
            true,
            false,
            Some(TemporalEffect::Closed),
        ),
        (
            vec![("hgv:conditional", "no @ (Mo-Fr 07:00-18:00)")],
            A::CAR,
            true,
            false,
            None,
        ),
        // Openings of statically closed roads.
        (
            vec![
                ("motor_vehicle", "no"),
                ("motor_vehicle:conditional", "yes @ (05:00-12:00)"),
            ],
            A::CAR,
            false,
            true,
            Some(TemporalEffect::OpenOnly),
        ),
        (
            vec![
                ("motor_vehicle", "no"),
                ("taxi:conditional", "yes @ (18:00-11:00)"),
            ],
            A::TAXI,
            false,
            true,
            Some(TemporalEffect::OpenOnly),
        ),
        // A more specific plain tag shadows a general conditional.
        (
            vec![
                ("access:conditional", "no @ (Sa 06:00-19:00)"),
                ("foot", "yes"),
            ],
            A::FOOT,
            true,
            false,
            None,
        ),
        // Destination-only windows keep the unconditional access.
        (
            vec![
                ("motor_vehicle", "no"),
                ("motor_vehicle:conditional", "delivery @ (05:00-12:00)"),
            ],
            A::CAR,
            false,
            false,
            None,
        ),
        // Sun-relative times are unsupported.
        (
            vec![("access:conditional", "no @ (sunset-sunrise)")],
            A::CAR,
            true,
            false,
            None,
        ),
        // First-match precedence of mixed polarities is not a union.
        (
            vec![(
                "motor_vehicle:conditional",
                "no @ (Mo-Fr 07:00-19:00); yes @ (Mo 08:00-09:00)",
            )],
            A::CAR,
            true,
            false,
            None,
        ),
        // An override agreeing with the static access after the last
        // opposite one shadows nothing and is dropped.
        (
            vec![(
                "motor_vehicle:conditional",
                "yes @ (Sa-Su); no @ (Mo-Fr 07:00-19:00)",
            )],
            A::CAR,
            true,
            false,
            Some(TemporalEffect::Closed),
        ),
    ];
    for (pairs, mode, allowed, conditional, effect) in cases {
        let way = access(H::Residential, &pairs);
        let direction = &way.forward;
        assert_eq!(direction.access.contains(mode), allowed, "{pairs:?}");
        assert_eq!(
            direction.conditional_access.contains(mode),
            conditional,
            "{pairs:?}"
        );
        let mode_effect = direction
            .rules
            .iter()
            .find(|rule| rule.applies_to_mode(mode))
            .map(|rule| rule.effect);
        assert_eq!(mode_effect, effect, "{pairs:?}");
    }
}

#[test]
fn closes_conditionally_by_direction_and_merges_identical_mode_rules() {
    let way = access(
        H::Residential,
        &[(
            "motor_vehicle:forward:conditional",
            "no @ (Mo-Fr 08:05-08:35)",
        )],
    );
    assert_eq!(way.forward.rules.len(), 1);
    assert_eq!(way.forward.rules[0].modes, A::MOTOR_VEHICLES);
    assert!(way.reverse.rules.is_empty());
}

#[test]
fn turns_vehicle_property_conditions_into_restrictions() {
    let way = access(
        H::Residential,
        &[
            ("hgv:conditional", "no @ (weight>7.5)"),
            ("motor_vehicle:conditional", "no @ (length>12)"),
            ("bus", "yes"),
        ],
    );
    assert_eq!(
        way.vehicle_restrictions,
        vec![
            VehicleRestriction {
                // `bus=yes` is more specific, so buses ignore the length
                // condition on `motor_vehicle`.
                modes: A::new(A::MOTOR_VEHICLES & !A::BUS),
                kind: VehicleRestrictionKind::MaxLengthCm(1_200),
            },
            VehicleRestriction {
                modes: A::new(A::HGV),
                kind: VehicleRestrictionKind::MaxWeightKg(7_500),
            },
        ]
    );
    assert_eq!(way.conditionals.imported, 6);
}
