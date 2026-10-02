use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use netweevil_core::{
    AccessMask, CacheBundleId, CompiledAcceleration, CompiledCostComponent, CompiledEdgeMetric,
    CompiledProfileBundle, CompiledTurnCostConfig, DirectedEdge, EDGE_FLAG_ROUNDABOUT,
    EDGE_FLAG_TARGET_TRAFFIC_SIGNAL, EdgeId, NO_FEATURE_ROW, NodeId, NodeSpatialIndex, RoadClass,
    SmoothnessClass, SpatialIndexCell, SurfaceClass, TopologyBounds, TopologyBundle,
    TopologyEdgeLayers, TopologyNode, TravelMode, TurnRestriction, TurnRestrictionKind,
};
use netweevil_profile::{ReturnConfig, ReturnGeometry};

use crate::{
    LabeledPoint, OdPair, OdPairsDocument, PointSetDocument, PreparedRoutingEngine, RouteRequest,
    SnapOptions,
};

// Topology building blocks.

pub(super) fn node(id: u32, lon: f64, lat: f64) -> TopologyNode {
    TopologyNode {
        node_id: NodeId(id),
        lon,
        lat,
        z: 0.0,
    }
}

/// A car-accessible residential asphalt edge without geometry, name or flags.
pub(super) fn edge(id: u32, from: u32, to: u32, source_way_id: i64, length_m: u32) -> DirectedEdge {
    DirectedEdge {
        edge_id: EdgeId(id),
        from: NodeId(from),
        to: NodeId(to),
        source_way_id,
        length_m,
        ascent_m: 0.0,
        descent_m: 0.0,
        feature_row: NO_FEATURE_ROW,
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
    }
}

fn service_edge(id: u32, from: u32, to: u32, source_way_id: i64, length_m: u32) -> DirectedEdge {
    DirectedEdge {
        road_class: RoadClass::Service,
        surface: SurfaceClass::Paved,
        ..edge(id, from, to, source_way_id, length_m)
    }
}

/// Builds a car topology with node ids in `nodes` order, a single spatial
/// index cell covering every node, edge-based transitions and weak components.
pub(super) fn topology(
    source_path: &str,
    nodes: &[(f64, f64)],
    edges: &[DirectedEdge],
) -> TopologyBundle {
    let (lons, lats) = (nodes.iter().map(|n| n.0), nodes.iter().map(|n| n.1));
    let spatial_index = spatial_index_for_all_nodes(
        nodes.len() as u32,
        lons.clone().fold(f64::INFINITY, f64::min),
        lats.clone().fold(f64::INFINITY, f64::min),
        lons.fold(f64::NEG_INFINITY, f64::max),
        lats.fold(f64::NEG_INFINITY, f64::max),
    );
    with_edge_based_topology(TopologyBundle {
        schema_version: 3,
        source_path: source_path.to_string(),
        source_sha256: "abc".to_string(),
        nodes: nodes
            .iter()
            .enumerate()
            .map(|(index, &(lon, lat))| node(index as u32, lon, lat))
            .collect(),
        edge_layers: TopologyEdgeLayers::from_directed_edges(edges),
        turn_restrictions: Vec::new(),
        names: Vec::new(),
        edge_based_topology: Default::default(),
        spatial_index: Some(spatial_index),
        node_component_ids: Vec::new(),
        edge_component_ids: Vec::new(),
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
    })
}

/// Adds a car `no_*` restriction over `edge_path`. Restrictions do not change
/// the topological edge-based graph, so no rebuild is needed.
pub(super) fn with_no_turn(
    mut topology: TopologyBundle,
    relation_id: i64,
    edge_path: &[u32],
) -> TopologyBundle {
    topology.turn_restrictions = vec![TurnRestriction {
        relation_id,
        kind: TurnRestrictionKind::NoTurn,
        edge_path: edge_path.iter().copied().map(EdgeId).collect(),
        mode_mask: AccessMask::new(AccessMask::CAR),
    }];
    topology
}

// Topology fixtures.

