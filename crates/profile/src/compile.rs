use anyhow::Result;
use netweevil_core::{
    CacheBundleId, CompiledAcceleration, CompiledEdgeMetric, CompiledProfileBundle,
    CompiledTurnCostConfig, DatasetAccelerationBundle, NO_MIDDLE, RoadClass, TopologyBundle,
};

use crate::edge_cost::{
    edge_travel_time_s, ensure_supported_matchers, generalized_cost, is_directionally_excluded,
    is_excluded, mode_access_bit,
};
use crate::progress::{
    PercentReporter, ProfileCompileProgress, ProfileCompileStage, emit_compile_progress,
};
use crate::schema::ProfileDocument;
use crate::turns::transition_turn_penalty_cost;

pub fn compile_profile_bundle(
    profile: &ProfileDocument,
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
) -> Result<CompiledProfileBundle> {
    compile_profile_bundle_with_acceleration(profile, topology, source_topology_bundle_id, None)
}

pub fn compile_profile_bundle_with_acceleration(
    profile: &ProfileDocument,
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
    acceleration: Option<(&DatasetAccelerationBundle, CacheBundleId)>,
) -> Result<CompiledProfileBundle> {
    compile_profile_bundle_with_acceleration_with_progress(
        profile,
        topology,
        source_topology_bundle_id,
        acceleration,
        |_| {},
    )
}

pub fn compile_profile_bundle_with_acceleration_with_progress<F>(
    profile: &ProfileDocument,
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
    acceleration: Option<(&DatasetAccelerationBundle, CacheBundleId)>,
    mut progress: F,
) -> Result<CompiledProfileBundle>
where
    F: FnMut(ProfileCompileProgress),
{
    ensure_supported_matchers(profile)?;
    let profile_hash = profile.fingerprint()?;
    let mode_bit = mode_access_bit(profile.profile.mode);
    let edge_count = topology.edge_count();
    let mut edge_metrics = Vec::with_capacity(edge_count);
    let mut edge_reporter = PercentReporter::starting_at_zero();

    emit_compile_progress(
        &mut progress,
        ProfileCompileStage::CompileEdgeMetrics,
        Some(0.0),
        format!("Compiling edge metrics 0% (0/{})", edge_count),
    );

    for edge_index in 0..edge_count {
        let edge = topology.edge(edge_index);
        let edge_profile = topology.edge_profile(edge_index);
        let metric = if !edge.access_mask.contains(mode_bit)
            || (edge.road_class == RoadClass::Ferry && !profile.ferry.allow)
            || is_directionally_excluded(profile, &edge)
            || is_excluded(profile, &edge, edge_profile.highway)
        {
            CompiledEdgeMetric {
                edge_id: edge.edge_id,
                travel_time_s: None,
                generalized_cost: None,
            }
        } else if let Some(travel_time_s) = edge_travel_time_s(profile, &edge, edge_profile.highway)
        {
            CompiledEdgeMetric {
                edge_id: edge.edge_id,
                travel_time_s: Some(travel_time_s),
                generalized_cost: Some(generalized_cost(profile, &edge, travel_time_s)),
            }
        } else {
            CompiledEdgeMetric {
                edge_id: edge.edge_id,
                travel_time_s: None,
                generalized_cost: None,
            }
        };

        edge_metrics.push(metric);

        edge_reporter.emit_if_needed(
            (edge_index + 1) as u64,
            edge_count as u64,
            ProfileCompileStage::CompileEdgeMetrics,
            &mut progress,
            |percent| {
                format!(
                    "Compiling edge metrics {:.0}% ({}/{})",
                    percent,
                    edge_index + 1,
                    edge_count
                )
            },
        );
    }

    emit_compile_progress(
        &mut progress,
        ProfileCompileStage::CompileEdgeMetrics,
        Some(100.0),
        format!(
            "Compiling edge metrics 100% ({}/{})",
            edge_count, edge_count
        ),
    );

    let compiled_acceleration = acceleration.map(|(bundle, bundle_id)| {
        compile_acceleration_with_progress(
            bundle,
            bundle_id,
            topology,
            &edge_metrics,
            profile,
            &mut progress,
        )
    });

    emit_compile_progress(
        &mut progress,
        ProfileCompileStage::Complete,
        Some(100.0),
        format!(
            "Compiled profile '{}' into {} edge metrics",
            profile.profile.id,
            edge_metrics.len()
        ),
    );

    Ok(CompiledProfileBundle {
        schema_version: 3,
        profile_id: profile.profile.id.clone(),
        profile_hash,
        mode: profile.profile.mode,
        turn_costs: CompiledTurnCostConfig {
            left_penalty_s: profile.turns.left_penalty_s,
            right_penalty_s: profile.turns.right_penalty_s,
            uturn_penalty_s: profile.turns.uturn_penalty_s,
            traffic_signal_penalty_s: profile.turns.traffic_signal_penalty_s,
            roundabout_entry_penalty_s: profile.turns.roundabout_entry_penalty_s,
            cost_time_weight: profile.cost.time_weight,
        },
        source_topology_bundle_id,
        acceleration: compiled_acceleration,
        edge_metrics,
    })
}

