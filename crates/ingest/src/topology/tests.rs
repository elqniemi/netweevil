use super::{
    TOPOLOGY_BUNDLE_SCHEMA_VERSION, apportioned_duration_s, build_topology_from_scan,
    label_weak_components,
};
use crate::access::ConditionalCounts;
use crate::scan::{ObjectCounts, RuleSetInterner, ScanOutput, pending_osm_way};
use crate::test_util::{edge, pending_way, tags};
use netweevil_core::{AccessMask, TemporalEffect, VehicleRestrictionKind};
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::Path;

#[test]
fn builds_directional_three_dimensional_edge_quantities() {
    let scan = ScanOutput {
        pending_ways: vec![pending_way(10, &[100, 200])],
        restriction_candidates: Vec::new(),
        node_coords: FxHashMap::from_iter([(100, (114.1, 22.3, 10.0)), (200, (114.1, 22.3, 22.0))]),
        traffic_signal_nodes: FxHashSet::default(),
        junction_nodes: Default::default(),
        counts: ObjectCounts {
            nodes: 2,
            ways: 1,
            relations: 0,
        },
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
        ..ScanOutput::default()
    };
    let (bundle, _, _) =
        build_topology_from_scan(Path::new("station.gpkg"), "abc", scan, &mut |_| {})
            .expect("3D topology builds");

    assert_eq!(bundle.schema_version, TOPOLOGY_BUNDLE_SCHEMA_VERSION);
    assert_eq!(bundle.nodes[0].z, 10.0);
    assert_eq!(bundle.nodes[1].z, 22.0);
    assert_eq!(bundle.edge_count(), 2);
    let forward = bundle.routing_edge(0);
    let reverse = bundle.routing_edge(1);
    assert_eq!(forward.length_m, 12);
    assert_eq!((forward.ascent_m, forward.descent_m), (12.0, 0.0));
    assert_eq!((reverse.ascent_m, reverse.descent_m), (0.0, 12.0));
}

#[test]
fn apportions_way_duration_by_segment_length() {
    // 250 m of a 1 km way gets a quarter of its duration.
    let duration = apportioned_duration_s(Some(600.0), 250, 1_000, 2).expect("duration");
    assert!((duration - 150.0).abs() < f64::EPSILON);
    // A zero-length way splits its duration evenly across segments.
    let duration = apportioned_duration_s(Some(600.0), 0, 0, 3).expect("duration");
    assert!((duration - 200.0).abs() < f64::EPSILON);
}

#[test]
fn labels_weak_components_for_disconnected_subnetworks() {
    let labels = label_weak_components(
        4,
        &[edge(0, 0, 1, 10), edge(1, 1, 0, 10), edge(2, 2, 3, 20)],
    );

    assert_eq!(labels.component_count, 2);
    assert_eq!(labels.node_component_ids, vec![0, 0, 1, 1]);
    assert_eq!(labels.edge_component_ids, vec![0, 0, 1]);
    assert_eq!(labels.largest_component_node_count, 2);
    assert_eq!(labels.largest_component_edge_count, 2);
}

#[test]
fn carries_tagged_restrictions_onto_directed_edges() {
    let mut rule_sets = RuleSetInterner::default();
    let mut conditionals = ConditionalCounts::default();
    let mut way = |id: i64, nodes: &[i64], pairs: &[(&str, &str)]| {
        pending_osm_way(
            id,
            nodes.to_vec(),
            &tags(pairs),
            &mut rule_sets,
            &mut conditionals,
        )
        .expect("routable way")
    };
    let ways = vec![
        way(
            10,
            &[1, 2],
            &[
                ("highway", "residential"),
                ("oneway", "yes"),
                ("hgv", "destination"),
                ("maxheight", "3.5"),
                ("hgv:conditional", "no @ (Mo-Fr 07:00-18:00)"),
            ],
        ),
        way(
            20,
            &[2, 3],
            &[
                ("highway", "pedestrian"),
                ("motor_vehicle:conditional", "yes @ (05:00-12:00)"),
            ],
        ),
    ];
    let scan = ScanOutput {
        pending_ways: ways,
        node_coords: FxHashMap::from_iter([
            (1, (6.56, 53.21, 0.0)),
            (2, (6.561, 53.21, 0.0)),
            (3, (6.562, 53.21, 0.0)),
        ]),
        temporal_rule_sets: rule_sets.into_sets(),
        ..ScanOutput::default()
    };
    let (bundle, _, _) =
        build_topology_from_scan(Path::new("test.osm.pbf"), "abc", scan, &mut |_| {})
            .expect("topology builds");

    // Way 10: forward for every mode, reverse for foot and bicycle only.
    // Way 20: both directions for pedestrians, cars only by condition.
    assert_eq!(bundle.edge_count(), 4);
    let forward = bundle.edge_profile(0);
    assert!(forward.access_mask.contains(AccessMask::CAR));
    assert_eq!(forward.destination_only.0, AccessMask::HGV);
    let forward_rules = &bundle.temporal_rule_sets[bundle
        .routing_edge(0)
        .temporal_rule_id
        .expect("forward rules") as usize];
    assert_eq!(forward_rules.rules[0].effect, TemporalEffect::Closed);
    assert_eq!(forward_rules.rules[0].modes, AccessMask::HGV);
    assert_eq!(bundle.routing_edge(1).temporal_rule_id, None);

    let restrictions = &bundle.edge_layers.vehicle_restrictions;
    assert_eq!(restrictions.edges, vec![0, 1]);
    assert_eq!(restrictions.restrictions.len(), 1);
    assert_eq!(
        restrictions.restrictions[0].kind,
        VehicleRestrictionKind::MaxHeightCm(350)
    );

    for edge_index in [2, 3] {
        let pedestrian = bundle.edge_profile(edge_index);
        assert_eq!(pedestrian.access_mask.0, AccessMask::FOOT);
        assert!(pedestrian.conditional_access.contains(AccessMask::CAR));
        let rules = &bundle.temporal_rule_sets[bundle
            .routing_edge(edge_index)
            .temporal_rule_id
            .expect("rules") as usize];
        assert!(
            rules
                .rules
                .iter()
                .all(|rule| rule.effect == TemporalEffect::OpenOnly)
        );
    }
}