/// a -> b -> c along a residential way, plus a slow direct service road a -> c.
pub(super) fn test_topology() -> TopologyBundle {
    topology(
        "test",
        &[(6.0, 53.0), (6.001, 53.0), (6.002, 53.0)],
        &[
            edge(0, 0, 1, 10, 100),
            edge(1, 1, 2, 10, 200),
            service_edge(2, 0, 2, 11, 500),
        ],
    )
}

pub(super) fn service_area_linear_topology() -> TopologyBundle {
    topology(
        "service-area-linear",
        &[(6.0, 53.0), (6.001, 53.0), (6.002, 53.0)],
        &[edge(0, 0, 1, 30, 100), edge(1, 1, 2, 31, 200)],
    )
}

/// A straight a-b-c-d line with service-road bypasses a -> c and c -> d, and
/// the turn 1 -> 2 forbidden.
pub(super) fn restricted_topology() -> TopologyBundle {
    let topology = topology(
        "test",
        &[(6.0, 53.0), (6.001, 53.0), (6.002, 53.0), (6.003, 53.0)],
        &[
            edge(0, 0, 1, 10, 100),
            edge(1, 1, 2, 11, 100),
            edge(2, 2, 3, 12, 100),
            service_edge(3, 0, 2, 20, 200),
            service_edge(4, 2, 3, 21, 200),
        ],
    );
    with_no_turn(topology, 99, &[1, 2])
}

/// `restricted_topology` with the pairwise ban replaced by the sequence 0, 1, 2.
pub(super) fn multi_edge_restricted_topology() -> TopologyBundle {
    with_no_turn(restricted_topology(), 100, &[0, 1, 2])
}

/// A left-then-right zigzag 0, 1, 2 and a service-road detour 3, 4 with a
/// single right turn.
pub(super) fn turn_penalty_topology() -> TopologyBundle {
    topology(
        "test",
        &[
            (6.0, 53.0),
            (6.001, 53.0),
            (6.001, 53.001),
            (6.002, 53.001),
            (6.0, 53.001),
        ],
        &[
            edge(0, 0, 1, 10, 100),
            edge(1, 1, 2, 11, 100),
            edge(2, 2, 3, 12, 100),
            service_edge(3, 0, 4, 20, 100),
            service_edge(4, 4, 3, 21, 200),
        ],
    )
}

pub(super) fn traffic_signal_penalty_topology() -> TopologyBundle {
    let mut topology = turn_penalty_topology();
    topology.set_edge_flags(1, EDGE_FLAG_TARGET_TRAFFIC_SIGNAL);
    topology
}

/// A fast route entering a roundabout on edge 1 and a slower plain detour.
pub(super) fn roundabout_entry_penalty_topology() -> TopologyBundle {
    topology(
        "test",
        &[(6.0, 53.0), (6.001, 53.0), (6.002, 53.001), (6.0, 53.001)],
        &[
            edge(0, 0, 1, 10, 100),
            DirectedEdge {
                flags: EDGE_FLAG_ROUNDABOUT,
                ..edge(1, 1, 2, 11, 100)
            },
            service_edge(2, 0, 3, 20, 100),
            service_edge(3, 3, 2, 21, 100),
        ],
    )
}

/// Ten nodes 0.0001 degrees apart joined by nine 10 m edges, without a
/// spatial index.
pub(super) fn snap_test_topology() -> TopologyBundle {
    let nodes = (0..10)
        .map(|index| (6.0 + f64::from(index) * 0.0001, 53.0))
        .collect::<Vec<_>>();
    let edges = (0..9)
        .map(|index| edge(index, index, index + 1, i64::from(index) + 1, 10))
        .collect::<Vec<_>>();
    let mut topology = topology("test", &nodes, &edges);
    topology.spatial_index = None;
    topology
}

pub(super) fn one_way_dead_end_topology() -> TopologyBundle {
    topology(
        "oneway-dead-end",
        &[(6.0, 53.0), (6.001, 53.0), (6.002, 53.0)],
        &[edge(0, 0, 1, 100, 100), edge(1, 1, 2, 101, 100)],
    )
}

