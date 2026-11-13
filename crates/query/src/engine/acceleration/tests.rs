use super::*;
use netweevil_core::{
    CacheBundleId, CompiledAcceleration, CompiledProfileBundle, DatasetAccelerationBundle,
    TravelMode,
};
use std::sync::Arc;

/// Edges 0, 1, 2 in rank order: upward arcs 0 -> `upward_head_0` and
/// 1 -> 2, downward arc 1 -> 0.
fn hierarchy(upward_head_0: u32) -> Arc<DatasetAccelerationBundle> {
    Arc::new(
        DatasetAccelerationBundle::from_arcs(
            CacheBundleId::new("witness-test"),
            Default::default(),
            vec![0, 1, 2],
            &[(0, upward_head_0), (1, 2)],
            &[(1, 0)],
        )
        .expect("valid hierarchy"),
    )
}

/// Upward arc 1 has weight `direct_weight`; the triangle through edge 0
/// costs 2 + 4, so arc 1 unpacks through edge 0 only when that weight is 6.
fn graph(source: Arc<DatasetAccelerationBundle>, direct_weight: u32) -> AccelerationGraph {
    let metrics = Arc::new(CompiledProfileBundle {
        schema_version: 3,
        profile_id: "witness-test".into(),
        metric_key: direct_weight.to_string(),
        mode: TravelMode::Car,
        turn_costs: Default::default(),
        components: vec![],
        temporal: Default::default(),
        source_topology_bundle_id: CacheBundleId::new("witness-test"),
        edge_metrics: vec![],
        vehicle: Default::default(),
        acceleration: Some(CompiledAcceleration {
            source_hierarchy_key: source.hierarchy_key,
            schema_version: netweevil_core::COMPILED_ACCELERATION_SCHEMA_VERSION,
            source_acceleration_bundle_id: CacheBundleId::new("witness-test"),
            algorithm: netweevil_core::CCH_ALGORITHM.into(),
            upward_weight: vec![4, direct_weight].into(),
            downward_weight: vec![2].into(),
            time_upward_weight: vec![].into(),
            time_downward_weight: vec![].into(),
            distance_upward_weight: vec![].into(),
            distance_downward_weight: vec![].into(),
        }),
    });
    build_acceleration_graph(metrics, None, Some(source), 3)
        .expect("valid hierarchy")
        .expect("accelerated graph")
}

fn unpack(graph: &AccelerationGraph) -> Vec<usize> {
    let mut path = vec![1];
    push_unpacked_arc(
        graph,
        &mut UnpackStack::default(),
        HierarchyArc::between(&graph.source, 1, 2).expect("arc 1 -> 2"),
        &mut path,
    );
    path
}

#[test]
fn cached_witnesses_follow_metric_and_topology_owners() {
    let a = graph(hierarchy(2), 6);
    let b = graph(a.source.clone(), 5);
    for _ in 0..3 {
        assert_eq!(unpack(&a), [1, 0, 2]);
        assert_eq!(unpack(&a), [1, 0, 2]);
        assert_eq!(unpack(&b), [1, 2]);
    }
    let c = graph(hierarchy(1), 6);
    assert_eq!(unpack(&a), [1, 0, 2]);
    assert_eq!(unpack(&c), [1, 2]);
    drop(a);
    drop(b);
    drop(c);
    assert_eq!(unpack(&graph(hierarchy(2), 5)), [1, 2]);
}

#[test]
fn ranked_rows_hold_the_weights_at_their_arc_positions() {
    let graph = graph(hierarchy(2), 6);
    let source = graph.source.as_ref();
    assert_eq!(graph.ranked.weight_vectors(), (vec![4, 6], vec![2]));
    assert_eq!(source.upward_position(1, 2), Some(1));
    assert_eq!(source.downward_position(1, 0), Some(0));
    assert_eq!(source.downward_position(2, 0), None);
    assert_eq!(graph.ranked.downward_arc(0), (1, 2));
    assert_eq!(graph.ranked.upward_row(0).collect::<Vec<_>>(), [(2, 4)]);
    assert_eq!(graph.ranked.downward_row(0).collect::<Vec<_>>(), [(1, 2)]);
}
