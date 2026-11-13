use super::*;

fn directed_edge(index: u32, road_class: RoadClass) -> DirectedEdge {
    DirectedEdge {
        edge_id: EdgeId(index),
        from: NodeId(index),
        to: NodeId(index + 1),
        source_way_id: 70 + i64::from(index),
        length_m: 100 + index,
        ascent_m: 1.5,
        descent_m: 0.5,
        feature_row: NO_FEATURE_ROW,
        source_direction: -1,
        temporal_rule_id: (index == 1).then_some(4),
        duration_s: None,
        road_class,
        surface: SurfaceClass::Asphalt,
        smoothness: SmoothnessClass::Good,
        access_mask: AccessMask::new(AccessMask::ALL),
        is_toll: false,
        max_speed_kph: Some(50.0),
        lanes: None,
        name_index: (index == 0).then_some(9),
        flags: EDGE_FLAG_ROUNDABOUT,
    }
}

#[test]
fn loaded_records_are_compact() {
    assert_eq!(size_of::<RoutingEdge>(), 20);
    assert_eq!(size_of::<TopologyNode>(), 24);
}

#[test]
fn edge_columns_round_trip_through_the_gathered_view() {
    let edges = [
        directed_edge(0, RoadClass::Residential),
        directed_edge(1, RoadClass::Residential),
        directed_edge(2, RoadClass::Primary),
    ];
    let layers = TopologyEdgeLayers::from_directed_edges(&edges);
    layers.validate().expect("columns align");
    assert_eq!(layers.profile_rows().len(), 2);
    assert_eq!(layers.profile_row_index(), &[0, 0, 1]);

    let bundle = TopologyBundle {
        schema_version: TOPOLOGY_BUNDLE_SCHEMA_VERSION,
        source_path: String::new(),
        nodes: Vec::new(),
        edge_layers: layers,
        turn_restrictions: Vec::new(),
        names: Vec::new(),
        edge_based_topology: EdgeBasedTopology::default(),
        spatial_index: None,
        node_component_ids: Vec::new(),
        edge_component_ids: Vec::new(),
        feature_attributes: FeatureAttributeTable::default(),
        temporal_rule_sets: Vec::new(),
        regions: TopologyRegions::default(),
        elevation: TopologyElevation::default(),
    };
    for (index, expected) in edges.iter().enumerate() {
        let edge = bundle.edge(index);
        assert_eq!(edge.edge_id, expected.edge_id);
        assert_eq!(edge.from, expected.from);
        assert_eq!(edge.source_way_id, expected.source_way_id);
        assert_eq!(edge.length_m, expected.length_m);
        assert_eq!(edge.ascent_m, expected.ascent_m);
        assert_eq!(edge.descent_m, expected.descent_m);
        assert_eq!(edge.source_direction, expected.source_direction);
        assert_eq!(edge.temporal_rule_id, expected.temporal_rule_id);
        assert_eq!(edge.road_class, expected.road_class);
        assert_eq!(edge.name_index, expected.name_index);
        assert_eq!(edge.flags, expected.flags);
    }
}

#[test]
fn profile_updates_intern_rows() {
    let edges = [
        directed_edge(0, RoadClass::Residential),
        directed_edge(1, RoadClass::Residential),
    ];
    let mut layers = TopologyEdgeLayers::from_directed_edges(&edges);
    layers.update_profile(1, |profile| {
        profile.destination_only = AccessMask::new(AccessMask::CAR);
    });
    assert_eq!(layers.profile_rows().len(), 2);
    assert!(layers.profile(0).destination_only.is_empty());
    assert!(layers.profile(1).destination_only.contains(AccessMask::CAR));

    layers.update_profile(0, |profile| {
        profile.destination_only = AccessMask::new(AccessMask::CAR);
    });
    assert_eq!(layers.profile_row_index(), &[1, 1]);
    assert_eq!(layers.profile_rows().len(), 2);
}

#[test]
fn routing_edge_sentinels_read_as_absent() {
    let mut edge = RoutingEdge::new(NodeId(0), NodeId(1), 10, None, 0);
    assert_eq!(edge.temporal_rule_id(), None);
    edge.set_temporal_rule_id(Some(0));
    assert_eq!(edge.temporal_rule_id(), Some(0));
    assert_eq!(RoutingEdge::default().temporal_rule_id(), None);
}

#[test]
fn nodes_keep_source_coordinates() {
    let node = TopologyNode::new(114.180_003_000_4, 22.28, -18.5);
    assert_eq!(node.lon(), 114.180_003_000_4);
    assert_eq!(node.elevation_m(), Some(-18.5));
    assert_eq!(TopologyNode::new(6.5, 53.2, f64::NAN).elevation_m(), None);
}