/// CCH basic customization.
///
/// Initializes arcs that correspond to real edge-to-edge transitions with the
/// turn-adjusted transition cost, then replays the elimination order: for
/// every state `v` (in contraction order) and every arc pair `x -> v`,
/// `v -> y` with higher-ranked endpoints, relaxes `w(x -> y)` through the
/// lower triangle. Improvements record `v` as the arc's middle for
/// query-time path unpacking. Processing middles in increasing rank
/// guarantees child arc weights are final before any triangle uses them.
fn compile_acceleration_with_progress(
    bundle: &DatasetAccelerationBundle,
    source_acceleration_bundle_id: CacheBundleId,
    topology: &TopologyBundle,
    edge_metrics: &[CompiledEdgeMetric],
    profile: &ProfileDocument,
    progress: &mut impl FnMut(ProfileCompileProgress),
) -> CompiledAcceleration {
    let edge_count = topology.edge_count();
    let upward_len = bundle.upward_first_out.last().copied().unwrap_or_default() as usize;
    let downward_len = bundle
        .downward_first_out
        .last()
        .copied()
        .unwrap_or_default() as usize;
    let mut upward_weight = vec![f64::INFINITY; upward_len];
    let mut upward_middle = vec![NO_MIDDLE; upward_len];
    let mut downward_weight = vec![f64::INFINITY; downward_len];
    let mut downward_middle = vec![NO_MIDDLE; downward_len];

    let result_template = |upward_weight: Vec<f64>,
                           upward_middle: Vec<u32>,
                           downward_weight: Vec<f64>,
                           downward_middle: Vec<u32>| {
        CompiledAcceleration {
            schema_version: 3,
            source_acceleration_bundle_id: source_acceleration_bundle_id.clone(),
            algorithm: bundle.algorithm.clone(),
            upward_weight,
            upward_middle,
            downward_weight,
            downward_middle,
        }
    };

    let transition_topology = &topology.edge_based_topology;
    if bundle.upward_first_out.len() != edge_count + 1
        || bundle.downward_first_out.len() != edge_count + 1
        || bundle.edge_rank.len() != edge_count
        || bundle.edge_order.len() != edge_count
        || transition_topology.edge_transition_first_out.len() != edge_count + 1
    {
        // Shape mismatch: emit empty weights; engine preparation rejects the
        // bundle with a re-import error instead of silently degrading.
        return result_template(
            upward_weight,
            upward_middle,
            downward_weight,
            downward_middle,
        );
    }

    emit_compile_progress(
        progress,
        ProfileCompileStage::CompileAcceleration,
        Some(0.0),
        format!("Customizing CCH 0% (upward {upward_len} arcs, downward {downward_len} arcs)"),
    );

    let pairwise_forbidden_turns =
        build_pairwise_forbidden_turn_table(topology, mode_access_bit(profile.profile.mode));
    let edge_rank = &bundle.edge_rank;

    let find_arc = |first_out: &[u32], heads: &[u32], tail: usize, head: u32| -> Option<usize> {
        let start = first_out[tail] as usize;
        let end = first_out[tail + 1] as usize;
        heads[start..end]
            .binary_search(&head)
            .ok()
            .map(|offset| start + offset)
    };

    // Phase 1: base transition costs, mirroring the exact engine's
    // transition construction (both endpoint edges traversable, pairwise
    // turn not forbidden, turn penalty applied).
    for edge_index in 0..edge_count {
        if edge_metrics[edge_index].generalized_cost.is_none() {
            continue;
        }
        let start = transition_topology.edge_transition_first_out[edge_index] as usize;
        let end = transition_topology.edge_transition_first_out[edge_index + 1] as usize;
        for &next_edge in &transition_topology.edge_transition_edges[start..end] {
            let next_index = next_edge as usize;
            if next_index == edge_index || next_index >= edge_count {
                continue;
            }
            let Some(next_cost) = edge_metrics[next_index].generalized_cost else {
                continue;
            };
            if pairwise_forbidden_turns
                .get(edge_index)
                .is_some_and(|blocked| blocked.binary_search(&next_edge).is_ok())
            {
                continue;
            }
            let weight = next_cost
                + transition_turn_penalty_cost(
                    topology,
                    edge_index,
                    next_index,
                    profile.turns.left_penalty_s,
                    profile.turns.right_penalty_s,
                    profile.turns.uturn_penalty_s,
                    profile.turns.traffic_signal_penalty_s,
                    profile.turns.roundabout_entry_penalty_s,
                    profile.cost.time_weight,
                );
            if edge_rank[edge_index] < edge_rank[next_index] {
                if let Some(slot) = find_arc(
                    &bundle.upward_first_out,
                    &bundle.upward_head,
                    edge_index,
                    next_edge,
                ) {
                    upward_weight[slot] = upward_weight[slot].min(weight);
                }
            } else if let Some(slot) = find_arc(
                &bundle.downward_first_out,
                &bundle.downward_head,
                edge_index,
                next_edge,
            ) {
                downward_weight[slot] = downward_weight[slot].min(weight);
            }
        }
    }

    // Reverse-downward index: downward arcs grouped by head, used to find all
    // `x -> v` with `rank(x) > rank(v)` when processing middle `v`.
    let mut downward_tail = vec![0_u32; downward_len];
    for tail in 0..edge_count {
        let range =
            bundle.downward_first_out[tail] as usize..bundle.downward_first_out[tail + 1] as usize;
        downward_tail[range].fill(tail as u32);
    }
    let mut reverse_downward_first_out = vec![0_u32; edge_count + 1];
    for &head in &bundle.downward_head {
        reverse_downward_first_out[head as usize + 1] += 1;
    }
    for edge_index in 0..edge_count {
        reverse_downward_first_out[edge_index + 1] += reverse_downward_first_out[edge_index];
    }
    let mut reverse_downward_arc = vec![0_u32; downward_len];
    let mut write_positions = reverse_downward_first_out[..edge_count].to_vec();
    for (slot, &head) in bundle.downward_head.iter().enumerate() {
        let write_index = &mut write_positions[head as usize];
        reverse_downward_arc[*write_index as usize] = slot as u32;
        *write_index += 1;
    }

    // Phase 2: triangle relaxation in elimination order.
    let mut reporter = PercentReporter::starting_at_zero();
    for (order_index, &middle) in bundle.edge_order.iter().enumerate() {
        let middle_index = middle as usize;
        let incoming_range = reverse_downward_first_out[middle_index] as usize
            ..reverse_downward_first_out[middle_index + 1] as usize;
        let outgoing_range = bundle.upward_first_out[middle_index] as usize
            ..bundle.upward_first_out[middle_index + 1] as usize;
        for reverse_slot in incoming_range {
            let incoming_arc = reverse_downward_arc[reverse_slot] as usize;
            let incoming_weight = downward_weight[incoming_arc];
            if !incoming_weight.is_finite() {
                continue;
            }
            let tail = downward_tail[incoming_arc] as usize;
            for outgoing_arc in outgoing_range.clone() {
                let head = bundle.upward_head[outgoing_arc];
                if head as usize == tail {
                    continue;
                }
                let outgoing_weight = upward_weight[outgoing_arc];
                if !outgoing_weight.is_finite() {
                    continue;
                }
                let candidate = incoming_weight + outgoing_weight;
                if edge_rank[tail] < edge_rank[head as usize] {
                    if let Some(slot) =
                        find_arc(&bundle.upward_first_out, &bundle.upward_head, tail, head)
                        && candidate < upward_weight[slot]
                    {
                        upward_weight[slot] = candidate;
                        upward_middle[slot] = middle;
                    }
                } else if let Some(slot) = find_arc(
                    &bundle.downward_first_out,
                    &bundle.downward_head,
                    tail,
                    head,
                ) && candidate < downward_weight[slot]
                {
                    downward_weight[slot] = candidate;
                    downward_middle[slot] = middle;
                }
            }
        }
        reporter.emit_if_needed(
            (order_index + 1) as u64,
            edge_count as u64,
            ProfileCompileStage::CompileAcceleration,
            progress,
            |percent| {
                format!(
                    "Customizing CCH {percent:.0}% ({}/{edge_count})",
                    order_index + 1
                )
            },
        );
    }

    emit_compile_progress(
        progress,
        ProfileCompileStage::CompileAcceleration,
        Some(100.0),
        format!("Customizing CCH 100% (upward {upward_len} arcs, downward {downward_len} arcs)"),
    );

    result_template(
        upward_weight,
        upward_middle,
        downward_weight,
        downward_middle,
    )
}

