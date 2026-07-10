//! End-to-end engine tests on a small synthetic grid network: dispatch via
//! the real routing engine, deterministic runs, congestion delays, and
//! forced rerouting around a no-access zone added in the scenario.

use std::collections::BTreeMap;
use std::sync::Arc;

use netweevil_core::{
    AccessMask, CacheBundleId, CompiledEdgeMetric, CompiledProfileBundle, DirectedEdge,
    EdgeBasedTopology, EdgeId, NodeId, RoadClass, SmoothnessClass, SurfaceClass, TopologyBundle,
    TopologyNode, TravelMode,
};
use netweevil_query::PreparedRoutingEngine;
use netweevil_simulate::{
    DemandConfig, DepartureConfig, EndpointDistribution, FleetConfig, ScenarioHeader,
    SimulationRunner, SimulationScenario, TimeConfig, WeightedPoint, ZoneConfig, ZoneEffect,
};

const GRID: usize = 7;
const LON_STEP: f64 = 0.0015; // ~100 m at lat 53
const LAT_STEP: f64 = 0.001; // ~111 m
const BASE_LON: f64 = 6.5;
const BASE_LAT: f64 = 53.2;
const CAR_SPEED_MPS: f64 = 13.89; // 50 kph

fn node_index(x: usize, y: usize) -> u32 {
    (y * GRID + x) as u32
}

fn node_lon(x: usize) -> f64 {
    BASE_LON + x as f64 * LON_STEP
}

fn node_lat(y: usize) -> f64 {
    BASE_LAT + y as f64 * LAT_STEP
}

fn grid_engine() -> Arc<PreparedRoutingEngine> {
    grid_engine_for("car_test", TravelMode::Car, CAR_SPEED_MPS)
}

fn grid_engine_for(
    profile_id: &str,
    mode: TravelMode,
    speed_mps: f64,
) -> Arc<PreparedRoutingEngine> {
    let mut nodes = Vec::new();
    for y in 0..GRID {
        for x in 0..GRID {
            nodes.push(TopologyNode {
                node_id: NodeId(node_index(x, y)),
                lon: node_lon(x),
                lat: node_lat(y),
                z: 0.0,
            });
        }
    }

    let mut bundle = TopologyBundle {
        schema_version: 1,
        source_path: "synthetic-grid".to_string(),
        source_sha256: "synthetic".to_string(),
        nodes,
        edge_layers: Default::default(),
        edges: Vec::new(),
        turn_restrictions: Vec::new(),
        names: Vec::new(),
        edge_based_topology: Default::default(),
        spatial_index: None,
        node_component_ids: Vec::new(),
        edge_component_ids: Vec::new(),
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
    };

    let mut edge_count = 0u32;
    let mut push_pair = |bundle: &mut TopologyBundle, a: u32, b: u32, length: u32| {
        for (from, to, source_direction) in [(a, b, 1), (b, a, -1)] {
            bundle.push_edge(DirectedEdge {
                edge_id: EdgeId(edge_count),
                from: NodeId(from),
                to: NodeId(to),
                source_way_id: edge_count as i64,
                length_m: length,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::CAR | AccessMask::BICYCLE),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            });
            edge_count += 1;
        }
    };

    for y in 0..GRID {
        for x in 0..GRID {
            if x + 1 < GRID {
                push_pair(&mut bundle, node_index(x, y), node_index(x + 1, y), 100);
            }
            if y + 1 < GRID {
                push_pair(&mut bundle, node_index(x, y), node_index(x, y + 1), 111);
            }
        }
    }

    bundle.edge_based_topology = build_edge_based_topology(GRID * GRID, &bundle.edges);

    let edge_metrics: Vec<CompiledEdgeMetric> = (0..bundle.edge_count())
        .map(|index| {
            let length = bundle.routing_edge(index).length_m as f64;
            CompiledEdgeMetric {
                edge_id: EdgeId(index as u32),
                travel_time_s: Some(length / speed_mps),
                generalized_cost: Some(length / speed_mps),
            }
        })
        .collect();
    let metrics = CompiledProfileBundle {
        schema_version: 1,
        profile_id: profile_id.to_string(),
        profile_hash: "synthetic".to_string(),
        mode,
        turn_costs: Default::default(),
        components: Vec::new(),
        temporal: Default::default(),
        source_topology_bundle_id: CacheBundleId::new("synthetic"),
        acceleration: None,
        edge_metrics,
    };

    Arc::new(
        PreparedRoutingEngine::new(Arc::new(bundle), Arc::new(metrics), None)
            .expect("synthetic engine builds"),
    )
}