/// The only route from node 0 to node 2 is the forbidden turn 0 -> 1.
pub(super) fn illegal_turn_only_topology() -> TopologyBundle {
    with_no_turn(one_way_dead_end_topology(), 200, &[0, 1])
}

/// Node 0 is the nearest node to the origin but has no incident edge.
pub(super) fn dead_node_snap_topology() -> TopologyBundle {
    topology(
        "dead-node-snap",
        &[(6.0, 53.0), (6.0001, 53.0), (6.0002, 53.0)],
        &[edge(0, 1, 2, 310, 10)],
    )
}

/// The only route from node 0 to node 3 is the forbidden sequence 0, 1, 2.
pub(super) fn ignored_restriction_only_topology() -> TopologyBundle {
    let topology = topology(
        "ignored-restriction",
        &[(6.0, 53.0), (6.001, 53.0), (6.002, 53.0), (6.003, 53.0)],
        &[
            edge(0, 0, 1, 210, 100),
            edge(1, 1, 2, 211, 100),
            edge(2, 2, 3, 212, 100),
        ],
    );
    with_no_turn(topology, 201, &[0, 1, 2])
}

/// Reaching node 2 from edge 0 requires the forbidden u-turn 0 -> 1.
pub(super) fn forbidden_uturn_topology() -> TopologyBundle {
    let topology = topology(
        "forbidden-uturn",
        &[(6.0, 53.0), (6.001, 53.0), (6.0, 53.001)],
        &[
            edge(0, 0, 1, 300, 100),
            edge(1, 1, 0, 301, 100),
            edge(2, 0, 2, 302, 120),
        ],
    );
    with_no_turn(topology, 202, &[0, 1])
}

/// One-way edge 0 -> 1 with a two-edge legal detour 1 -> 2 -> 0 (20 s).
pub(super) fn oneway_with_detour_topology() -> TopologyBundle {
    topology(
        "oneway-detour",
        &[(6.0, 53.0), (6.001, 53.0), (6.0005, 53.0005)],
        &[
            edge(0, 0, 1, 310, 100),
            edge(1, 1, 2, 311, 100),
            edge(2, 2, 0, 312, 100),
        ],
    )
}

/// A ferry-only component next to a separate street component.
pub(super) fn ferry_only_subnetwork_topology() -> TopologyBundle {
    let ferry = DirectedEdge {
        duration_s: Some(60.0),
        road_class: RoadClass::Ferry,
        surface: SurfaceClass::Unknown,
        smoothness: SmoothnessClass::Unknown,
        ..edge(0, 0, 1, 400, 100)
    };
    topology(
        "ferry-only",
        &[(6.0, 53.0), (6.001, 53.0), (6.01, 53.0), (6.011, 53.0)],
        &[ferry, edge(1, 2, 3, 401, 100)],
    )
}

/// Two one-edge components about 600 m apart.
pub(super) fn disconnected_topology() -> TopologyBundle {
    topology(
        "disconnected",
        &[(6.0, 53.0), (6.001, 53.0), (6.01, 53.0), (6.011, 53.0)],
        &[edge(0, 0, 1, 10, 100), edge(1, 2, 3, 11, 100)],
    )
}

/// Two one-edge components separated by a hop of about 33 m.
pub(super) fn hop_disconnected_topology() -> TopologyBundle {
    topology(
        "hop-disconnected",
        &[(6.0, 53.0), (6.0005, 53.0), (6.0010, 53.0), (6.0015, 53.0)],
        &[edge(0, 0, 1, 20, 50), edge(1, 2, 3, 21, 50)],
    )
}

// Metric fixtures.

/// Car metrics whose per-edge travel time equals the generalized cost. Turn
/// penalties do not contribute to the cost (`cost_time_weight = 0`).
pub(super) fn metrics(costs: &[f64]) -> CompiledProfileBundle {
    CompiledProfileBundle {
        schema_version: 3,
        profile_id: "test".to_string(),
        profile_hash: "abc".to_string(),
        mode: TravelMode::Car,
        turn_costs: CompiledTurnCostConfig::default(),
        components: Vec::new(),
        temporal: Default::default(),
        source_topology_bundle_id: CacheBundleId::new("topology-test"),
        acceleration: None,
        edge_metrics: costs
            .iter()
            .enumerate()
            .map(|(index, &cost)| CompiledEdgeMetric {
                edge_id: EdgeId(index as u32),
                travel_time_s: Some(cost),
                generalized_cost: Some(cost),
            })
            .collect(),
    }
}

