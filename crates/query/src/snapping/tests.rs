use super::*;
use netweevil_core::{NodeId, NodeSpatialIndex, SpatialIndexCell, TopologyBounds};

#[test]
fn finite_global_snap_radius_is_bounded_by_the_node_grid() {
    let topology = TopologyBundle {
        schema_version: 1,
        source_path: "test".into(),
        nodes: vec![TopologyNode {
            node_id: NodeId(0),
            lon: 6.0,
            lat: 53.0,
            z: 0.0,
        }],
        edge_layers: Default::default(),
        feature_attributes: Default::default(),
        temporal_rule_sets: vec![],
        regions: Default::default(),
        elevation: Default::default(),
        turn_restrictions: vec![],
        names: vec![],
        edge_based_topology: Default::default(),
        spatial_index: None,
        node_component_ids: vec![0],
        edge_component_ids: vec![],
    };
    let index = NodeSpatialIndex {
        bounds: TopologyBounds {
            min_lon: 6.0,
            min_lat: 53.0,
            max_lon: 6.001,
            max_lat: 53.001,
        },
        columns: 1,
        rows: 1,
        cell_width_deg: 0.001,
        cell_height_deg: 0.001,
        cells: vec![SpatialIndexCell {
            node_start: 0,
            node_len: 1,
        }],
        node_ids: vec![0],
    };
    for (lon, lat) in [(6.0, 53.0), (-180.0, -90.0), (180.0, 90.0)] {
        let point = LabeledPoint {
            id: "global".into(),
            lon,
            lat,
            z: None,
        };
        let candidates = spatial_snap_nodes(&topology, &index, &point, f64::MAX);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, 0);
    }
}

#[test]
fn nearest_segments_group_directions_and_rank_by_distance() {
    use crate::tests::fixtures::{forbidden_uturn_topology, point, snap_within, uniform_metrics};
    // Two-way segment 0 <-> 1 along the latitude, one-way edge 2 north.
    let topology = forbidden_uturn_topology();
    let metrics = uniform_metrics(topology.edge_count(), 10.0);
    let graph = build_routing_graph(&topology, &metrics).unwrap();
    let located = point("p", 6.0004, 53.0001);

    let nearest = nearest_segments(&topology, &graph, &located, &snap_within(500.0), 5).unwrap();
    let edges = nearest
        .iter()
        .map(|segment| {
            let mut edges = segment
                .edges
                .iter()
                .map(|edge| edge.edge_index)
                .collect::<Vec<_>>();
            edges.sort_unstable();
            edges
        })
        .collect::<Vec<_>>();
    assert_eq!(edges, vec![vec![0, 1], vec![2]]);
    assert!(nearest[0].distance_m < nearest[1].distance_m);
    let fractions = nearest[0]
        .edges
        .iter()
        .map(|edge| edge.fraction)
        .sum::<f64>();
    assert!(
        (fractions - 1.0).abs() < 1e-9,
        "opposite directions mirror the fraction"
    );

    let only = nearest_segments(&topology, &graph, &located, &snap_within(500.0), 1).unwrap();
    assert_eq!(only.len(), 1);
    assert!(
        nearest_segments(&topology, &graph, &located, &snap_within(1.0), 3)
            .unwrap()
            .is_empty()
    );
}