/// Same shape as the ingest crate's builder: node CSR + full transition
/// table (every outgoing edge at the head node).
fn build_edge_based_topology(node_count: usize, edges: &[DirectedEdge]) -> EdgeBasedTopology {
    let mut node_first_out = vec![0u32; node_count + 1];
    for edge in edges {
        node_first_out[edge.from.0 as usize + 1] += 1;
    }
    for index in 1..node_first_out.len() {
        node_first_out[index] += node_first_out[index - 1];
    }
    let mut node_edge_order = vec![0u32; edges.len()];
    let mut cursor = node_first_out.clone();
    for (edge_index, edge) in edges.iter().enumerate() {
        let slot = cursor[edge.from.0 as usize];
        node_edge_order[slot as usize] = edge_index as u32;
        cursor[edge.from.0 as usize] += 1;
    }
    let mut edge_transition_first_out = vec![0u32; edges.len() + 1];
    for (edge_index, edge) in edges.iter().enumerate() {
        let head = edge.to.0 as usize;
        edge_transition_first_out[edge_index + 1] = edge_transition_first_out[edge_index]
            + (node_first_out[head + 1] - node_first_out[head]);
    }
    let mut edge_transition_edges = vec![0u32; edge_transition_first_out[edges.len()] as usize];
    let mut write = edge_transition_first_out[..edges.len()].to_vec();
    for (edge_index, edge) in edges.iter().enumerate() {
        let head = edge.to.0 as usize;
        for &next in
            &node_edge_order[node_first_out[head] as usize..node_first_out[head + 1] as usize]
        {
            edge_transition_edges[write[edge_index] as usize] = next;
            write[edge_index] += 1;
        }
    }
    EdgeBasedTopology {
        node_first_out,
        node_edge_order,
        edge_transition_first_out,
        edge_transition_edges,
    }
}

fn profiles() -> BTreeMap<String, Arc<PreparedRoutingEngine>> {
    let mut map = BTreeMap::new();
    map.insert("car_test".to_string(), grid_engine());
    map
}

fn point(x: usize, y: usize) -> EndpointDistribution {
    EndpointDistribution::Points {
        points: vec![WeightedPoint {
            lon: node_lon(x),
            lat: node_lat(y),
            weight: 1.0,
            label: String::new(),
        }],
    }
}

fn base_scenario(agent_count: u32) -> SimulationScenario {
    SimulationScenario {
        scenario: ScenarioHeader {
            id: "test".to_string(),
            label: String::new(),
            seed: 42,
            description: String::new(),
        },
        time: TimeConfig {
            duration_s: 3600.0,
            tick_s: 1.0,
            stop_when_all_arrived: true,
        },
        fleets: vec![FleetConfig {
            fleet_id: "cars".to_string(),
            profile_id: "car_test".to_string(),
            agent_count,
            demand: DemandConfig {
                origins: point(0, 3),
                destinations: point(GRID - 1, 3),
                od_pairs: Vec::new(),
            },
            departures: DepartureConfig::Instant { at_s: 0.0 },
            behavior: Default::default(),
            speed: Default::default(),
        }],
        zones: Vec::new(),
        traffic: Default::default(),
        output: Default::default(),
    }
}

#[test]
fn single_agent_crosses_the_grid() {
    let scenario = base_scenario(1);
    let runner = SimulationRunner::new(scenario, grid_engine().topology_arc(), profiles())
        .expect("runner builds");
    let result = runner.run().expect("run succeeds");

    assert_eq!(result.summary.agents_total, 1);
    assert_eq!(result.summary.agents_arrived, 1);
    let trajectories = result.trajectories.as_ref().expect("trajectories stored");
    let trajectory = &trajectories[0];
    // Six 100 m edges straight across.
    assert!(
        trajectory.edges.len() >= 6,
        "edges: {}",
        trajectory.edges.len()
    );
    assert!(
        trajectory.distance_m >= 595.0,
        "distance {}",
        trajectory.distance_m
    );
    let arrive = trajectory.arrive_s.expect("arrived");
    // Free flow would be ~43 s at 50 kph; allow generous slack for the
    // speed floor and tick rounding but catch order-of-magnitude breaks.
    assert!(arrive > 20.0 && arrive < 240.0, "arrive_s {arrive}");
    assert!(!result.frames.is_empty());
    assert!(!result.edge_usage.is_empty());
}

#[test]
fn runs_are_deterministic() {
    let run = |seed: u64| {
        let mut scenario = base_scenario(40);
        scenario.scenario.seed = seed;
        let runner = SimulationRunner::new(scenario, grid_engine().topology_arc(), profiles())
            .expect("runner builds");
        let result = runner.run().expect("run succeeds");
        serde_json::to_string(&result.trajectories).expect("serializes")
    };
    let first = run(7);
    let second = run(7);
    assert_eq!(first, second, "same seed must reproduce identical runs");
    let other_seed = run(8);
    assert_ne!(first, other_seed, "different seed should differ");
}

