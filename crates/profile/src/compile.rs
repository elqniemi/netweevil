use anyhow::Result;
use netweevil_core::{
    CacheBundleId, CompiledAcceleration, CompiledEdgeMetric, CompiledProfileBundle,
    CompiledTurnCostConfig, DatasetAccelerationBundle, RoadClass, TopologyBundle,
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

fn compile_acceleration_with_progress(
    bundle: &DatasetAccelerationBundle,
    source_acceleration_bundle_id: CacheBundleId,
    topology: &TopologyBundle,
    edge_metrics: &[CompiledEdgeMetric],
    profile: &ProfileDocument,
    progress: &mut impl FnMut(ProfileCompileProgress),
) -> CompiledAcceleration {
    let pairwise_forbidden_turns =
        build_pairwise_forbidden_turn_table(topology, mode_access_bit(profile.profile.mode));
    let (upward_path_first_out, upward_path_edges) =
        if bundle.upward_path_first_out.is_empty() && bundle.upward_path_edges.is_empty() {
            (
                (0..=bundle.upward_head.len() as u32).collect::<Vec<_>>(),
                bundle.upward_head.clone(),
            )
        } else {
            (
                bundle.upward_path_first_out.clone(),
                bundle.upward_path_edges.clone(),
            )
        };
    let (downward_path_first_out, downward_path_edges) =
        if bundle.downward_path_first_out.is_empty() && bundle.downward_path_edges.is_empty() {
            (
                (0..=bundle.downward_head.len() as u32).collect::<Vec<_>>(),
                bundle.downward_head.clone(),
            )
        } else {
            (
                bundle.downward_path_first_out.clone(),
                bundle.downward_path_edges.clone(),
            )
        };
    emit_compile_progress(
        progress,
        ProfileCompileStage::CompileAcceleration,
        Some(0.0),
        format!(
            "Customizing acceleration 0% (upward {} arcs, downward {} arcs)",
            bundle.upward_head.len(),
            bundle.downward_head.len()
        ),
    );
    let upward_weight = compile_acceleration_arc_weights_with_progress(
        topology,
        edge_metrics,
        profile,
        &bundle.upward_first_out,
        &upward_path_first_out,
        &upward_path_edges,
        &pairwise_forbidden_turns,
        ProfileCompileStage::CompileAcceleration,
        progress,
        0.0,
        50.0,
        "upward",
    );
    let downward_weight = compile_acceleration_arc_weights_with_progress(
        topology,
        edge_metrics,
        profile,
        &bundle.downward_first_out,
        &downward_path_first_out,
        &downward_path_edges,
        &pairwise_forbidden_turns,
        ProfileCompileStage::CompileAcceleration,
        progress,
        50.0,
        100.0,
        "downward",
    );

    emit_compile_progress(
        progress,
        ProfileCompileStage::CompileAcceleration,
        Some(100.0),
        format!(
            "Customizing acceleration 100% (upward {} arcs, downward {} arcs)",
            bundle.upward_head.len(),
            bundle.downward_head.len()
        ),
    );

    CompiledAcceleration {
        schema_version: 2,
        source_acceleration_bundle_id,
        algorithm: bundle.algorithm.clone(),
        edge_order: Vec::new(),
        edge_rank: Vec::new(),
        upward_first_out: Vec::new(),
        upward_head: Vec::new(),
        upward_weight,
        upward_path_first_out: Vec::new(),
        upward_path_edges: Vec::new(),
        downward_first_out: Vec::new(),
        downward_head: Vec::new(),
        downward_weight,
        downward_path_first_out: Vec::new(),
        downward_path_edges: Vec::new(),
    }
}

fn compile_acceleration_arc_weights_with_progress(
    topology: &TopologyBundle,
    edge_metrics: &[CompiledEdgeMetric],
    profile: &ProfileDocument,
    first_out: &[u32],
    path_first_out: &[u32],
    path_edges: &[u32],
    forbidden_turns: &[Vec<u32>],
    stage: ProfileCompileStage,
    progress: &mut impl FnMut(ProfileCompileProgress),
    percent_start: f64,
    percent_end: f64,
    direction_label: &str,
) -> Vec<f64> {
    let mut weights = vec![f64::INFINITY; first_out.last().copied().unwrap_or_default() as usize];
    let edge_count = topology.edge_count();
    if first_out.len() != edge_count + 1 {
        return weights;
    }
    let mut reporter = PercentReporter::starting_at_zero();
    for edge_index in 0..edge_count {
        let start = first_out[edge_index] as usize;
        let end = first_out[edge_index + 1] as usize;
        for slot in start..end {
            let path_start = path_first_out.get(slot).copied().unwrap_or_default() as usize;
            let path_end = path_first_out.get(slot + 1).copied().unwrap_or_default() as usize;
            if path_end <= path_start {
                continue;
            }
            let mut total_cost = 0.0;
            let mut previous_edge = edge_index;
            let mut valid = true;
            for &next_edge in &path_edges[path_start..path_end] {
                let next_edge = next_edge as usize;
                let Some(next_cost) = edge_metrics[next_edge].generalized_cost else {
                    valid = false;
                    break;
                };
                if forbidden_turns
                    .get(previous_edge)
                    .is_some_and(|blocked| blocked.binary_search(&(next_edge as u32)).is_ok())
                {
                    valid = false;
                    break;
                }
                total_cost += next_cost
                    + transition_turn_penalty_cost(
                        topology,
                        previous_edge,
                        next_edge,
                        profile.turns.left_penalty_s,
                        profile.turns.right_penalty_s,
                        profile.turns.uturn_penalty_s,
                        profile.turns.traffic_signal_penalty_s,
                        profile.turns.roundabout_entry_penalty_s,
                        profile.cost.time_weight,
                    );
                previous_edge = next_edge;
            }
            if valid {
                weights[slot] = total_cost;
            }
        }
        reporter.emit_if_needed(
            (edge_index + 1) as u64,
            edge_count as u64,
            stage,
            progress,
            |percent| {
                let scaled = percent_start + ((percent / 100.0) * (percent_end - percent_start));
                format!(
                    "Customizing acceleration {:.0}% ({}/{}) [{}]",
                    scaled,
                    edge_index + 1,
                    edge_count,
                    direction_label
                )
            },
        );
    }
    weights
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
            schema_version: 1,
            source_topology_bundle_id: CacheBundleId::new("topology-test"),
            algorithm: "edge_based_shortcut_ch_v1".to_string(),
            build_settings: Default::default(),
            stats: Default::default(),
            edge_order: vec![0, 1],
            edge_rank: vec![0, 1],
            upward_first_out: vec![0, 1, 1],
            upward_head: vec![1],
            upward_path_first_out: vec![0, 1],
            upward_path_edges: vec![1],
            downward_first_out: vec![0, 0, 0],
            downward_head: vec![],
            downward_path_first_out: vec![0],
            downward_path_edges: vec![],
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
        assert!(compiled_acceleration.upward_head.is_empty());
        assert_eq!(compiled_acceleration.upward_weight.len(), 1);
        assert!(compiled_acceleration.upward_weight[0].is_finite());
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
