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
    progress: F,
) -> Result<CompiledProfileBundle>
where
    F: FnMut(ProfileCompileProgress),
{
    compile_profile(
        profile,
        topology,
        source_topology_bundle_id,
        acceleration,
        true,
        progress,
    )
}

/// Compiles the per-edge metrics that a profile bundle persists. With an
/// acceleration bundle the result carries its acceleration header but no
/// arc weights: engines customize those when they load the bundle (see
/// [`customize_acceleration`]), so compiling for storage skips that work.
pub fn compile_profile_metrics_with_progress<F>(
    profile: &ProfileDocument,
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
    acceleration: Option<(&DatasetAccelerationBundle, CacheBundleId)>,
    progress: F,
) -> Result<CompiledProfileBundle>
where
    F: FnMut(ProfileCompileProgress),
{
    compile_profile(
        profile,
        topology,
        source_topology_bundle_id,
        acceleration,
        false,
        progress,
    )
}

fn compile_profile<F>(
    profile: &ProfileDocument,
    topology: &TopologyBundle,
    source_topology_bundle_id: CacheBundleId,
    acceleration: Option<(&DatasetAccelerationBundle, CacheBundleId)>,
    customize: bool,
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

    let turn_costs = CompiledTurnCostConfig {
        left_penalty_s: profile.turns.left_penalty_s,
        right_penalty_s: profile.turns.right_penalty_s,
        uturn_penalty_s: profile.turns.uturn_penalty_s,
        traffic_signal_penalty_s: profile.turns.traffic_signal_penalty_s,
        roundabout_entry_penalty_s: profile.turns.roundabout_entry_penalty_s,
        cost_time_weight: profile.cost.time_weight,
    };
    let compiled_acceleration = acceleration.map(|(bundle, bundle_id)| {
        if customize {
            compile_acceleration_with_progress(
                bundle,
                bundle_id,
                topology,
                &edge_metrics,
                &turn_costs,
                mode_bit,
                &mut progress,
            )
        } else {
            CompiledAcceleration {
                schema_version: COMPILED_ACCELERATION_SCHEMA_VERSION,
                source_acceleration_bundle_id: bundle_id,
                algorithm: bundle.algorithm.clone(),
                upward_weight: Vec::new(),
                downward_weight: Vec::new(),
                time_upward_weight: Vec::new(),
                time_downward_weight: Vec::new(),
                distance_upward_weight: Vec::new(),
                distance_downward_weight: Vec::new(),
            }
        }
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
        turn_costs,
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

/// Customizes the dataset CCH for an already compiled profile: generalized
/// cost, travel time and distance weights over every hierarchy arc.
///
/// Compiled profile bundles persist only per-edge metrics, so engines call
/// this when they load one. Returns weights of the wrong length (which
/// engine preparation rejects) when the bundles do not belong together.
pub fn customize_acceleration(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    acceleration: &DatasetAccelerationBundle,
    source_acceleration_bundle_id: CacheBundleId,
) -> CompiledAcceleration {
    compile_acceleration_with_progress(
        acceleration,
        source_acceleration_bundle_id,
        topology,
        &metrics.edge_metrics,
        &metrics.turn_costs,
        metrics.mode.access_bit(),
        &mut |_| {},
    )
}

/// Arc-aligned weights of one customized metric.
struct MetricWeights {
    upward: Vec<u32>,
    downward: Vec<u32>,
}

/// Sentinel for a transition without a hierarchy arc of its own.
const NO_ARC: u32 = u32::MAX;
/// Marks a downward arc slot in [`CustomizationContext::transition_arc`].
const DOWNWARD_ARC: u32 = 1 << 31;

/// Metric-independent lookup tables shared by every customization run.
struct CustomizationContext<'a> {
    bundle: &'a DatasetAccelerationBundle,
    /// Hierarchy arc of every static, non-forbidden edge transition,
    /// aligned with `edge_transition_edges`.
    transition_arc: Vec<u32>,
    downward_tail: Vec<u32>,
    reverse_downward_first_out: Vec<u32>,
    reverse_downward_arc: Vec<u32>,
}

fn find_arc(first_out: &[u32], heads: &[u32], tail: usize, head: u32) -> Option<usize> {
    let start = first_out[tail] as usize;
    let end = first_out[tail + 1] as usize;
    heads[start..end]
        .binary_search(&head)
        .ok()
        .map(|offset| start + offset)
}

impl<'a> CustomizationContext<'a> {
    fn new(
        bundle: &'a DatasetAccelerationBundle,
        topology: &TopologyBundle,
        edge_metrics: &[CompiledEdgeMetric],
        mode_bit: u16,
    ) -> Self {
        let edge_count = topology.edge_count();
        let transitions = &topology.edge_based_topology;
        let pairwise_forbidden_turns = build_pairwise_forbidden_turn_table(topology, mode_bit);
        let usable = |edge_index: usize| {
            edge_metrics[edge_index].generalized_cost.is_some()
                && topology.routing_edge(edge_index).flags
                    & EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION
                    == 0
        };
        // Base arcs mirror the exact engine's transition construction: both
        // endpoint edges traversable and static, the pairwise turn allowed.
        let mut transition_arc = vec![NO_ARC; transitions.edge_transition_edges.len()];
        for edge_index in 0..edge_count {
            if !usable(edge_index) {
                continue;
            }
            let start = transitions.edge_transition_first_out[edge_index] as usize;
            let end = transitions.edge_transition_first_out[edge_index + 1] as usize;
            for transition in start..end {
                let next_edge = transitions.edge_transition_edges[transition];
                let next_index = next_edge as usize;
                if next_index == edge_index || next_index >= edge_count || !usable(next_index) {
                    continue;
                }
                if pairwise_forbidden_turns[edge_index]
                    .binary_search(&next_edge)
                    .is_ok()
                {
                    continue;
                }
                let arc = if bundle.edge_rank[edge_index] < bundle.edge_rank[next_index] {
                    find_arc(
                        &bundle.upward_first_out,
                        &bundle.upward_head,
                        edge_index,
                        next_edge,
                    )
                    .map(|slot| slot as u32)
                } else {
                    find_arc(
                        &bundle.downward_first_out,
                        &bundle.downward_head,
                        edge_index,
                        next_edge,
                    )
                    .map(|slot| slot as u32 | DOWNWARD_ARC)
                };
                transition_arc[transition] = arc.unwrap_or(NO_ARC);
            }
        }

        // Reverse-downward index: downward arcs grouped by head, used to
        // find all `x -> v` with `rank(x) > rank(v)` when processing `v`.
        let downward_len = bundle.downward_head.len();
        let mut downward_tail = vec![0_u32; downward_len];
        for tail in 0..edge_count {
            let range = bundle.downward_first_out[tail] as usize
                ..bundle.downward_first_out[tail + 1] as usize;
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
        Self {
            bundle,
            transition_arc,
            downward_tail,
            reverse_downward_first_out,
            reverse_downward_arc,
        }
    }

    /// Basic customization of one metric. Phase 1 gives every base arc the
    /// minimum `base_weight(edge, next)` of its transitions; phase 2 replays
    /// the elimination order relaxing lower triangles
    /// `w(x -> y) = min(w(x -> y), w(x -> v) + w(v -> y))`. Processing
    /// middles in increasing rank makes both child weights final first.
    fn customize(
        &self,
        topology: &TopologyBundle,
        base_weight: impl Fn(usize, usize) -> f64,
        mut on_middle: impl FnMut(usize),
    ) -> MetricWeights {
        let bundle = self.bundle;
        let transitions = &topology.edge_based_topology;
        let edge_rank = &bundle.edge_rank;
        let mut upward = vec![CCH_WEIGHT_INFINITY; bundle.upward_head.len()];
        let mut downward = vec![CCH_WEIGHT_INFINITY; bundle.downward_head.len()];

        for edge_index in 0..topology.edge_count() {
            let start = transitions.edge_transition_first_out[edge_index] as usize;
            let end = transitions.edge_transition_first_out[edge_index + 1] as usize;
            for transition in start..end {
                let arc = self.transition_arc[transition];
                if arc == NO_ARC {
                    continue;
                }
                let weight = base_weight(
                    edge_index,
                    transitions.edge_transition_edges[transition] as usize,
                );
                if !weight.is_finite() {
                    continue;
                }
                let encoded = encode_cch_weight(weight);
                let slot = if arc & DOWNWARD_ARC != 0 {
                    &mut downward[(arc & !DOWNWARD_ARC) as usize]
                } else {
                    &mut upward[arc as usize]
                };
                *slot = (*slot).min(encoded);
            }
        }

        for (order_index, &middle) in bundle.edge_order.iter().enumerate() {
            let middle_index = middle as usize;
            let incoming_range = self.reverse_downward_first_out[middle_index] as usize
                ..self.reverse_downward_first_out[middle_index + 1] as usize;
            let outgoing_range = bundle.upward_first_out[middle_index] as usize
                ..bundle.upward_first_out[middle_index + 1] as usize;
            for reverse_slot in incoming_range {
                let incoming_arc = self.reverse_downward_arc[reverse_slot] as usize;
                let incoming_weight = downward[incoming_arc];
                if incoming_weight == CCH_WEIGHT_INFINITY {
                    continue;
                }
                let tail = self.downward_tail[incoming_arc] as usize;
                for outgoing_arc in outgoing_range.clone() {
                    let head = bundle.upward_head[outgoing_arc];
                    if head as usize == tail {
                        continue;
                    }
                    let outgoing_weight = upward[outgoing_arc];
                    if outgoing_weight == CCH_WEIGHT_INFINITY {
                        continue;
                    }
                    let candidate = add_cch_weights(incoming_weight, outgoing_weight);
                    if edge_rank[tail] < edge_rank[head as usize] {
                        if let Some(slot) =
                            find_arc(&bundle.upward_first_out, &bundle.upward_head, tail, head)
                            && candidate < upward[slot]
                        {
                            upward[slot] = candidate;
                        }
                    } else if let Some(slot) = find_arc(
                        &bundle.downward_first_out,
                        &bundle.downward_head,
                        tail,
                        head,
                    ) && candidate < downward[slot]
                    {
                        downward[slot] = candidate;
                    }
                }
            }
            on_middle(order_index);
        }
        MetricWeights { upward, downward }
    }
}

/// CCH basic customization for generalized cost plus the travel-time and
/// distance weight sets used by fixed-point one-to-all sweeps on time- and
/// distance-limited isochrones. The three runs are independent and execute
/// concurrently; only the generalized-cost run reports progress.
fn compile_acceleration_with_progress(
    bundle: &DatasetAccelerationBundle,
    source_acceleration_bundle_id: CacheBundleId,
    topology: &TopologyBundle,
    edge_metrics: &[CompiledEdgeMetric],
    turns: &CompiledTurnCostConfig,
    mode_bit: u16,
    progress: &mut impl FnMut(ProfileCompileProgress),
) -> CompiledAcceleration {
    let edge_count = topology.edge_count();
    let upward_len = bundle.upward_head.len();
    let downward_len = bundle.downward_head.len();
    let result = |upward_weight: Vec<u32>, downward_weight: Vec<u32>| CompiledAcceleration {
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
        || bundle.upward_first_out.last().copied().unwrap_or_default() as usize != upward_len
        || bundle
            .downward_first_out
            .last()
            .copied()
            .unwrap_or_default() as usize
            != downward_len
        || bundle.edge_rank.len() != edge_count
        || bundle.edge_order.len() != edge_count
        || edge_metrics.len() != edge_count
        || transition_topology.edge_transition_first_out.len() != edge_count + 1
    {
        // Shape mismatch: emit empty weights; engine preparation rejects the
        // bundle with a re-import error instead of silently degrading.
        return result(Vec::new(), Vec::new());
    }

    emit_compile_progress(
        progress,
        ProfileCompileStage::CompileAcceleration,
        Some(0.0),
        format!("Customizing CCH 0% (upward {upward_len} arcs, downward {downward_len} arcs)"),
    );

    let context = CustomizationContext::new(bundle, topology, edge_metrics, mode_bit);
    let turn_penalty = |edge_index: usize, next_index: usize, time_weight: f64| {
        transition_turn_penalty_cost(
            topology,
            edge_index,
            next_index,
            turns.left_penalty_s,
            turns.right_penalty_s,
            turns.uturn_penalty_s,
            turns.traffic_signal_penalty_s,
            turns.roundabout_entry_penalty_s,
            time_weight,
        )
    };
    let cost_weight = |edge_index: usize, next_index: usize| -> f64 {
        edge_metrics[next_index]
            .generalized_cost
            .map_or(f64::INFINITY, |cost| {
                cost + turn_penalty(edge_index, next_index, turns.cost_time_weight)
            })
    };
    let time_weight = |edge_index: usize, next_index: usize| -> f64 {
        edge_metrics[next_index]
            .travel_time_s
            .map_or(f64::INFINITY, |time| {
                time + turn_penalty(edge_index, next_index, 1.0)
            })
    };
    let distance_weight = |_edge_index: usize, next_index: usize| {
        f64::from(topology.routing_edge(next_index).length_m)
    };
    // Per-metric weight sets fall back to Dijkstra when any arc overflows.
    let representable = |weights: MetricWeights| {
        if weights.upward.contains(&CCH_WEIGHT_OVERFLOW)
            || weights.downward.contains(&CCH_WEIGHT_OVERFLOW)
        {
            (Vec::new(), Vec::new())
        } else {
            (weights.upward, weights.downward)
        }
    };

    let mut reporter = PercentReporter::starting_at_zero();
    let (cost, time, distance) = std::thread::scope(|scope| {
        let time = scope.spawn(|| representable(context.customize(topology, time_weight, |_| {})));
        let distance =
            scope.spawn(|| representable(context.customize(topology, distance_weight, |_| {})));
        let cost = context.customize(topology, cost_weight, |order_index| {
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
        });
        (
            cost,
            time.join().expect("time customization panicked"),
            distance.join().expect("distance customization panicked"),
        )
    });

    emit_compile_progress(
        progress,
        ProfileCompileStage::CompileAcceleration,
        Some(100.0),
        format!("Customizing CCH 100% (upward {upward_len} arcs, downward {downward_len} arcs)"),
    );

    CompiledAcceleration {
        time_upward_weight: time.0,
        time_downward_weight: time.1,
        distance_upward_weight: distance.0,
        distance_downward_weight: distance.1,
        ..result(cost.upward, cost.downward)
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
mod tests {
    use super::{compile_profile_bundle, compile_profile_bundle_with_acceleration};
    use crate::DirectionConfig;
    use crate::{
        CostConfig, ExcludeRule, FactorRule, FerryConfig, Objective, PreferencesConfig,
        ProfileDocument, ProfileHeader, ReturnConfig, SpeedRule, TagMatch,
    };
    use netweevil_core::{
        AccessMask, CacheBundleId, DatasetAccelerationBundle, DirectedEdge, EdgeBasedTopology,
        EdgeId, FeatureAttributeColumn, FeatureAttributeColumnData, FeatureAttributeDefinition,
        FeatureAttributeTable, FeatureAttributeType, NodeId, RoadClass, SmoothnessClass,
        SurfaceClass, TopologyBundle, TopologyEdgeLayers, TopologyNode, TravelMode,
    };
    use std::collections::BTreeMap;

    #[test]
    fn sub_metre_edges_keep_physical_cost_without_penalizing_coincident_connectors() {
        let mut profile = ferry_profile(false);
        profile.profile.mode = TravelMode::Foot;
        profile.cost.time_weight = 1.0;
        profile.cost.distance_weight = 2.0;
        let mut topology = ferry_topology(None);
        topology.nodes = vec![
            TopologyNode {
                node_id: NodeId(0),
                lon: 114.18,
                lat: 22.28,
                z: 0.0,
            },
            TopologyNode {
                node_id: NodeId(1),
                lon: 114.180003,
                lat: 22.28,
                z: 0.0,
            },
        ];
        let edge = &mut topology.edge_layers.routing[0];
        edge.length_m = 0;
        topology.edge_layers.profile[0].road_class = RoadClass::Path;
        topology.edge_layers.profile[0].access_mask = AccessMask::new(AccessMask::FOOT);
        let physical_length =
            netweevil_core::geo::distance_3d_meters(114.18, 22.28, 0.0, 114.180003, 22.28, 0.0);
        assert!(physical_length > 0.0 && physical_length < 0.5);
        let metric = |topology: &TopologyBundle| {
            compile_profile_bundle(&profile, topology, CacheBundleId::new("sub-metre"))
                .unwrap()
                .edge_metrics[0]
                .clone()
        };
        let cost = metric(&topology);
        let expected_time = physical_length / (5.0 / 3.6);
        assert!((cost.travel_time_s.unwrap() - expected_time).abs() < 1e-9);
        assert!(
            (cost.generalized_cost.unwrap() - expected_time - 2.0 * physical_length).abs() < 1e-9
        );
        assert!(netweevil_core::encode_cch_weight(cost.generalized_cost.unwrap()) > 0);
        topology.nodes[1].lon = topology.nodes[0].lon;
        topology.nodes[1].z = 0.25;
        topology.edge_layers.routing[0].ascent_m = 0.25;
        assert!(
            metric(&topology).travel_time_s.unwrap() > 0.0,
            "vertical sub-metre edges must cost time"
        );
        topology.nodes[1].z = 0.0;
        topology.edge_layers.routing[0].ascent_m = 0.0;
        assert_eq!(metric(&topology).travel_time_s, Some(0.0));
        assert_eq!(metric(&topology).generalized_cost, Some(0.0));
    }

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
            speeds: Default::default(),
            speed_rules: vec![SpeedRule {
                r#match: tag_match([("highway", "residential")]),
                speed_kph: 40.0,
            }],
            exclude_rules: vec![],
            factors: vec![FactorRule {
                r#match: tag_match([("smoothness", "bad")]),
                speed_factor: 0.5,
            }],
            slope_model: None,
            facilities: Default::default(),
            components: Default::default(),
            temporal: Default::default(),
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
            edge_layers: TopologyEdgeLayers::from_directed_edges(&[DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 10,
                length_m: 1_000,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
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
            }]),
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: EdgeBasedTopology::default(),
            spatial_index: None,
            node_component_ids: vec![0, 0],
            edge_component_ids: vec![0],
            feature_attributes: Default::default(),
            temporal_rule_sets: Vec::new(),
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
    fn compiles_open_attributes_facilities_slope_and_named_components() {
        let profile: ProfileDocument = serde_yaml::from_str(
            r#"
profile:
  id: hk_walk
  label: Hong Kong walking
  mode: foot
  defaults_pack: test
slope_model:
  kind: tobler
facilities:
  attribute: feature_type
  classes:
    staircase:
      kind: stairs
      vertical_speed_mps: 0.25
      burden_per_vertical_m: 2.0
components:
  uncovered_time:
    expression: travel_time * (covered == false)
    weight: 0.5
  ascent:
    expression: ascent
    weight: 1.0
temporal:
  allow_wait: true
  max_wait_s: 900
"#,
        )
        .expect("extended profile parses");
        let topology = TopologyBundle {
            schema_version: 1,
            source_path: "station.gpkg".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![],
            edge_layers: TopologyEdgeLayers::from_directed_edges(&[DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 10,
                length_m: 100,
                ascent_m: 10.0,
                descent_m: 0.0,
                feature_row: 0,
                source_direction: 1,
                temporal_rule_id: None,
                duration_s: None,
                road_class: RoadClass::Path,
                surface: SurfaceClass::Paved,
                smoothness: SmoothnessClass::Good,
                access_mask: AccessMask::new(AccessMask::FOOT),
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
            edge_based_topology: EdgeBasedTopology::default(),
            spatial_index: None,
            node_component_ids: vec![0, 0],
            edge_component_ids: vec![0],
            feature_attributes: FeatureAttributeTable {
                row_count: 1,
                strings: vec!["staircase".to_string()],
                columns: vec![
                    FeatureAttributeColumn {
                        definition: FeatureAttributeDefinition {
                            name: "FacilityType".to_string(),
                            semantic_role: Some("feature_type".to_string()),
                            value_type: FeatureAttributeType::String,
                            domain: Default::default(),
                        },
                        data: FeatureAttributeColumnData::String(vec![Some(0)]),
                    },
                    FeatureAttributeColumn {
                        definition: FeatureAttributeDefinition {
                            name: "Covered".to_string(),
                            semantic_role: Some("covered".to_string()),
                            value_type: FeatureAttributeType::Boolean,
                            domain: Default::default(),
                        },
                        data: FeatureAttributeColumnData::Boolean(vec![Some(false)]),
                    },
                ],
            },
            temporal_rule_sets: Vec::new(),
        };

        let compiled =
            compile_profile_bundle(&profile, &topology, CacheBundleId::new("topology-test"))
                .expect("extended profile compiles");
        let travel_time = compiled.edge_metrics[0].travel_time_s.unwrap();
        assert!(travel_time >= 40.0, "stairs must respect vertical speed");
        let components = compiled
            .components
            .iter()
            .map(|component| (component.name.as_str(), component.edge_values[0]))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(components["ascent"], 10.0);
        assert_eq!(components["stair_burden"], 20.0);
        assert!((f64::from(components["uncovered_time"]) - travel_time).abs() < 1e-4);
        assert!(compiled.temporal.allow_wait);
        assert_eq!(compiled.temporal.max_wait_s, 900.0);
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
            speeds: Default::default(),
            speed_rules: vec![SpeedRule {
                r#match: tag_match([("lanes", "2")]),
                speed_kph: 50.0,
            }],
            exclude_rules: vec![],
            factors: vec![],
            slope_model: None,
            facilities: Default::default(),
            components: Default::default(),
            temporal: Default::default(),
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
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: EdgeBasedTopology::default(),
            spatial_index: None,
            node_component_ids: vec![0, 0],
            edge_component_ids: vec![0],
            feature_attributes: Default::default(),
            temporal_rule_sets: Vec::new(),
        };

        let error =
            compile_profile_bundle(&profile, &topology, CacheBundleId::new("topology-test"))
                .expect_err("unsupported matcher should fail");
        assert!(
            error
                .to_string()
                .contains("attribute 'lanes' which is not present")
        );
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
            speeds: Default::default(),
            speed_rules: vec![],
            exclude_rules: vec![ExcludeRule {
                r#match: tag_match([("highway", "primary")]),
            }],
            factors: vec![],
            slope_model: None,
            facilities: Default::default(),
            components: Default::default(),
            temporal: Default::default(),
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
                    ascent_m: 0.0,
                    descent_m: 0.0,
                    feature_row: netweevil_core::NO_FEATURE_ROW,
                    source_direction: 0,
                    temporal_rule_id: None,
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
            ]),
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: EdgeBasedTopology::default(),
            spatial_index: None,
            node_component_ids: vec![0, 0, 0],
            edge_component_ids: vec![0, 0],
            feature_attributes: Default::default(),
            temporal_rule_sets: Vec::new(),
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
        let mut topology = TopologyBundle {
            schema_version: 3,
            source_path: "test.osm.pbf".to_string(),
            source_sha256: "abc".to_string(),
            nodes: vec![
                TopologyNode {
                    node_id: NodeId(0),
                    lon: 0.0,
                    lat: 0.0,
                    z: 0.0,
                },
                TopologyNode {
                    node_id: NodeId(1),
                    lon: 1.0,
                    lat: 0.0,
                    z: 0.0,
                },
                TopologyNode {
                    node_id: NodeId(2),
                    lon: 2.0,
                    lat: 0.0,
                    z: 0.0,
                },
            ],
            edge_layers: TopologyEdgeLayers::from_directed_edges(&[
                DirectedEdge {
                    edge_id: EdgeId(0),
                    from: NodeId(0),
                    to: NodeId(1),
                    source_way_id: 10,
                    length_m: 1_000,
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
                    length_m: 1_000,
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
            edge_based_topology: EdgeBasedTopology {
                node_first_out: vec![0, 1, 2, 2],
                node_edge_order: vec![0, 1],
                edge_transition_first_out: vec![0, 1, 1],
                edge_transition_edges: vec![1],
            },
            spatial_index: None,
            node_component_ids: vec![0, 0, 0],
            edge_component_ids: vec![0, 0],
            feature_attributes: Default::default(),
            temporal_rule_sets: Vec::new(),
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
        assert!(compiled_acceleration.upward_weight[0] < netweevil_core::CCH_WEIGHT_OVERFLOW);
        assert!(compiled_acceleration.downward_weight.is_empty());
        // Per-metric weight sets align with the same arcs.
        assert_eq!(compiled_acceleration.time_upward_weight.len(), 1);
        assert!(compiled_acceleration.time_upward_weight[0] < netweevil_core::CCH_WEIGHT_OVERFLOW);
        assert_eq!(compiled_acceleration.distance_upward_weight.len(), 1);
        assert!(
            compiled_acceleration.distance_upward_weight[0] < netweevil_core::CCH_WEIGHT_OVERFLOW
        );
        assert!(compiled_acceleration.time_downward_weight.is_empty());
        assert!(compiled_acceleration.distance_downward_weight.is_empty());

        // Storage compiles skip customization; load-time customization of
        // the stored metrics reproduces the compile-time weights exactly.
        let stored = super::compile_profile_metrics_with_progress(
            &profile,
            &topology,
            CacheBundleId::new("topology-test"),
            Some((&acceleration, CacheBundleId::new("accel-test"))),
            |_| {},
        )
        .expect("metric compile succeeds");
        let header = stored.acceleration.as_ref().expect("acceleration header");
        assert!(header.upward_weight.is_empty() && header.time_upward_weight.is_empty());
        let customized = super::customize_acceleration(
            &topology,
            &stored,
            &acceleration,
            header.source_acceleration_bundle_id.clone(),
        );
        assert_eq!(
            customized.upward_weight,
            compiled_acceleration.upward_weight
        );
        assert_eq!(
            customized.time_upward_weight,
            compiled_acceleration.time_upward_weight
        );
        assert_eq!(
            customized.distance_upward_weight,
            compiled_acceleration.distance_upward_weight
        );

        topology.edge_layers.routing[1].flags =
            netweevil_core::EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION;
        let compiled = compile_profile_bundle_with_acceleration(
            &profile,
            &topology,
            CacheBundleId::new("topology-test"),
            Some((&acceleration, CacheBundleId::new("accel-test"))),
        )
        .expect("temporal-only edge still compiles for exact temporal routing");
        assert!(compiled.edge_metrics[1].generalized_cost.is_some());
        let static_acceleration = compiled.acceleration.expect("acceleration compiled");
        assert!(static_acceleration.upward_weight[0] == netweevil_core::CCH_WEIGHT_INFINITY);
        assert!(static_acceleration.time_upward_weight[0] == netweevil_core::CCH_WEIGHT_INFINITY);
        assert!(
            static_acceleration.distance_upward_weight[0] == netweevil_core::CCH_WEIGHT_INFINITY
        );
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
            speeds: Default::default(),
            speed_rules: vec![],
            exclude_rules: vec![],
            factors: vec![],
            slope_model: None,
            facilities: Default::default(),
            components: Default::default(),
            temporal: Default::default(),
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
            edge_layers: TopologyEdgeLayers::from_directed_edges(&[DirectedEdge {
                edge_id: EdgeId(0),
                from: NodeId(0),
                to: NodeId(1),
                source_way_id: 10,
                length_m: 1_000,
                ascent_m: 0.0,
                descent_m: 0.0,
                feature_row: netweevil_core::NO_FEATURE_ROW,
                source_direction: 0,
                temporal_rule_id: None,
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
            }]),
            turn_restrictions: vec![],
            names: vec![],
            edge_based_topology: EdgeBasedTopology::default(),
            spatial_index: None,
            node_component_ids: vec![0, 0],
            edge_component_ids: vec![0],
            feature_attributes: Default::default(),
            temporal_rule_sets: Vec::new(),
        }
    }
}
