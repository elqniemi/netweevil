use netweevil_core::{
    CCH_WEIGHT_INFINITY, CCH_WEIGHT_OVERFLOW, add_cch_weights, encode_cch_weight,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Result;
use netweevil_core::{
    COMPILED_ACCELERATION_SCHEMA_VERSION, COMPILED_PROFILE_BUNDLE_SCHEMA_VERSION, CacheBundleId,
    CompiledAcceleration, CompiledCostComponent, CompiledEdgeMetric, CompiledProfileBundle,
    CompiledTemporalProfile, CompiledTurnCostConfig, DatasetAccelerationBundle,
    EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION, NO_FEATURE_ROW, RoadClass, TopologyBundle,
};

use crate::components::{ParsedCostComponent, parse_component_expression};
use crate::edge_cost::{
    edge_traversal_cost, ensure_supported_matchers, generalized_cost, is_directionally_excluded,
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
    profile.validate()?;
    let parsed_components = profile
        .components
        .iter()
        .map(|(name, component)| {
            Ok((
                name.clone(),
                component.weight,
                parse_component_expression(&component.expression)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let dataset_attributes = topology
        .feature_attributes
        .columns
        .iter()
        .flat_map(|column| {
            std::iter::once(netweevil_core::normalize_attribute_name(
                &column.definition.name,
            ))
            .chain(
                column
                    .definition
                    .semantic_role
                    .as_deref()
                    .map(netweevil_core::normalize_attribute_name),
            )
        })
        .collect::<BTreeSet<_>>();
    ensure_supported_matchers(
        profile,
        &dataset_attributes,
        parsed_components
            .iter()
            .flat_map(|(_, _, parsed)| parsed.attribute_keys()),
    )?;
    let attribute_match_cache = build_attribute_match_cache(profile, &parsed_components, topology);
    let profile_hash = profile.fingerprint()?;
    let mode_bit = mode_access_bit(profile.profile.mode);
    let edge_count = topology.edge_count();
    let mut edge_metrics = Vec::with_capacity(edge_count);
    let mut components = parsed_components
        .iter()
        .map(|(name, weight, parsed)| ComponentWork {
            parsed: Some(parsed.clone()),
            compiled: CompiledCostComponent {
                name: name.clone(),
                weight: *weight,
                edge_values: Vec::with_capacity(edge_count),
                scales_with_travel_time: parsed.scales_with_travel_time(),
                overlay_name: parsed.overlay_name.clone(),
                invert_overlay: parsed.invert_overlay,
            },
        })
        .collect::<Vec<_>>();
    let mut edge_reporter = PercentReporter::starting_at_zero();

    emit_compile_progress(
        &mut progress,
        ProfileCompileStage::CompileEdgeMetrics,
        Some(0.0),
        format!("Compiling edge metrics 0% (0/{})", edge_count),
    );

    for edge_index in 0..edge_count {
        let edge = topology.edge(edge_index);
        let length_m = traversal_length_m(topology, &edge);
        let routing_edge = topology.routing_edge(edge_index);
        let edge_profile = topology.edge_profile(edge_index);
        let attribute_matches = |name: &str, expected: &str| {
            if routing_edge.feature_row == NO_FEATURE_ROW {
                return false;
            }
            attribute_match_cache
                .get(name)
                .and_then(|values| values.get(expected))
                .and_then(|matches| matches.get(routing_edge.feature_row as usize))
                .copied()
                .unwrap_or_else(|| topology.edge_attribute_matches(edge_index, name, expected))
        };
        let facility = profile.facilities.classes.iter().find_map(|(class, cost)| {
            attribute_matches(&profile.facilities.attribute, class).then_some(cost)
        });
        let metric = if !edge.access_mask.contains(mode_bit)
            || (edge.road_class == RoadClass::Ferry && !profile.ferry.allow)
            || is_directionally_excluded(profile, &edge)
            || is_excluded(profile, &edge, edge_profile.highway, &attribute_matches)
        {
            for component in &mut components {
                component.compiled.edge_values.push(0.0);
            }
            CompiledEdgeMetric {
                edge_id: edge.edge_id,
                travel_time_s: None,
                generalized_cost: None,
            }
        } else if let Some(traversal) = edge_traversal_cost(
            profile,
            &edge,
            length_m,
            edge_profile.highway,
            routing_edge.ascent_m,
            routing_edge.descent_m,
            facility,
            &attribute_matches,
        ) {
            let mut values = components
                .iter()
                .map(|component| {
                    component.parsed.as_ref().map_or(0.0, |parsed| {
                        parsed.edge_value(
                            traversal.travel_time_s,
                            length_m,
                            f64::from(routing_edge.ascent_m),
                            f64::from(routing_edge.descent_m),
                            &attribute_matches,
                        )
                    })
                })
                .collect::<Vec<_>>();
            for (name, value) in &traversal.facility_components {
                let component_index = if let Some(index) = components
                    .iter()
                    .position(|component| component.compiled.name == *name)
                {
                    index
                } else {
                    components.push(ComponentWork {
                        parsed: None,
                        compiled: CompiledCostComponent {
                            name: name.clone(),
                            weight: 0.0,
                            edge_values: vec![0.0; edge_index],
                            scales_with_travel_time: false,
                            overlay_name: None,
                            invert_overlay: false,
                        },
                    });
                    values.push(0.0);
                    components.len() - 1
                };
                values[component_index] += *value;
            }
            let mut cost = generalized_cost(profile, &edge, length_m, traversal.travel_time_s);
            for (component, value) in components.iter_mut().zip(values) {
                component.compiled.edge_values.push(value as f32);
                let overlay_multiplier = component
                    .parsed
                    .as_ref()
                    .map_or(1.0, ParsedCostComponent::default_overlay_multiplier);
                cost += component.compiled.weight * value * overlay_multiplier;
            }
            CompiledEdgeMetric {
                edge_id: edge.edge_id,
                travel_time_s: Some(traversal.travel_time_s),
                generalized_cost: Some(cost),
            }
        } else {
            for component in &mut components {
                component.compiled.edge_values.push(0.0);
            }
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
        schema_version: COMPILED_PROFILE_BUNDLE_SCHEMA_VERSION,
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
        components: components
            .into_iter()
            .map(|component| component.compiled)
            .collect(),
        temporal: CompiledTemporalProfile {
            allow_wait: profile.temporal.allow_wait,
            max_wait_s: profile.temporal.max_wait_s,
        },
        source_topology_bundle_id,
        acceleration: compiled_acceleration,
        edge_metrics,
    })
}

struct ComponentWork {
    parsed: Option<ParsedCostComponent>,
    compiled: CompiledCostComponent,
}

type AttributeMatchCache = HashMap<String, HashMap<String, Vec<bool>>>;

/// Precompute the finite set of open-attribute comparisons used by a profile.
///
/// A source feature commonly expands into several directed segment edges. A
/// direct string-key lookup for every rule on every edge would repeatedly scan
/// the attribute registry and decode the same source row millions of times on
/// multilayer datasets. Caching per feature row makes profile compilation
/// linear in directed edges after one compact feature-table pass per distinct
/// predicate.
fn build_attribute_match_cache(
    profile: &ProfileDocument,
    parsed_components: &[(String, f64, ParsedCostComponent)],
    topology: &TopologyBundle,
) -> AttributeMatchCache {
    let mut requested = BTreeMap::<String, BTreeSet<String>>::new();
    let mut add_tags = |tags: &BTreeMap<String, String>| {
        for (name, expected) in tags {
            if topology.feature_attributes.column(name).is_some() {
                requested
                    .entry(name.clone())
                    .or_default()
                    .insert(expected.clone());
            }
        }
    };
    for rule in &profile.speed_rules {
        add_tags(&rule.r#match.tags);
    }
    for rule in &profile.exclude_rules {
        add_tags(&rule.r#match.tags);
    }
    for rule in &profile.factors {
        add_tags(&rule.r#match.tags);
    }
    if topology
        .feature_attributes
        .column(&profile.facilities.attribute)
        .is_some()
    {
        requested
            .entry(profile.facilities.attribute.clone())
            .or_default()
            .extend(profile.facilities.classes.keys().cloned());
    }
    for (_, _, component) in parsed_components {
        for predicate in &component.predicates {
            if topology.feature_attributes.column(&predicate.key).is_some() {
                requested
                    .entry(predicate.key.clone())
                    .or_default()
                    .insert(predicate.expected.clone());
            }
        }
    }

    requested
        .into_iter()
        .map(|(name, expected_values)| {
            let column_index = topology
                .feature_attributes
                .column_index(&name)
                .expect("requested attribute column was checked above");
            let values = expected_values
                .into_iter()
                .map(|expected| {
                    let matches = (0..topology.feature_attributes.row_count)
                        .map(|row| {
                            topology.feature_attributes.value_matches_at(
                                row,
                                column_index,
                                &expected,
                            )
                        })
                        .collect();
                    (expected, matches)
                })
                .collect();
            (name, values)
        })
        .collect()
}

/// CCH basic customization.
///
/// Initializes arcs that correspond to real edge-to-edge transitions with the
/// turn-adjusted transition cost, then replays the elimination order: for
/// every state `v` (in contraction order) and every arc pair `x -> v`,
/// `v -> y` with higher-ranked endpoints, relaxes `w(x -> y)` through the
/// lower triangle. Processing states in increasing rank
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
    let mut upward_weight = vec![CCH_WEIGHT_INFINITY; upward_len];
    let mut downward_weight = vec![CCH_WEIGHT_INFINITY; downward_len];

    let result_template =
        |upward_weight: Vec<u32>, downward_weight: Vec<u32>| CompiledAcceleration {
            schema_version: COMPILED_ACCELERATION_SCHEMA_VERSION,
            source_acceleration_bundle_id: source_acceleration_bundle_id.clone(),
            algorithm: bundle.algorithm.clone(),
            upward_weight,
            downward_weight,
            time_upward_weight: Vec::new(),
            time_downward_weight: Vec::new(),
            distance_upward_weight: Vec::new(),
            distance_downward_weight: Vec::new(),
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
        return result_template(upward_weight, downward_weight);
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
        if edge_metrics[edge_index].generalized_cost.is_none()
            || topology.routing_edge(edge_index).flags & EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION
                != 0
        {
            continue;
        }
        let start = transition_topology.edge_transition_first_out[edge_index] as usize;
        let end = transition_topology.edge_transition_first_out[edge_index + 1] as usize;
        for &next_edge in &transition_topology.edge_transition_edges[start..end] {
            let next_index = next_edge as usize;
            if next_index == edge_index || next_index >= edge_count {
                continue;
            }
            if topology.routing_edge(next_index).flags & EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION
                != 0
            {
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
                    upward_weight[slot] = upward_weight[slot].min(encode_cch_weight(weight));
                }
            } else if let Some(slot) = find_arc(
                &bundle.downward_first_out,
                &bundle.downward_head,
                edge_index,
                next_edge,
            ) {
                downward_weight[slot] = downward_weight[slot].min(encode_cch_weight(weight));
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
            if incoming_weight == CCH_WEIGHT_INFINITY {
                continue;
            }
            let tail = downward_tail[incoming_arc] as usize;
            for outgoing_arc in outgoing_range.clone() {
                let head = bundle.upward_head[outgoing_arc];
                if head as usize == tail {
                    continue;
                }
                let outgoing_weight = upward_weight[outgoing_arc];
                if outgoing_weight == CCH_WEIGHT_INFINITY {
                    continue;
                }
                let candidate = add_cch_weights(incoming_weight, outgoing_weight);
                if edge_rank[tail] < edge_rank[head as usize] {
                    if let Some(slot) =
                        find_arc(&bundle.upward_first_out, &bundle.upward_head, tail, head)
                        && candidate < upward_weight[slot]
                    {
                        upward_weight[slot] = candidate;
                    }
                } else if let Some(slot) = find_arc(
                    &bundle.downward_first_out,
                    &bundle.downward_head,
                    tail,
                    head,
                ) && candidate < downward_weight[slot]
                {
                    downward_weight[slot] = candidate;
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

    // Per-metric weight sets over the same arcs, for fixed-point one-to-all
    // sweeps on time- and distance-limited isochrones. Both runs
    // are independent of the generalized-cost weights, so they customize in
    // parallel.
    let customize_metric =
        |base_weight: &(dyn Fn(usize, usize) -> f64 + Sync)| -> (Vec<u32>, Vec<u32>) {
            let mut metric_upward = vec![CCH_WEIGHT_INFINITY; upward_len];
            let mut metric_downward = vec![CCH_WEIGHT_INFINITY; downward_len];
            for edge_index in 0..edge_count {
                if edge_metrics[edge_index].generalized_cost.is_none()
                    || topology.routing_edge(edge_index).flags
                        & EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION
                        != 0
                {
                    continue;
                }
                let start = transition_topology.edge_transition_first_out[edge_index] as usize;
                let end = transition_topology.edge_transition_first_out[edge_index + 1] as usize;
                for &next_edge in &transition_topology.edge_transition_edges[start..end] {
                    let next_index = next_edge as usize;
                    if next_index == edge_index || next_index >= edge_count {
                        continue;
                    }
                    if edge_metrics[next_index].generalized_cost.is_none()
                        || topology.routing_edge(next_index).flags
                            & EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION
                            != 0
                    {
                        continue;
                    }
                    if pairwise_forbidden_turns
                        .get(edge_index)
                        .is_some_and(|blocked| blocked.binary_search(&next_edge).is_ok())
                    {
                        continue;
                    }
                    let weight = base_weight(edge_index, next_index);
                    if !weight.is_finite() {
                        continue;
                    }
                    if edge_rank[edge_index] < edge_rank[next_index] {
                        if let Some(slot) = find_arc(
                            &bundle.upward_first_out,
                            &bundle.upward_head,
                            edge_index,
                            next_edge,
                        ) {
                            metric_upward[slot] =
                                metric_upward[slot].min(encode_cch_weight(weight));
                        }
                    } else if let Some(slot) = find_arc(
                        &bundle.downward_first_out,
                        &bundle.downward_head,
                        edge_index,
                        next_edge,
                    ) {
                        metric_downward[slot] =
                            metric_downward[slot].min(encode_cch_weight(weight));
                    }
                }
            }
            for &middle in bundle.edge_order.iter() {
                let middle_index = middle as usize;
                let incoming_range = reverse_downward_first_out[middle_index] as usize
                    ..reverse_downward_first_out[middle_index + 1] as usize;
                let outgoing_range = bundle.upward_first_out[middle_index] as usize
                    ..bundle.upward_first_out[middle_index + 1] as usize;
                for reverse_slot in incoming_range {
                    let incoming_arc = reverse_downward_arc[reverse_slot] as usize;
                    let incoming_weight = metric_downward[incoming_arc];
                    if incoming_weight == CCH_WEIGHT_INFINITY {
                        continue;
                    }
                    let tail = downward_tail[incoming_arc] as usize;
                    for outgoing_arc in outgoing_range.clone() {
                        let head = bundle.upward_head[outgoing_arc];
                        if head as usize == tail {
                            continue;
                        }
                        let outgoing_weight = metric_upward[outgoing_arc];
                        if outgoing_weight == CCH_WEIGHT_INFINITY {
                            continue;
                        }
                        let candidate = add_cch_weights(incoming_weight, outgoing_weight);
                        if edge_rank[tail] < edge_rank[head as usize] {
                            if let Some(slot) =
                                find_arc(&bundle.upward_first_out, &bundle.upward_head, tail, head)
                                && candidate < metric_upward[slot]
                            {
                                metric_upward[slot] = candidate;
                            }
                        } else if let Some(slot) = find_arc(
                            &bundle.downward_first_out,
                            &bundle.downward_head,
                            tail,
                            head,
                        ) && candidate < metric_downward[slot]
                        {
                            metric_downward[slot] = candidate;
                        }
                    }
                }
            }
            if metric_upward.contains(&CCH_WEIGHT_OVERFLOW)
                || metric_downward.contains(&CCH_WEIGHT_OVERFLOW)
            {
                (Vec::new(), Vec::new())
            } else {
                (metric_upward, metric_downward)
            }
        };

    let time_weight = |edge_index: usize, next_index: usize| -> f64 {
        let Some(travel_time_s) = edge_metrics[next_index].travel_time_s else {
            return f64::INFINITY;
        };
        travel_time_s
            + transition_turn_penalty_cost(
                topology,
                edge_index,
                next_index,
                profile.turns.left_penalty_s,
                profile.turns.right_penalty_s,
                profile.turns.uturn_penalty_s,
                profile.turns.traffic_signal_penalty_s,
                profile.turns.roundabout_entry_penalty_s,
                1.0,
            )
    };
    let distance_weight = |_edge_index: usize, next_index: usize| -> f64 {
        topology.routing_edge(next_index).length_m as f64
    };
    let (
        (time_upward_weight, time_downward_weight),
        (distance_upward_weight, distance_downward_weight),
    ) = rayon::join(
        || customize_metric(&time_weight),
        || customize_metric(&distance_weight),
    );

    CompiledAcceleration {
        schema_version: COMPILED_ACCELERATION_SCHEMA_VERSION,
        source_acceleration_bundle_id,
        algorithm: bundle.algorithm.clone(),
        upward_weight,
        downward_weight,
        time_upward_weight,
        time_downward_weight,
        distance_upward_weight,
        distance_downward_weight,
    }
}

/// Persisted edge lengths are whole metres. Recover physical length only when
/// rounding erased a real segment, otherwise retain the existing distance
/// contract. Truly coincident graph connectors still have zero length.
fn traversal_length_m(topology: &TopologyBundle, edge: &netweevil_core::DirectedEdge) -> f64 {
    if edge.length_m != 0 {
        return f64::from(edge.length_m);
    }
    let Some((from, to)) = topology
        .nodes
        .get(edge.from.0 as usize)
        .zip(topology.nodes.get(edge.to.0 as usize))
    else {
        return 0.0;
    };
    netweevil_core::geo::distance_3d_meters(from.lon, from.lat, from.z, to.lon, to.lat, to.z)
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
mod tests;