/// Like [`metrics`], but turn penalties are charged to the generalized cost.
pub(super) fn turn_weighted_metrics(costs: &[f64]) -> CompiledProfileBundle {
    let mut metrics = metrics(costs);
    metrics.turn_costs.cost_time_weight = 1.0;
    metrics
}

pub(super) fn uniform_metrics(edge_count: usize, travel_time_s: f64) -> CompiledProfileBundle {
    turn_weighted_metrics(&vec![travel_time_s; edge_count])
}

/// Sets both travel time and generalized cost; `None` prunes the edge.
pub(super) fn set_edge_cost(metrics: &mut CompiledProfileBundle, edge: usize, cost: Option<f64>) {
    metrics.edge_metrics[edge].travel_time_s = cost;
    metrics.edge_metrics[edge].generalized_cost = cost;
}

/// A zero-weight accounting component without a temporal overlay.
pub(super) fn cost_component(name: &str, edge_values: &[f32]) -> CompiledCostComponent {
    CompiledCostComponent {
        name: name.to_string(),
        weight: 0.0,
        edge_values: edge_values.to_vec(),
        scales_with_travel_time: false,
        overlay_name: None,
        invert_overlay: false,
    }
}

/// a -> b -> c costs 30, the direct a -> c service road costs 100.
pub(super) fn test_metrics() -> CompiledProfileBundle {
    metrics(&[10.0, 20.0, 100.0])
}

pub(super) fn service_area_linear_metrics() -> CompiledProfileBundle {
    metrics(&[10.0, 20.0])
}

pub(super) fn restricted_metrics() -> CompiledProfileBundle {
    metrics(&[10.0, 10.0, 10.0, 20.0, 20.0])
}

/// The zigzag costs 15 and the detour 20 before turn penalties.
pub(super) fn turn_penalty_metrics() -> CompiledProfileBundle {
    turn_weighted_metrics(&[5.0, 5.0, 5.0, 10.0, 10.0])
}

/// The roundabout route costs 10 and the detour 12 before entry penalties.
pub(super) fn roundabout_entry_penalty_metrics() -> CompiledProfileBundle {
    turn_weighted_metrics(&[5.0, 5.0, 6.0, 6.0])
}

pub(super) fn disconnected_metrics() -> CompiledProfileBundle {
    metrics(&[5.0, 5.0])
}

// Request builders.

pub(super) fn point(id: &str, lon: f64, lat: f64) -> LabeledPoint {
    LabeledPoint {
        id: id.to_string(),
        lon,
        lat,
        z: None,
    }
}

pub(super) fn snap_within(max_distance_m: f64) -> SnapOptions {
    SnapOptions {
        max_distance_m,
        ..Default::default()
    }
}

pub(super) fn full_geometry() -> ReturnConfig {
    ReturnConfig {
        geometry: ReturnGeometry::Full,
        ..Default::default()
    }
}

/// A static route request with default options (500 m snap, summary only).
pub(super) fn route_request(
    route_id: &str,
    origin: LabeledPoint,
    destination: LabeledPoint,
) -> RouteRequest {
    RouteRequest {
        route_id: route_id.to_string(),
        origin,
        destination,
        snap: Default::default(),
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: Default::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    }
}

pub(super) fn point_set(points: Vec<LabeledPoint>) -> PointSetDocument {
    PointSetDocument {
        points,
        snap: Default::default(),
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: Default::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    }
}

pub(super) fn od_pair(pair_id: &str, origin: LabeledPoint, destination: LabeledPoint) -> OdPair {
    OdPair {
        pair_id: pair_id.to_string(),
        origin,
        destination,
    }
}

pub(super) fn od_pairs(pairs: Vec<OdPair>) -> OdPairsDocument {
    OdPairsDocument {
        pairs,
        snap: Default::default(),
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: Default::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    }
}