fn build_pairwise_forbidden_turn_table(topology: &TopologyBundle, mode_bit: u16) -> Vec<Vec<u32>> {
    let mut forbidden = vec![Vec::new(); topology.edge_count()];
    for restriction in topology
        .turn_restrictions
        .iter()
        .filter(|restriction| restriction.mode_mask.contains(mode_bit))
    {
        if restriction.edge_path.len() != 2 {
            continue;
        }
        let from_edge = restriction.edge_path[0].0 as usize;
        let to_edge = restriction.edge_path[1].0;
        forbidden[from_edge].push(to_edge);
    }
    for blocked in &mut forbidden {
        blocked.sort_unstable();
        blocked.dedup();
    }
    forbidden
}

#[cfg(test)]
mod tests {
    use super::{compile_profile_bundle, compile_profile_bundle_with_acceleration};
    use crate::DirectionConfig;
    use crate::{
        CostConfig, ExcludeRule, FactorRule, FerryConfig, Objective, PreferencesConfig,
        ProfileDocument, ProfileHeader, ReturnConfig, SpeedRule, TagMatch,
    };
    use netweevil_core::{
        AccessMask, CacheBundleId, DatasetAccelerationBundle, DirectedEdge, EdgeBasedTopology,
        EdgeId, NodeId, RoadClass, SmoothnessClass, SurfaceClass, TopologyBundle, TopologyNode,
        TravelMode,
    };
    use std::collections::BTreeMap;

