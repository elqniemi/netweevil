use netweevil_core::{
    AccessMask, CacheBundleId, CompiledAcceleration, CompiledEdgeMetric, CompiledProfileBundle,
    CompiledTurnCostConfig, DirectedEdge, EDGE_FLAG_ROUNDABOUT, EDGE_FLAG_TARGET_TRAFFIC_SIGNAL,
    EdgeId, NodeId, NodeSpatialIndex, RoadClass, SmoothnessClass, SpatialIndexCell, SurfaceClass,
    TopologyBounds, TopologyBundle, TopologyEdgeLayers, TopologyNode, TravelMode, TurnRestriction,
    TurnRestrictionKind,
};

pub(super) fn test_topology() -> TopologyBundle {
    with_edge_based_topology(TopologyBundle {
        schema_version: 1,
        source_path: "test".to_string(),
        source_sha256: "abc".to_string(),
        nodes: vec![
            TopologyNode {
                node_id: NodeId(0),
                lon: 6.0,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(1),
                lon: 6.001,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(2),
                lon: 6.002,
                lat: 53.0,
                z: 0.0,
            },
        ],
        edge_layers: TopologyEdgeLayers::from_directed_edges(&[
            DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 10,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(1),
                from: NodeId(1),
                to: NodeId(2),
                source_way_id: 10,
                length_m: 200,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(2),
                from: NodeId(0),
                to: NodeId(2),
                source_way_id: 11,
                length_m: 500,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Service,
                surface: SurfaceClass::Paved,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
        ]),
        turn_restrictions: vec![],
        names: vec![],
        edge_based_topology: Default::default(),
        spatial_index: Some(spatial_index_for_all_nodes(3, 6.0, 53.0, 6.002, 53.0)),
        node_component_ids: vec![],
        edge_component_ids: vec![],
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
    })
}