pub(super) fn exact_engine(
    topology: TopologyBundle,
    metrics: CompiledProfileBundle,
) -> PreparedRoutingEngine {
    PreparedRoutingEngine::new(Arc::new(topology), Arc::new(metrics), None)
        .expect("exact engine builds")
}

/// A prepared engine accelerated by the independent [`build_test_cch`].
pub(super) fn cch_engine(
    topology: TopologyBundle,
    metrics: &CompiledProfileBundle,
) -> PreparedRoutingEngine {
    let (bundle, metrics) = build_test_cch(&topology, metrics);
    PreparedRoutingEngine::new(Arc::new(topology), Arc::new(metrics), Some(bundle))
        .expect("accelerated engine builds")
}

pub(super) fn write_temp_file(name: &str, contents: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time works")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("netweevil-query-{unique}-{name}"));
    std::fs::write(&path, contents).expect("fixture written");
    path
}

// Derived topology state.

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

fn weak_components_for_test(topology: &TopologyBundle) -> (Vec<u32>, Vec<u32>) {
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

fn find_test_component_root(parent: &mut [u32], index: usize) -> u32 {
    let parent_index = parent[index] as usize;
    if parent_index != index {
        parent[index] = find_test_component_root(parent, parent_index);
    }
    parent[index]
}

fn union_test_components(parent: &mut [u32], left: usize, right: usize) {
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

fn spatial_index_for_all_nodes(
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
        ACCELERATION_BUNDLE_SCHEMA_VERSION, CCH_ALGORITHM, CCH_WEIGHT_INFINITY,
        DatasetAccelerationBundle, add_cch_weights, encode_cch_weight,
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
    let customize = |base: &dyn Fn(usize, usize) -> f64| -> BTreeMap<(u32, u32), (u32, u32)> {
        let mut arcs = BTreeMap::<(u32, u32), (u32, u32)>::new();
        for &(tail, head) in &arc_set {
            arcs.insert(
                (tail, head),
                (
                    encode_cch_weight(base(tail as usize, head as usize)),
                    u32::MAX,
                ),
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
                    if incoming_weight == CCH_WEIGHT_INFINITY
                        || outgoing_weight == CCH_WEIGHT_INFINITY
                    {
                        continue;
                    }
                    let candidate = add_cch_weights(incoming_weight, outgoing_weight);
                    let entry = arcs
                        .entry((tail, head))
                        .or_insert((CCH_WEIGHT_INFINITY, u32::MAX));
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
    let mut downward_first_out = vec![0_u32; edge_count + 1];
    let mut downward_head = Vec::new();
    let mut downward_weight = Vec::new();
    let mut time_upward_weight = Vec::new();
    let mut time_downward_weight = Vec::new();
    let mut distance_upward_weight = Vec::new();
    let mut distance_downward_weight = Vec::new();
    let metric_weight = |arcs: &BTreeMap<(u32, u32), (u32, u32)>, tail: u32, head: u32| {
        arcs.get(&(tail, head))
            .map(|&(weight, _)| weight)
            .unwrap_or(CCH_WEIGHT_INFINITY)
    };
    for (&(tail, head), &(weight, _)) in &arcs {
        if tail < head {
            upward_first_out[tail as usize + 1] += 1;
            upward_head.push(head);
            upward_weight.push(weight);
            time_upward_weight.push(metric_weight(&time_arcs, tail, head));
            distance_upward_weight.push(metric_weight(&distance_arcs, tail, head));
        } else {
            downward_first_out[tail as usize + 1] += 1;
            downward_head.push(head);
            downward_weight.push(weight);
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
        schema_version: netweevil_core::COMPILED_ACCELERATION_SCHEMA_VERSION,
        source_acceleration_bundle_id: CacheBundleId::new("test-cch"),
        algorithm: CCH_ALGORITHM.to_string(),
        upward_weight,
        downward_weight,
        time_upward_weight,
        time_downward_weight,
        distance_upward_weight,
        distance_downward_weight,
    });
    (bundle, metrics)
}