    #[test]
    fn compiles_edge_metrics_from_topology() {
        let profile = ProfileDocument {
            profile: ProfileHeader {
                id: "car_test".to_string(),
                label: "Car test".to_string(),
                mode: TravelMode::Car,
                defaults_pack: "test".to_string(),
                extends: None,
            },
            cost: CostConfig {
                objective: Objective::Fastest,
                distance_weight: 0.0,
                time_weight: 1.0,
            },
            speed_rules: vec![SpeedRule {
                r#match: tag_match([("highway", "residential")]),
                speed_kph: 40.0,
            }],
            exclude_rules: vec![],
            factors: vec![FactorRule {
                r#match: tag_match([("smoothness", "bad")]),
                speed_factor: 0.5,
            }],
            direction: DirectionConfig::default(),
            turns: Default::default(),
            ferry: FerryConfig::default(),
            preferences: PreferencesConfig::default(),
            returns: ReturnConfig::default(),
        };
        let topology = TopologyBundle {
            schema_version: 1,
            source_path: "test.osm.pbf".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![],
            edge_layers: Default::default(),
            edges: vec![DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 10,
                length_m: 1_000,
                duration_s: None,
                road_class: RoadClass::Residential,
                surface: SurfaceClass::Asphalt,
                smoothness: SmoothnessClass::Bad,
                access_mask: AccessMask::new(AccessMask::CAR),
                is_toll: false,
                max_speed_kph: None,
                lanes: None,
                name_index: None,
                geometry_offset: 0,
                geometry_len: 0,
                flags: 0,
            }],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: EdgeBasedTopology::default(),
            spatial_index: None,
            node_component_ids: vec![0, 0],
            edge_component_ids: vec![0],
        };