#[test]
fn congestion_delays_a_platoon() {
    // One agent free-flowing vs 80 agents pushed through the same corridor.
    let solo = {
        let runner =
            SimulationRunner::new(base_scenario(1), grid_engine().topology_arc(), profiles())
                .expect("runner builds");
        runner.run().expect("run succeeds")
    };
    let crowd = {
        let runner =
            SimulationRunner::new(base_scenario(80), grid_engine().topology_arc(), profiles())
                .expect("runner builds");
        runner.run().expect("run succeeds")
    };

    assert_eq!(crowd.summary.agents_arrived, crowd.summary.agents_total);
    let solo_time = solo.fleets[0].mean_travel_time_s.expect("solo arrives");
    let crowd_time = crowd.fleets[0].mean_travel_time_s.expect("crowd arrives");
    assert!(
        crowd_time > solo_time * 1.3,
        "expected congestion delay: solo {solo_time:.1}s crowd {crowd_time:.1}s"
    );
    // Busy segments must be visible in the edge statistics.
    assert!(
        crowd
            .edge_bins
            .iter()
            .flat_map(|bin| bin.stats.iter())
            .any(|stat| stat.mean_speed_factor < 0.95),
        "expected at least one congested edge bin"
    );
}

#[test]
fn no_access_zone_forces_reroute_around() {
    let mut scenario = base_scenario(8);
    // Block the middle column except the top row; agents are routed through
    // it at dispatch (zones are invisible to the router) and must reroute.
    let mid_lon = node_lon(GRID / 2);
    scenario.zones.push(ZoneConfig {
        zone_id: "blocked".to_string(),
        label: String::new(),
        polygon: vec![
            [mid_lon - LON_STEP * 0.6, BASE_LAT - LAT_STEP],
            [mid_lon + LON_STEP * 0.6, BASE_LAT - LAT_STEP],
            [
                mid_lon + LON_STEP * 0.6,
                node_lat(GRID - 2) + LAT_STEP * 0.4,
            ],
            [
                mid_lon - LON_STEP * 0.6,
                node_lat(GRID - 2) + LAT_STEP * 0.4,
            ],
        ],
        effect: ZoneEffect::NoAccess { modes: Vec::new() },
    });
    let runner = SimulationRunner::new(scenario, grid_engine().topology_arc(), profiles())
        .expect("runner builds");
    let result = runner.run().expect("run succeeds");

    assert!(
        result.summary.agents_arrived >= 1,
        "agents should find the detour: {:?}",
        result.summary
    );
    assert!(
        result.summary.total_reroutes >= 1,
        "expected forced reroutes, got {:?}",
        result.summary
    );
    // The detour is longer than the straight 600 m line.
    let trajectories = result.trajectories.as_ref().expect("trajectories stored");
    let arrived = trajectories.iter().find(|t| t.arrive_s.is_some()).unwrap();
    assert!(
        arrived.distance_m > 700.0,
        "detour distance {}",
        arrived.distance_m
    );
}

#[test]
fn car_and_bicycle_fleets_run_simultaneously() {
    let mut scenario = base_scenario(10);
    scenario.fleets.push(FleetConfig {
        fleet_id: "bikes".to_string(),
        profile_id: "bike_test".to_string(),
        agent_count: 10,
        demand: DemandConfig {
            origins: point(0, 1),
            destinations: point(GRID - 1, 5),
            od_pairs: Vec::new(),
        },
        departures: DepartureConfig::Instant { at_s: 0.0 },
        behavior: Default::default(),
        speed: Default::default(),
    });
    let mut engines = profiles();
    engines.insert(
        "bike_test".to_string(),
        grid_engine_for("bike_test", TravelMode::Bicycle, 5.5),
    );
    let runner = SimulationRunner::new(scenario, grid_engine().topology_arc(), engines)
        .expect("runner builds");
    let result = runner.run().expect("run succeeds");

    assert_eq!(result.summary.agents_total, 20);
    assert_eq!(result.summary.agents_arrived, 20, "{:?}", result.summary);
    let car = result.fleets.iter().find(|f| f.fleet_id == "cars").unwrap();
    let bike = result
        .fleets
        .iter()
        .find(|f| f.fleet_id == "bikes")
        .unwrap();
    assert_eq!(car.mode, "car");
    assert_eq!(bike.mode, "bicycle");
    // Bikes are paced by their own (slower) profile speeds.
    let car_speed = car.mean_distance_m.unwrap() / car.mean_travel_time_s.unwrap();
    let bike_speed = bike.mean_distance_m.unwrap() / bike.mean_travel_time_s.unwrap();
    assert!(
        bike_speed < car_speed * 0.85,
        "bike {bike_speed:.1} m/s vs car {car_speed:.1} m/s"
    );
    // ...and bikes never exceed their 5.5 m/s free-flow profile speed.
    assert!(bike_speed <= 5.6, "bike speed {bike_speed:.2} m/s");
}
