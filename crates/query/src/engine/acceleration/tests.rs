use super::*;
use netweevil_core::{
    CacheBundleId, CompiledAcceleration, CompiledProfileBundle, DatasetAccelerationBundle,
    TravelMode,
};
use std::sync::Arc;

/// Upward arc 1 has weight `direct_weight`; the triangle through edge 0
/// costs 4 + 2, so arc 1 unpacks through edge 0 only when that weight is 6.
fn graph(direct_weight: u32) -> AccelerationGraph {
    AccelerationGraph {
        source: Arc::new(DatasetAccelerationBundle {
            schema_version: netweevil_core::ACCELERATION_BUNDLE_SCHEMA_VERSION,
            source_topology_bundle_id: CacheBundleId::new("witness-test"),
            algorithm: netweevil_core::CCH_ALGORITHM.into(),
            stats: Default::default(),
            edge_order: vec![0, 1, 2],
            edge_rank: vec![0, 1, 2],
            upward_first_out: vec![0, 1, 2, 2],
            upward_head: vec![2, 2],
            downward_first_out: vec![0, 0, 1, 1],
            downward_head: vec![0],
        }),
        metrics: Arc::new(CompiledProfileBundle {
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
        }),
        upward_tail: vec![0, 1],
        downward_tail: vec![1],
        reverse_downward_first_out: vec![0, 1, 1, 1],
        reverse_downward_edge: vec![1],
        reverse_downward_arc: vec![0],
        deferred_metric_weights: None,
        unpack: UnpackTable::new(2, 1),
        ranked: Default::default(),
    }
}

fn unpack(graph: &AccelerationGraph) -> Vec<usize> {
    let mut path = vec![1];
    push_unpacked_arc(graph, &mut Vec::new(), true, 1, &mut path);
    path
}

#[test]
fn cached_witnesses_follow_metric_and_topology_owners() {
    let a = graph(6);
    let mut b = graph(5);
    b.source = a.source.clone();
    for _ in 0..3 {
        assert_eq!(unpack(&a), [1, 0, 2]);
        assert_eq!(unpack(&a), [1, 0, 2]);
        assert_eq!(unpack(&b), [1, 2]);
    }
    let mut c = graph(6);
    c.metrics = a.metrics.clone();
    Arc::make_mut(&mut c.source).upward_head[0] = 1;
    assert_eq!(unpack(&a), [1, 0, 2]);
    assert_eq!(unpack(&c), [1, 2]);
    drop(a);
    drop(b);
    drop(c);
    assert_eq!(unpack(&graph(5)), [1, 2]);
}