        let compiled =
            compile_profile_bundle(&profile, &topology, CacheBundleId::new("topology-test"))
                .expect("compile succeeds");

        assert_eq!(compiled.edge_metrics.len(), 1);
        assert!(compiled.edge_metrics[0].travel_time_s.unwrap() > 170.0);
        assert!(compiled.edge_metrics[0].travel_time_s.unwrap() < 190.0);
        assert_eq!(compiled.source_topology_bundle_id.0, "topology-test");
    }

    #[test]
    fn rejects_unsupported_match_keys() {
        let profile = ProfileDocument {
            profile: ProfileHeader {
                id: "bad_profile".to_string(),
                label: "Bad".to_string(),
                mode: TravelMode::Car,
                defaults_pack: "test".to_string(),
                extends: None,
            },
            cost: Default::default(),
            speed_rules: vec![SpeedRule {
                r#match: tag_match([("lanes", "2")]),
                speed_kph: 50.0,
            }],
            exclude_rules: vec![],
            factors: vec![],
            direction: DirectionConfig::default(),
            turns: Default::default(),
            ferry: FerryConfig::default(),
            preferences: PreferencesConfig::default(),
            returns: ReturnConfig::default(),
        };
        let topology = TopologyBundle {
            schema_version: 1,
            source_path: "test.osm.pbf".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![],
            edge_layers: Default::default(),
            edges: vec![],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: EdgeBasedTopology::default(),
            spatial_index: None,
            node_component_ids: vec![0, 0],
            edge_component_ids: vec![0],
        };

        let error =
            compile_profile_bundle(&profile, &topology, CacheBundleId::new("topology-test"))
                .expect_err("unsupported matcher should fail");
        assert!(error.to_string().contains("unsupported match key 'lanes'"));
    }

    #[test]
    fn excludes_matching_edges_from_compiled_metrics() {
        let profile = ProfileDocument {
            profile: ProfileHeader {
                id: "foot_test".to_string(),
                label: "Foot test".to_string(),
                mode: TravelMode::Foot,
                defaults_pack: "test".to_string(),
                extends: None,
            },
            cost: Default::default(),
            speed_rules: vec![],
            exclude_rules: vec![ExcludeRule {
                r#match: tag_match([("highway", "primary")]),
            }],
            factors: vec![],
            direction: DirectionConfig::default(),
            turns: Default::default(),
            ferry: FerryConfig::default(),
            preferences: PreferencesConfig::default(),
            returns: ReturnConfig::default(),
        };
        let topology = TopologyBundle {
            schema_version: 1,
            source_path: "test.osm.pbf".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![],
            edge_layers: Default::default(),
            edges: vec![
                DirectedEdge {
                    edge_id: EdgeId(0),
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 10,
                    length_m: 100,
                    duration_s: None,
                    road_class: RoadClass::Primary,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::FOOT),
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
                    duration_s: None,
                    road_class: RoadClass::Residential,
                    surface: SurfaceClass::Asphalt,
                    smoothness: SmoothnessClass::Good,
                    access_mask: AccessMask::new(AccessMask::FOOT),
                    is_toll: false,
                    max_speed_kph: None,
                    lanes: None,
                    name_index: None,
                    geometry_offset: 0,
                    geometry_len: 0,
                    flags: 0,
                },
            ],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: EdgeBasedTopology::default(),
            spatial_index: None,
            node_component_ids: vec![0, 0, 0],
            edge_component_ids: vec![0, 0],
        };

        let compiled =
            compile_profile_bundle(&profile, &topology, CacheBundleId::new("topology-test"))
                .expect("compile succeeds");

        assert_eq!(compiled.edge_metrics[0].travel_time_s, None);
        assert_eq!(compiled.edge_metrics[0].generalized_cost, None);
        assert!(compiled.edge_metrics[1].travel_time_s.is_some());
    }

    #[test]
    fn uses_tagged_ferry_duration_when_available() {
        let profile = ferry_profile(true);
        let topology = ferry_topology(Some(900.0));

        let compiled =
            compile_profile_bundle(&profile, &topology, CacheBundleId::new("topology-test"))
                .expect("compile succeeds");

        assert_eq!(compiled.edge_metrics.len(), 1);
        assert_eq!(compiled.edge_metrics[0].travel_time_s, Some(1_200.0));
        assert_eq!(compiled.edge_metrics[0].generalized_cost, Some(1_560.0));
    }

    #[test]
    fn rejects_ferry_without_duration_when_inference_is_disabled() {
        let profile = ferry_profile(false);
        let topology = ferry_topology(None);

        let compiled =
            compile_profile_bundle(&profile, &topology, CacheBundleId::new("topology-test"))
                .expect("compile succeeds");

        assert_eq!(compiled.edge_metrics.len(), 1);
        assert_eq!(compiled.edge_metrics[0].travel_time_s, None);
        assert_eq!(compiled.edge_metrics[0].generalized_cost, None);
    }

    #[test]
    fn stores_turn_costs_in_compiled_profile_bundle() {
        let mut profile = ferry_profile(true);
        profile.turns.left_penalty_s = 7.0;
        profile.turns.right_penalty_s = 3.0;
        profile.turns.uturn_penalty_s = 25.0;
        profile.turns.traffic_signal_penalty_s = 4.0;
        profile.turns.roundabout_entry_penalty_s = 2.0;
        profile.cost.time_weight = 1.5;

        let compiled = compile_profile_bundle(
            &profile,
            &ferry_topology(Some(900.0)),
            CacheBundleId::new("topology-test"),
        )
        .expect("compile succeeds");

        assert_eq!(compiled.turn_costs.left_penalty_s, 7.0);
        assert_eq!(compiled.turn_costs.right_penalty_s, 3.0);
        assert_eq!(compiled.turn_costs.uturn_penalty_s, 25.0);
        assert_eq!(compiled.turn_costs.traffic_signal_penalty_s, 4.0);
        assert_eq!(compiled.turn_costs.roundabout_entry_penalty_s, 2.0);
        assert_eq!(compiled.turn_costs.cost_time_weight, 1.5);
    }

    #[test]
    fn compiles_acceleration_weights_from_dataset_bundle() {
        let profile = ferry_profile(true);
        let topology = TopologyBundle {
            schema_version: 3,
            source_path: "test.osm.pbf".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    osm_node_id: 1,
                    lon: 0.0,
                    lat: 0.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    osm_node_id: 2,
                    lon: 1.0,
                    lat: 0.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    osm_node_id: 3,
                    lon: 2.0,
                    lat: 0.0,
                },
            ],
            edge_layers: Default::default(),
            edges: vec![
                DirectedEdge {
                    edge_id: EdgeId(0),
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 10,
                    length_m: 1_000,
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
                    length_m: 1_000,
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
            ],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: EdgeBasedTopology {
                node_first_out: vec![0, 1, 2, 2],
                node_edge_order: vec![0, 1],
                edge_transition_first_out: vec![0, 1, 1],
                edge_transition_edges: vec![1],
            },
            spatial_index: None,
            node_component_ids: vec![0, 0, 0],
            edge_component_ids: vec![0, 0],
        };
        let acceleration = DatasetAccelerationBundle {
            schema_version: netweevil_core::ACCELERATION_BUNDLE_SCHEMA_VERSION,
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
            algorithm: netweevil_core::CCH_ALGORITHM.to_string(),
            stats: Default::default(),
            edge_order: vec![0, 1],
            edge_rank: vec![0, 1],
            upward_first_out: vec![0, 1, 1],
            upward_head: vec![1],
            downward_first_out: vec![0, 0, 0],
            downward_head: vec![],
        };

        let compiled = compile_profile_bundle_with_acceleration(
            &profile,
            &topology,
            CacheBundleId::new("topology-test"),
            Some((&acceleration, CacheBundleId::new("accel-test"))),
        )
        .expect("compile succeeds");

        let compiled_acceleration = compiled.acceleration.expect("acceleration compiled");
        assert_eq!(
            compiled_acceleration.source_acceleration_bundle_id.0,
            "accel-test"
        );
        assert_eq!(compiled_acceleration.upward_weight.len(), 1);
        assert!(compiled_acceleration.upward_weight[0].is_finite());
        assert_eq!(
            compiled_acceleration.upward_middle,
            vec![netweevil_core::NO_MIDDLE]
        );
        assert!(compiled_acceleration.downward_weight.is_empty());
    }

    fn tag_match<const N: usize>(pairs: [(&str, &str); N]) -> TagMatch {
        let mut tags = BTreeMap::new();
        for (key, value) in pairs {
            tags.insert(key.to_string(), value.to_string());
        }
        TagMatch { tags }
    }

    fn ferry_profile(infer_duration_when_missing: bool) -> ProfileDocument {
        ProfileDocument {
            profile: ProfileHeader {
                id: "ferry_test".to_string(),
                label: "Ferry test".to_string(),
                mode: TravelMode::Car,
                defaults_pack: "test".to_string(),
                extends: None,
            },
            cost: CostConfig {
                objective: Objective::Fastest,
                distance_weight: 0.0,
                time_weight: 1.0,
            },
            speed_rules: vec![],
            exclude_rules: vec![],
            factors: vec![],
            direction: DirectionConfig::default(),
            turns: Default::default(),
            ferry: FerryConfig {
                allow: true,
                use_ferry: 0.2,
                boarding_cost_s: 300.0,
                infer_duration_when_missing,
                default_speed_kph: 20.0,
            },
            preferences: PreferencesConfig::default(),
            returns: ReturnConfig::default(),
        }
    }

    fn ferry_topology(duration_s: Option<f64>) -> TopologyBundle {
        TopologyBundle {
            schema_version: 3,
            source_path: "test.osm.pbf".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![],
            edge_layers: Default::default(),
            edges: vec![DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 10,
                length_m: 1_000,
                duration_s,
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
            }],
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: EdgeBasedTopology::default(),
            spatial_index: None,
            node_component_ids: vec![0, 0],
            edge_component_ids: vec![0],
        }
    }
}