pub(super) fn test_metrics() -> CompiledProfileBundle {
    CompiledProfileBundle {
        schema_version: 2,
        profile_id: "test".to_string(),
        profile_hash: "abc".to_string(),
        mode: TravelMode::Car,
        turn_costs: CompiledTurnCostConfig::default(),
        components: Vec::new(),
        temporal: Default::default(),
        source_topology_bundle_id: CacheBundleId::new("topology-test"),
        acceleration: None,
        edge_metrics: vec![
            CompiledEdgeMetric {
                edge_id: EdgeId(0),
                travel_time_s: Some(10.0),
                generalized_cost: Some(10.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(1),
                travel_time_s: Some(20.0),
                generalized_cost: Some(20.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(2),
                travel_time_s: Some(100.0),
                generalized_cost: Some(100.0),
            },
        ],
    }
}

pub(super) fn service_area_linear_topology() -> TopologyBundle {
    with_edge_based_topology(TopologyBundle {
        schema_version: 2,
        source_path: "service-area-linear".to_string(),
        source_sha256: "abc".to_string(),
        nodes: vec![
            TopologyNode {
                node_id: NodeId(0),
                lon: 6.0,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(1),
                lon: 6.001,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(2),
                lon: 6.002,
                lat: 53.0,
                z: 0.0,
            },
        ],
        edge_layers: TopologyEdgeLayers::from_directed_edges(&[
            DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 30,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(1),
                from: NodeId(1),
                to: NodeId(2),
                source_way_id: 31,
                length_m: 200,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
        ]),
        turn_restrictions: vec![],
        names: vec![],
        edge_based_topology: Default::default(),
        spatial_index: Some(spatial_index_for_all_nodes(3, 6.0, 53.0, 6.002, 53.0)),
        node_component_ids: vec![],
        edge_component_ids: vec![],
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
    })
}

pub(super) fn service_area_linear_metrics() -> CompiledProfileBundle {
    CompiledProfileBundle {
        schema_version: 2,
        profile_id: "service-area-test".to_string(),
        profile_hash: "abc".to_string(),
        mode: TravelMode::Car,
        turn_costs: CompiledTurnCostConfig::default(),
        components: Vec::new(),
        temporal: Default::default(),
        source_topology_bundle_id: CacheBundleId::new("service-area-test"),
        acceleration: None,
        edge_metrics: vec![
            CompiledEdgeMetric {
                edge_id: EdgeId(0),
                travel_time_s: Some(10.0),
                generalized_cost: Some(10.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(1),
                travel_time_s: Some(20.0),
                generalized_cost: Some(20.0),
            },
        ],
    }
}

/// Builds a complete CCH for a fixture topology with an independent
/// mini-implementation: identity elimination order, complete elimination
/// game, and weight customization replaying the elimination with the routing
/// graph's own transition costs. Returns the dataset bundle plus the metrics
/// with the customized weights attached, ready for
/// `PreparedRoutingEngine::new(.., Some(bundle))`.
pub(super) fn build_test_cch(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
) -> (
    std::sync::Arc<netweevil_core::DatasetAccelerationBundle>,
    CompiledProfileBundle,
) {
    use netweevil_core::{
        ACCELERATION_BUNDLE_SCHEMA_VERSION, CCH_ALGORITHM, DatasetAccelerationBundle, NO_MIDDLE,
    };
    use std::collections::BTreeMap;

    let graph = crate::build_routing_graph(topology, metrics).expect("routing graph builds");
    let edge_count = topology.edge_count();

    // Arc topology mirrors the real preprocessor: arcs exist for every
    // TOPOLOGICAL transition (restrictions included) plus their elimination
    // closure; weights decide traversability. Identity order: rank == id.
    let mut arc_set = std::collections::BTreeSet::<(u32, u32)>::new();
    let transition_topology = &topology.edge_based_topology;
    for from_edge in 0..edge_count {
        let start = transition_topology.edge_transition_first_out[from_edge] as usize;
        let end = transition_topology.edge_transition_first_out[from_edge + 1] as usize;
        for &to_edge in &transition_topology.edge_transition_edges[start..end] {
            if to_edge as usize != from_edge && (to_edge as usize) < edge_count {
                arc_set.insert((from_edge as u32, to_edge));
            }
        }
    }
    for middle in 0..edge_count as u32 {
        let incoming = arc_set
            .iter()
            .filter(|&&(tail, head)| head == middle && tail > middle)
            .map(|&(tail, _)| tail)
            .collect::<Vec<_>>();
        let outgoing = arc_set
            .range((middle, 0)..(middle + 1, 0))
            .filter(|&&(_, head)| head > middle)
            .map(|&(_, head)| head)
            .collect::<Vec<_>>();
        for &tail in &incoming {
            for &head in &outgoing {
                if tail != head {
                    arc_set.insert((tail, head));
                }
            }
        }
    }

    // Customize the fixed arc set for any base transition weight, mirroring
    // the real compiler.
    let customize = |base: &dyn Fn(usize, usize) -> f64| -> BTreeMap<(u32, u32), (f64, u32)> {
        let mut arcs = BTreeMap::<(u32, u32), (f64, u32)>::new();
        for &(tail, head) in &arc_set {
            arcs.insert(
                (tail, head),
                (base(tail as usize, head as usize), NO_MIDDLE),
            );
        }
        for middle in 0..edge_count as u32 {
            let incoming = arcs
                .range((0, 0)..(u32::MAX, 0))
                .filter(|&(&(tail, head), _)| head == middle && tail > middle)
                .map(|(&(tail, _), &(weight, _))| (tail, weight))
                .collect::<Vec<_>>();
            let outgoing = arcs
                .range((middle, 0)..(middle + 1, 0))
                .filter(|&(&(_, head), _)| head > middle)
                .map(|(&(_, head), &(weight, _))| (head, weight))
                .collect::<Vec<_>>();
            for &(tail, incoming_weight) in &incoming {
                for &(head, outgoing_weight) in &outgoing {
                    if tail == head {
                        continue;
                    }
                    if !incoming_weight.is_finite() || !outgoing_weight.is_finite() {
                        continue;
                    }
                    let candidate = incoming_weight + outgoing_weight;
                    let entry = arcs
                        .entry((tail, head))
                        .or_insert((f64::INFINITY, NO_MIDDLE));
                    if candidate < entry.0 {
                        *entry = (candidate, middle);
                    }
                }
            }
        }
        arcs
    };

    let routing_transition_weight = |from_edge: usize, to_edge: usize| -> f64 {
        graph
            .transition_range(from_edge)
            .filter(|&index| graph.transition_edges[index] as usize == to_edge)
            .map(|index| graph.transition_costs[index])
            .fold(f64::INFINITY, f64::min)
    };

    let arcs = customize(&routing_transition_weight);
    let time_arcs = customize(&|from_edge, to_edge| {
        if !routing_transition_weight(from_edge, to_edge).is_finite() {
            return f64::INFINITY;
        }
        let Some(travel_time_s) = metrics.edge_metrics[to_edge].travel_time_s else {
            return f64::INFINITY;
        };
        travel_time_s + crate::turn_penalty_seconds(topology, metrics, from_edge, to_edge)
    });
    let distance_arcs = customize(&|from_edge, to_edge| {
        if !routing_transition_weight(from_edge, to_edge).is_finite() {
            return f64::INFINITY;
        }
        topology.routing_edge(to_edge).length_m as f64
    });

    let mut upward_first_out = vec![0_u32; edge_count + 1];
    let mut upward_head = Vec::new();
    let mut upward_weight = Vec::new();
    let mut upward_middle = Vec::new();
    let mut downward_first_out = vec![0_u32; edge_count + 1];
    let mut downward_head = Vec::new();
    let mut downward_weight = Vec::new();
    let mut downward_middle = Vec::new();
    let mut time_upward_weight = Vec::new();
    let mut time_downward_weight = Vec::new();
    let mut distance_upward_weight = Vec::new();
    let mut distance_downward_weight = Vec::new();
    let metric_weight = |arcs: &BTreeMap<(u32, u32), (f64, u32)>, tail: u32, head: u32| {
        arcs.get(&(tail, head))
            .map(|&(weight, _)| weight)
            .unwrap_or(f64::INFINITY)
    };
    for (&(tail, head), &(weight, middle)) in &arcs {
        if tail < head {
            upward_first_out[tail as usize + 1] += 1;
            upward_head.push(head);
            upward_weight.push(weight);
            upward_middle.push(middle);
            time_upward_weight.push(metric_weight(&time_arcs, tail, head));
            distance_upward_weight.push(metric_weight(&distance_arcs, tail, head));
        } else {
            downward_first_out[tail as usize + 1] += 1;
            downward_head.push(head);
            downward_weight.push(weight);
            downward_middle.push(middle);
            time_downward_weight.push(metric_weight(&time_arcs, tail, head));
            distance_downward_weight.push(metric_weight(&distance_arcs, tail, head));
        }
    }
    for edge_index in 0..edge_count {
        upward_first_out[edge_index + 1] += upward_first_out[edge_index];
        downward_first_out[edge_index + 1] += downward_first_out[edge_index];
    }

    let bundle = std::sync::Arc::new(DatasetAccelerationBundle {
        schema_version: ACCELERATION_BUNDLE_SCHEMA_VERSION,
        source_topology_bundle_id: metrics.source_topology_bundle_id.clone(),
        algorithm: CCH_ALGORITHM.to_string(),
        stats: Default::default(),
        edge_order: (0..edge_count as u32).collect(),
        edge_rank: (0..edge_count as u32).collect(),
        upward_first_out,
        upward_head,
        downward_first_out,
        downward_head,
    });
    let mut metrics = metrics.clone();
    metrics.acceleration = Some(CompiledAcceleration {
        schema_version: 3,
        source_acceleration_bundle_id: CacheBundleId::new("test-cch"),
        algorithm: CCH_ALGORITHM.to_string(),
        upward_weight,
        upward_middle,
        downward_weight,
        downward_middle,
        time_upward_weight,
        time_downward_weight,
        distance_upward_weight,
        distance_downward_weight,
    });
    (bundle, metrics)
}

pub(super) fn restricted_topology() -> TopologyBundle {
    with_edge_based_topology(TopologyBundle {
        schema_version: 3,
        source_path: "test".to_string(),
        source_sha256: "abc".to_string(),
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
        nodes: vec![
            TopologyNode {
                node_id: NodeId(0),
                lon: 6.0,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(1),
                lon: 6.001,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(2),
                lon: 6.002,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(3),
                lon: 6.003,
                lat: 53.0,
                z: 0.0,
            },
        ],
        edge_layers: TopologyEdgeLayers::from_directed_edges(&[
            DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 10,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(1),
                from: NodeId(1),
                to: NodeId(2),
                source_way_id: 11,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(2),
                from: NodeId(2),
                to: NodeId(3),
                source_way_id: 12,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(3),
                from: NodeId(0),
                to: NodeId(2),
                source_way_id: 20,
                length_m: 200,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Service,
                surface: SurfaceClass::Paved,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(4),
                from: NodeId(2),
                to: NodeId(3),
                source_way_id: 21,
                length_m: 200,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Service,
                surface: SurfaceClass::Paved,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
        ]),
        turn_restrictions: vec![TurnRestriction {
            relation_id: 99,
            kind: TurnRestrictionKind::NoTurn,
            edge_path: vec![EdgeId(1), EdgeId(2)],
            mode_mask: AccessMask::new(AccessMask::CAR),
        }],
        names: vec![],
        edge_based_topology: Default::default(),
        spatial_index: Some(spatial_index_for_all_nodes(4, 6.0, 53.0, 6.003, 53.0)),
        node_component_ids: vec![],
        edge_component_ids: vec![],
    })
}

pub(super) fn spatial_index_for_all_nodes(
    node_count: u32,
    min_lon: f64,
    min_lat: f64,
    max_lon: f64,
    max_lat: f64,
) -> NodeSpatialIndex {
    NodeSpatialIndex {
        bounds: TopologyBounds {
            min_lon,
            min_lat,
            max_lon,
            max_lat,
        },
        columns: 1,
        rows: 1,
        cell_width_deg: (max_lon - min_lon).max(0.001),
        cell_height_deg: (max_lat - min_lat).max(0.001),
        cells: vec![SpatialIndexCell {
            node_start: 0,
            node_len: node_count,
        }],
        node_ids: (0..node_count).collect(),
    }
}

pub(super) fn restricted_metrics() -> CompiledProfileBundle {
    CompiledProfileBundle {
        schema_version: 2,
        profile_id: "test".to_string(),
        profile_hash: "abc".to_string(),
        mode: TravelMode::Car,
        turn_costs: CompiledTurnCostConfig::default(),
        components: Vec::new(),
        temporal: Default::default(),
        source_topology_bundle_id: CacheBundleId::new("topology-test"),
        acceleration: None,
        edge_metrics: vec![
            CompiledEdgeMetric {
                edge_id: EdgeId(0),
                travel_time_s: Some(10.0),
                generalized_cost: Some(10.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(1),
                travel_time_s: Some(10.0),
                generalized_cost: Some(10.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(2),
                travel_time_s: Some(10.0),
                generalized_cost: Some(10.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(3),
                travel_time_s: Some(20.0),
                generalized_cost: Some(20.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(4),
                travel_time_s: Some(20.0),
                generalized_cost: Some(20.0),
            },
        ],
    }
}

pub(super) fn turn_penalty_topology() -> TopologyBundle {
    with_edge_based_topology(TopologyBundle {
        schema_version: 3,
        source_path: "test".to_string(),
        source_sha256: "abc".to_string(),
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
        nodes: vec![
            TopologyNode {
                node_id: NodeId(0),
                lon: 6.0,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(1),
                lon: 6.001,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(2),
                lon: 6.001,
                lat: 53.001,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(3),
                lon: 6.002,
                lat: 53.001,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(4),
                lon: 6.0,
                lat: 53.001,
                z: 0.0,
            },
        ],
        edge_layers: TopologyEdgeLayers::from_directed_edges(&[
            DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 10,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(1),
                from: NodeId(1),
                to: NodeId(2),
                source_way_id: 11,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(2),
                from: NodeId(2),
                to: NodeId(3),
                source_way_id: 12,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(3),
                from: NodeId(0),
                to: NodeId(4),
                source_way_id: 20,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Service,
                surface: SurfaceClass::Paved,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(4),
                from: NodeId(4),
                to: NodeId(3),
                source_way_id: 21,
                length_m: 200,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Service,
                surface: SurfaceClass::Paved,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
        ]),
        turn_restrictions: vec![],
        names: vec![],
        edge_based_topology: Default::default(),
        spatial_index: Some(spatial_index_for_all_nodes(5, 6.0, 53.0, 6.002, 53.001)),
        node_component_ids: vec![],
        edge_component_ids: vec![],
    })
}

pub(super) fn multi_edge_restricted_topology() -> TopologyBundle {
    let mut topology = restricted_topology();
    topology.turn_restrictions = vec![TurnRestriction {
        relation_id: 100,
        kind: TurnRestrictionKind::NoTurn,
        edge_path: vec![EdgeId(0), EdgeId(1), EdgeId(2)],
        mode_mask: AccessMask::new(AccessMask::CAR),
    }];
    topology
}

pub(super) fn traffic_signal_penalty_topology() -> TopologyBundle {
    let mut topology = turn_penalty_topology();
    topology.set_edge_flags(1, EDGE_FLAG_TARGET_TRAFFIC_SIGNAL);
    topology
}

pub(super) fn roundabout_entry_penalty_topology() -> TopologyBundle {
    with_edge_based_topology(TopologyBundle {
        schema_version: 3,
        source_path: "test".to_string(),
        source_sha256: "abc".to_string(),
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
        nodes: vec![
            TopologyNode {
                node_id: NodeId(0),
                lon: 6.0,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(1),
                lon: 6.001,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(2),
                lon: 6.002,
                lat: 53.001,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(3),
                lon: 6.0,
                lat: 53.001,
                z: 0.0,
            },
        ],
        edge_layers: TopologyEdgeLayers::from_directed_edges(&[
            DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 10,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(1),
                from: NodeId(1),
                to: NodeId(2),
                source_way_id: 11,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: EDGE_FLAG_ROUNDABOUT,
            },
            DirectedEdge {
                edge_id: EdgeId(2),
                from: NodeId(0),
                to: NodeId(3),
                source_way_id: 20,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Service,
                surface: SurfaceClass::Paved,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(3),
                from: NodeId(3),
                to: NodeId(2),
                source_way_id: 21,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Service,
                surface: SurfaceClass::Paved,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
        ]),
        turn_restrictions: vec![],
        names: vec![],
        edge_based_topology: Default::default(),
        spatial_index: Some(spatial_index_for_all_nodes(4, 6.0, 53.0, 6.002, 53.001)),
        node_component_ids: vec![],
        edge_component_ids: vec![],
    })
}

pub(super) fn roundabout_entry_penalty_metrics() -> CompiledProfileBundle {
    CompiledProfileBundle {
        schema_version: 3,
        profile_id: "test".to_string(),
        profile_hash: "abc".to_string(),
        mode: TravelMode::Car,
        turn_costs: CompiledTurnCostConfig {
            cost_time_weight: 1.0,
            ..CompiledTurnCostConfig::default()
        },
        components: Vec::new(),
        temporal: Default::default(),
        source_topology_bundle_id: CacheBundleId::new("topology-test"),
        acceleration: None,
        edge_metrics: vec![
            CompiledEdgeMetric {
                edge_id: EdgeId(0),
                travel_time_s: Some(5.0),
                generalized_cost: Some(5.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(1),
                travel_time_s: Some(5.0),
                generalized_cost: Some(5.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(2),
                travel_time_s: Some(6.0),
                generalized_cost: Some(6.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(3),
                travel_time_s: Some(6.0),
                generalized_cost: Some(6.0),
            },
        ],
    }
}

pub(super) fn snap_test_topology() -> TopologyBundle {
    let nodes = (0..10)
        .map(|index| TopologyNode {
            node_id: NodeId(index),
            lon: 6.0 + f64::from(index) * 0.0001,
            lat: 53.0,
            z: 0.0,
        })
        .collect::<Vec<_>>();
    let edges = (0..9)
        .map(|index| DirectedEdge {
            edge_id: EdgeId(index),
            from: NodeId(index),
            to: NodeId(index + 1),
            source_way_id: i64::from(index) + 1,
            length_m: 10,
            ascent_m: 0.0,
            descent_m: 0.0,
            feature_row: netweevil_core::NO_FEATURE_ROW,
            source_direction: 0,
            temporal_rule_id: None,
            duration_s: None,
            road_class: RoadClass::Residential,
            surface: SurfaceClass::Asphalt,
            smoothness: SmoothnessClass::Good,
            access_mask: AccessMask::new(AccessMask::CAR),
            is_toll: false,
            max_speed_kph: None,
            lanes: None,
            name_index: None,
            geometry_offset: 0,
            geometry_len: 0,
            flags: 0,
        })
        .collect::<Vec<_>>();
    with_edge_based_topology(TopologyBundle {
        schema_version: 3,
        source_path: "test".to_string(),
        source_sha256: "abc".to_string(),
        nodes,
        edge_layers: TopologyEdgeLayers::from_directed_edges(&edges),
        turn_restrictions: vec![],
        names: vec![],
        edge_based_topology: Default::default(),
        spatial_index: None,
        node_component_ids: vec![],
        edge_component_ids: vec![],
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
    })
}

pub(super) fn with_edge_based_topology(mut topology: TopologyBundle) -> TopologyBundle {
    topology.edge_based_topology = crate::build_edge_based_topology_fallback(&topology);
    if topology.node_component_ids.len() != topology.nodes.len()
        || topology.edge_component_ids.len() != topology.edge_count()
    {
        let (node_component_ids, edge_component_ids) = weak_components_for_test(&topology);
        topology.node_component_ids = node_component_ids;
        topology.edge_component_ids = edge_component_ids;
    }
    topology
}

pub(super) fn weak_components_for_test(topology: &TopologyBundle) -> (Vec<u32>, Vec<u32>) {
    let node_count = topology.nodes.len();
    let mut parent = (0..node_count as u32).collect::<Vec<_>>();

    for edge_index in 0..topology.edge_count() {
        let edge = topology.routing_edge(edge_index);
        union_test_components(&mut parent, edge.from.0 as usize, edge.to.0 as usize);
    }

    let mut remap = std::collections::BTreeMap::<u32, u32>::new();
    let mut node_component_ids = vec![0_u32; node_count];
    for (node_index, component_slot) in node_component_ids.iter_mut().enumerate() {
        let root = find_test_component_root(&mut parent, node_index);
        let next_component_id = remap.len() as u32;
        *component_slot = *remap.entry(root).or_insert(next_component_id);
    }

    let edge_component_ids = (0..topology.edge_count())
        .map(|edge_index| node_component_ids[topology.routing_edge(edge_index).from.0 as usize])
        .collect();
    (node_component_ids, edge_component_ids)
}

pub(super) fn find_test_component_root(parent: &mut [u32], index: usize) -> u32 {
    let parent_index = parent[index] as usize;
    if parent_index != index {
        parent[index] = find_test_component_root(parent, parent_index);
    }
    parent[index]
}

pub(super) fn union_test_components(parent: &mut [u32], left: usize, right: usize) {
    let left_root = find_test_component_root(parent, left);
    let right_root = find_test_component_root(parent, right);
    if left_root == right_root {
        return;
    }
    if left_root <= right_root {
        parent[right_root as usize] = left_root;
    } else {
        parent[left_root as usize] = right_root;
    }
}

pub(super) fn turn_penalty_metrics() -> CompiledProfileBundle {
    CompiledProfileBundle {
        schema_version: 3,
        profile_id: "test".to_string(),
        profile_hash: "abc".to_string(),
        mode: TravelMode::Car,
        turn_costs: CompiledTurnCostConfig {
            cost_time_weight: 1.0,
            ..CompiledTurnCostConfig::default()
        },
        components: Vec::new(),
        temporal: Default::default(),
        source_topology_bundle_id: CacheBundleId::new("topology-test"),
        acceleration: None,
        edge_metrics: vec![
            CompiledEdgeMetric {
                edge_id: EdgeId(0),
                travel_time_s: Some(5.0),
                generalized_cost: Some(5.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(1),
                travel_time_s: Some(5.0),
                generalized_cost: Some(5.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(2),
                travel_time_s: Some(5.0),
                generalized_cost: Some(5.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(3),
                travel_time_s: Some(10.0),
                generalized_cost: Some(10.0),
            },
            CompiledEdgeMetric {
                edge_id: EdgeId(4),
                travel_time_s: Some(10.0),
                generalized_cost: Some(10.0),
            },
        ],
    }
}

pub(super) fn one_way_dead_end_topology() -> TopologyBundle {
    with_edge_based_topology(TopologyBundle {
        schema_version: 3,
        source_path: "oneway-dead-end".to_string(),
        source_sha256: "abc".to_string(),
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
        nodes: vec![
            TopologyNode {
                node_id: NodeId(0),
                lon: 6.0,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(1),
                lon: 6.001,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(2),
                lon: 6.002,
                lat: 53.0,
                z: 0.0,
            },
        ],
        edge_layers: TopologyEdgeLayers::from_directed_edges(&[
            DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 100,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(1),
                from: NodeId(1),
                to: NodeId(2),
                source_way_id: 101,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
        ]),
        turn_restrictions: vec![],
        names: vec![],
        edge_based_topology: Default::default(),
        spatial_index: Some(spatial_index_for_all_nodes(3, 6.0, 53.0, 6.002, 53.0)),
        node_component_ids: vec![],
        edge_component_ids: vec![],
    })
}

pub(super) fn illegal_turn_only_topology() -> TopologyBundle {
    let mut topology = one_way_dead_end_topology();
    topology.turn_restrictions = vec![TurnRestriction {
        relation_id: 200,
        kind: TurnRestrictionKind::NoTurn,
        edge_path: vec![EdgeId(0), EdgeId(1)],
        mode_mask: AccessMask::new(AccessMask::CAR),
    }];
    topology
}

pub(super) fn dead_node_snap_topology() -> TopologyBundle {
    with_edge_based_topology(TopologyBundle {
        schema_version: 3,
        source_path: "dead-node-snap".to_string(),
        source_sha256: "abc".to_string(),
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
        nodes: vec![
            TopologyNode {
                node_id: NodeId(0),
                lon: 6.0,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(1),
                lon: 6.0001,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(2),
                lon: 6.0002,
                lat: 53.0,
                z: 0.0,
            },
        ],
        edge_layers: TopologyEdgeLayers::from_directed_edges(&[DirectedEdge {
            edge_id: EdgeId(0),
            from: NodeId(1),
            to: NodeId(2),
            source_way_id: 310,
            length_m: 10,
            ascent_m: 0.0,
            descent_m: 0.0,
            feature_row: netweevil_core::NO_FEATURE_ROW,
            source_direction: 0,
            temporal_rule_id: None,
            duration_s: None,
            road_class: RoadClass::Residential,
            surface: SurfaceClass::Asphalt,
            smoothness: SmoothnessClass::Good,
            access_mask: AccessMask::new(AccessMask::CAR),
            is_toll: false,
            max_speed_kph: None,
            lanes: None,
            name_index: None,
            geometry_offset: 0,
            geometry_len: 0,
            flags: 0,
        }]),
        turn_restrictions: vec![],
        names: vec![],
        edge_based_topology: Default::default(),
        spatial_index: Some(spatial_index_for_all_nodes(3, 6.0, 53.0, 6.0002, 53.0)),
        node_component_ids: vec![],
        edge_component_ids: vec![],
    })
}

pub(super) fn ignored_restriction_only_topology() -> TopologyBundle {
    with_edge_based_topology(TopologyBundle {
        schema_version: 3,
        source_path: "ignored-restriction".to_string(),
        source_sha256: "abc".to_string(),
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
        nodes: vec![
            TopologyNode {
                node_id: NodeId(0),
                lon: 6.0,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(1),
                lon: 6.001,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(2),
                lon: 6.002,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(3),
                lon: 6.003,
                lat: 53.0,
                z: 0.0,
            },
        ],
        edge_layers: TopologyEdgeLayers::from_directed_edges(&[
            DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 210,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(1),
                from: NodeId(1),
                to: NodeId(2),
                source_way_id: 211,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(2),
                from: NodeId(2),
                to: NodeId(3),
                source_way_id: 212,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
        ]),
        turn_restrictions: vec![TurnRestriction {
            relation_id: 201,
            kind: TurnRestrictionKind::NoTurn,
            edge_path: vec![EdgeId(0), EdgeId(1), EdgeId(2)],
            mode_mask: AccessMask::new(AccessMask::CAR),
        }],
        names: vec![],
        edge_based_topology: Default::default(),
        spatial_index: Some(spatial_index_for_all_nodes(4, 6.0, 53.0, 6.003, 53.0)),
        node_component_ids: vec![],
        edge_component_ids: vec![],
    })
}

pub(super) fn forbidden_uturn_topology() -> TopologyBundle {
    with_edge_based_topology(TopologyBundle {
        schema_version: 3,
        source_path: "forbidden-uturn".to_string(),
        source_sha256: "abc".to_string(),
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
        nodes: vec![
            TopologyNode {
                node_id: NodeId(0),
                lon: 6.0,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(1),
                lon: 6.001,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(2),
                lon: 6.0,
                lat: 53.001,
                z: 0.0,
            },
        ],
        edge_layers: TopologyEdgeLayers::from_directed_edges(&[
            DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 300,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(1),
                from: NodeId(1),
                to: NodeId(0),
                source_way_id: 301,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(2),
                from: NodeId(0),
                to: NodeId(2),
                source_way_id: 302,
                length_m: 120,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
        ]),
        turn_restrictions: vec![TurnRestriction {
            relation_id: 202,
            kind: TurnRestrictionKind::NoTurn,
            edge_path: vec![EdgeId(0), EdgeId(1)],
            mode_mask: AccessMask::new(AccessMask::CAR),
        }],
        names: vec![],
        edge_based_topology: Default::default(),
        spatial_index: Some(spatial_index_for_all_nodes(3, 6.0, 53.0, 6.001, 53.001)),
        node_component_ids: vec![],
        edge_component_ids: vec![],
    })
}

pub(super) fn ferry_only_subnetwork_topology() -> TopologyBundle {
    with_edge_based_topology(TopologyBundle {
        schema_version: 3,
        source_path: "ferry-only".to_string(),
        source_sha256: "abc".to_string(),
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
        nodes: vec![
            TopologyNode {
                node_id: NodeId(0),
                lon: 6.0,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(1),
                lon: 6.001,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(2),
                lon: 6.01,
                lat: 53.0,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(3),
                lon: 6.011,
                lat: 53.0,
                z: 0.0,
            },
        ],
        edge_layers: TopologyEdgeLayers::from_directed_edges(&[
            DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 400,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: Some(60.0),
                road_class: RoadClass::Ferry,
                surface: SurfaceClass::Unknown,
                smoothness: SmoothnessClass::Unknown,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
            DirectedEdge {
                edge_id: EdgeId(1),
                from: NodeId(2),
                to: NodeId(3),
                source_way_id: 401,
                length_m: 100,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            },
        ]),
        turn_restrictions: vec![],
        names: vec![],
        edge_based_topology: Default::default(),
        spatial_index: Some(spatial_index_for_all_nodes(4, 6.0, 53.0, 6.011, 53.0)),
        node_component_ids: vec![],
        edge_component_ids: vec![],
    })
}

pub(super) fn uniform_metrics(edge_count: usize, travel_time_s: f64) -> CompiledProfileBundle {
    CompiledProfileBundle {
        schema_version: 3,
        profile_id: "test".to_string(),
        profile_hash: "abc".to_string(),
        mode: TravelMode::Car,
        turn_costs: CompiledTurnCostConfig {
            cost_time_weight: 1.0,
            ..CompiledTurnCostConfig::default()
        },
        components: Vec::new(),
        temporal: Default::default(),
        source_topology_bundle_id: CacheBundleId::new("topology-test"),
        acceleration: None,
        edge_metrics: (0..edge_count)
            .map(|edge_index| CompiledEdgeMetric {
                edge_id: EdgeId(edge_index as u32),
                travel_time_s: Some(travel_time_s),
                generalized_cost: Some(travel_time_s),
            })
            .collect(),
    }
}
