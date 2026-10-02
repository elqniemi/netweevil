use super::{
    TOPOLOGY_BUNDLE_SCHEMA_VERSION, apportioned_duration_s, build_topology_from_scan,
    label_weak_components,
};
use crate::scan::{ObjectCounts, ScanOutput};
use crate::test_util::{edge, pending_way};
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
