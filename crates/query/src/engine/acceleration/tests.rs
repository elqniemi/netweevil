use super::*;
use netweevil_core::{
    CacheBundleId, CompiledAcceleration, CompiledProfileBundle, DatasetAccelerationBundle,
    TravelMode,
};
use std::sync::Arc;

/// Edges 0, 1, 2 in rank order: upward arcs 0 -> `upward_head_0` and
/// 1 -> 2, downward arc 1 -> 0.
fn hierarchy(upward_head_0: u32) -> Arc<DatasetAccelerationBundle> {
    Arc::new(DatasetAccelerationBundle {
        schema_version: netweevil_core::ACCELERATION_BUNDLE_SCHEMA_VERSION,
        source_topology_bundle_id: CacheBundleId::new("witness-test"),
        algorithm: netweevil_core::CCH_ALGORITHM.into(),
        stats: Default::default(),
        edge_order: vec![0, 1, 2],
        edge_rank: vec![0, 1, 2],
        upward_first_out: vec![0, 1, 2, 2],
        upward_head: vec![upward_head_0, 2],
        downward_first_out: vec![0, 0, 1, 1],
        downward_head: vec![0],
    })
}

/// Upward arc 1 has weight `direct_weight`; the triangle through edge 0
/// costs 2 + 4, so arc 1 unpacks through edge 0 only when that weight is 6.
fn graph(source: Arc<DatasetAccelerationBundle>, direct_weight: u32) -> AccelerationGraph {
    let metrics = Arc::new(CompiledProfileBundle {
        schema_version: 3,
        profile_id: "witness-test".into(),
        profile_hash: direct_weight.to_string(),
        mode: TravelMode::Car,
        turn_costs: Default::default(),
        components: vec![],
        temporal: Default::default(),
        source_topology_bundle_id: CacheBundleId::new("witness-test"),
        edge_metrics: vec![],
        acceleration: Some(CompiledAcceleration {
            schema_version: netweevil_core::COMPILED_ACCELERATION_SCHEMA_VERSION,
            source_acceleration_bundle_id: CacheBundleId::new("witness-test"),
            algorithm: netweevil_core::CCH_ALGORITHM.into(),
            upward_weight: vec![4, direct_weight],
            downward_weight: vec![2],
            time_upward_weight: vec![],
            time_downward_weight: vec![],
            distance_upward_weight: vec![],
            distance_downward_weight: vec![],
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
        HierarchyArc::upward(1, 1),
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
fn ranked_rows_hold_the_slot_ordered_weights() {
    let graph = graph(hierarchy(2), 6);
    let source = graph.source.as_ref();
    assert_eq!(graph.ranked.slot_weights(source), (vec![4, 6], vec![2]));
    assert_eq!(graph.ranked.upward_weight(source, 1, 1), 6);
    assert_eq!(graph.ranked.downward_weight(source, 1, 0), Some(2));
    assert_eq!(graph.ranked.downward_weight(source, 2, 0), None);
    assert_eq!(graph.downward_slots(), [0]);
    assert_eq!(graph.ranked.downward_arc(0), (1, 2));
}
