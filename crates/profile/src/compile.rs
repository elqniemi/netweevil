use netweevil_core::{
    CCH_WEIGHT_INFINITY, CCH_WEIGHT_OVERFLOW, add_cch_weights, encode_cch_weight,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use rayon::prelude::*;

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
                true,
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

/// Customizes the dataset CCH for an already compiled profile's
/// generalized cost: the weights every accelerated route search needs.
///
/// Compiled profile bundles persist only per-edge metrics, so engines call
/// this when they load one. The travel-time and distance weight sets that
/// only service areas use are left empty; build them on demand with
/// [`customize_service_area_metric`]. Returns weights of the wrong length
/// (which engine preparation rejects) when the bundles do not belong
/// together.
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
        false,
        &mut |_| {},
    )
}

/// Service-area metric whose CCH weights can be customized on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAreaMetric {
    /// Seconds including time-based turn penalties.
    TravelTime,
    /// Metres, without turn costs.
    Distance,
}

/// Upward and downward arc weights of one service-area metric, or `None`
/// when the bundles do not belong together or a weight overflows (callers
/// then search with plain Dijkstra).
pub fn customize_service_area_metric(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    acceleration: &DatasetAccelerationBundle,
    metric: ServiceAreaMetric,
) -> Option<(Vec<u32>, Vec<u32>)> {
    let edge_metrics = &metrics.edge_metrics;
    if !customization_shapes_match(acceleration, topology, edge_metrics) {
        return None;
    }
    let context = CustomizationContext::new(
        acceleration,
        topology,
        edge_metrics,
        metrics.mode.access_bit(),
    );
    let weights = match metric {
        ServiceAreaMetric::TravelTime => context.customize(
            topology,
            time_base_weight(topology, edge_metrics, &metrics.turn_costs),
            |_| {},
        ),
        ServiceAreaMetric::Distance => {
            context.customize(topology, distance_base_weight(topology), |_| {})
        }
    };
    representable(weights)
}

fn customization_shapes_match(
    bundle: &DatasetAccelerationBundle,
    topology: &TopologyBundle,
    edge_metrics: &[CompiledEdgeMetric],
) -> bool {
    let edge_count = topology.edge_count();
    bundle.upward_first_out.len() == edge_count + 1
        && bundle.downward_first_out.len() == edge_count + 1
        && bundle.upward_first_out.last().copied().unwrap_or_default() as usize
            == bundle.upward_head.len()
        && bundle
            .downward_first_out
            .last()
            .copied()
            .unwrap_or_default() as usize
            == bundle.downward_head.len()
        && bundle.edge_rank.len() == edge_count
        && bundle.edge_order.len() == edge_count
        && edge_metrics.len() == edge_count
        && topology.edge_based_topology.edge_transition_first_out.len() == edge_count + 1
}

fn turn_penalty(
    topology: &TopologyBundle,
    turns: &CompiledTurnCostConfig,
    edge_index: usize,
    next_index: usize,
    time_weight: f64,
) -> f64 {
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
}

fn cost_base_weight<'a>(
    topology: &'a TopologyBundle,
    edge_metrics: &'a [CompiledEdgeMetric],
    turns: &'a CompiledTurnCostConfig,
) -> impl Fn(usize, usize) -> f64 + Sync + 'a {
    move |edge_index, next_index| {
        edge_metrics[next_index]
            .generalized_cost
            .map_or(f64::INFINITY, |cost| {
                cost + turn_penalty(
                    topology,
                    turns,
                    edge_index,
                    next_index,
                    turns.cost_time_weight,
                )
            })
    }
}

fn time_base_weight<'a>(
    topology: &'a TopologyBundle,
    edge_metrics: &'a [CompiledEdgeMetric],
    turns: &'a CompiledTurnCostConfig,
) -> impl Fn(usize, usize) -> f64 + Sync + 'a {
    move |edge_index, next_index| {
        edge_metrics[next_index]
            .travel_time_s
            .map_or(f64::INFINITY, |time| {
                time + turn_penalty(topology, turns, edge_index, next_index, 1.0)
            })
    }
}

fn distance_base_weight(topology: &TopologyBundle) -> impl Fn(usize, usize) -> f64 + Sync + '_ {
    move |_edge_index, next_index| f64::from(topology.routing_edge(next_index).length_m)
}

/// Per-metric weight sets fall back to Dijkstra when any arc overflows.
fn representable(weights: MetricWeights) -> Option<(Vec<u32>, Vec<u32>)> {
    (!weights.upward.contains(&CCH_WEIGHT_OVERFLOW)
        && !weights.downward.contains(&CCH_WEIGHT_OVERFLOW))
    .then_some((weights.upward, weights.downward))
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
    /// Independent elimination subtrees (CSR over `task_middles`), each
    /// listed in rank order. Every lower neighbour of a middle lies in its
    /// subtree, so subtrees relax concurrently, each one sequentially.
    task_first: Vec<u32>,
    task_middles: Vec<u32>,
    /// Middles above every task subtree, in rank order, relaxed after all
    /// tasks.
    top_middles: Vec<u32>,
}

/// Sentinel for "no vertex" in elimination-tree arrays.
const NO_VERTEX: u32 = u32::MAX;

fn find_arc(first_out: &[u32], heads: &[u32], tail: usize, head: u32) -> Option<usize> {
    let start = first_out[tail] as usize;
    let end = first_out[tail + 1] as usize;
    heads[start..end]
        .binary_search(&head)
        .ok()
        .map(|offset| start + offset)
}

/// Middles with at least this many triangles relax their incoming arcs in
/// parallel; smaller ones run on one thread.
const PARALLEL_MIDDLE_TRIANGLES: usize = 4096;

/// Hierarchy arc of every static, non-forbidden edge transition, aligned
/// with `edge_transition_edges`.
fn transition_arcs(
    bundle: &DatasetAccelerationBundle,
    topology: &TopologyBundle,
    edge_metrics: &[CompiledEdgeMetric],
    mode_bit: u16,
) -> Vec<u32> {
    let edge_count = topology.edge_count();
    let transitions = &topology.edge_based_topology;
    let pairwise_forbidden_turns = build_pairwise_forbidden_turn_table(topology, mode_bit);
    let usable = |edge_index: usize| {
        edge_metrics[edge_index].generalized_cost.is_some()
            && topology.routing_edge(edge_index).flags & EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION
                == 0
    };
    // Base arcs mirror the exact engine's transition construction: both
    // endpoint edges traversable and static, the pairwise turn allowed.
    let mut transition_tail = vec![0_u32; transitions.edge_transition_edges.len()];
    for edge_index in 0..edge_count {
        let range = transitions.edge_transition_first_out[edge_index] as usize
            ..transitions.edge_transition_first_out[edge_index + 1] as usize;
        transition_tail[range].fill(edge_index as u32);
    }
    let transition_arc: Vec<u32> = transition_tail
        .par_iter()
        .zip(transitions.edge_transition_edges.par_iter())
        .map(|(&edge, &next_edge)| {
            let (edge_index, next_index) = (edge as usize, next_edge as usize);
            if next_index == edge_index
                || next_index >= edge_count
                || !usable(edge_index)
                || !usable(next_index)
                || pairwise_forbidden_turns[edge_index]
                    .binary_search(&next_edge)
                    .is_ok()
            {
                return NO_ARC;
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
            arc.unwrap_or(NO_ARC)
        })
        .collect();
    drop(transition_tail);

    transition_arc
}

/// Downward arcs grouped by head: `(downward_tail, first_out, arc)`, used
/// to find all `x -> v` with `rank(x) > rank(v)` when processing `v`.
fn reverse_downward_index(
    bundle: &DatasetAccelerationBundle,
    edge_count: usize,
) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
    let downward_len = bundle.downward_head.len();
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
    drop(write_positions);

    (
        downward_tail,
        reverse_downward_first_out,
        reverse_downward_arc,
    )
}

/// Splits the elimination order into independent subtrees (CSR of middles
/// in rank order) plus the remaining top middles in rank order.
fn elimination_tasks(
    bundle: &DatasetAccelerationBundle,
    edge_count: usize,
) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
    // With one or two workers the tree costs more than it saves: run every
    // middle in rank order instead.
    if rayon::current_num_threads() <= 2 {
        return (vec![0], Vec::new(), bundle.edge_order.clone());
    }
    // Elimination tree (Liu's algorithm with path compression). Middle
    // `v` reads `x -> v` and `v -> y`, whose lower triangles have middles
    // `u` with `u -> v` upward or `v -> u` downward: `v`'s lower
    // neighbours. Linking every lower neighbour's current root under `v`
    // makes each of them a descendant of `v`.
    let mut upward_in_first = vec![0_u32; edge_count + 1];
    for &head in &bundle.upward_head {
        upward_in_first[head as usize + 1] += 1;
    }
    for edge_index in 0..edge_count {
        upward_in_first[edge_index + 1] += upward_in_first[edge_index];
    }
    let mut upward_in_tail = vec![0_u32; bundle.upward_head.len()];
    let mut write_positions = upward_in_first[..edge_count].to_vec();
    for tail in 0..edge_count {
        for &head in &bundle.upward_head
            [bundle.upward_first_out[tail] as usize..bundle.upward_first_out[tail + 1] as usize]
        {
            let write_index = &mut write_positions[head as usize];
            upward_in_tail[*write_index as usize] = tail as u32;
            *write_index += 1;
        }
    }
    drop(write_positions);
    let mut parent = vec![NO_VERTEX; edge_count];
    let mut ancestor = vec![NO_VERTEX; edge_count];
    for &middle in &bundle.edge_order {
        let lower_neighbours = upward_in_tail[upward_in_first[middle as usize] as usize
            ..upward_in_first[middle as usize + 1] as usize]
            .iter()
            .chain(
                &bundle.downward_head[bundle.downward_first_out[middle as usize] as usize
                    ..bundle.downward_first_out[middle as usize + 1] as usize],
            );
        for &neighbour in lower_neighbours {
            let mut root = neighbour;
            while ancestor[root as usize] != NO_VERTEX && ancestor[root as usize] != middle {
                let next = ancestor[root as usize];
                ancestor[root as usize] = middle;
                root = next;
            }
            if ancestor[root as usize] == NO_VERTEX && root != middle {
                ancestor[root as usize] = middle;
                parent[root as usize] = middle;
            }
        }
    }
    drop((ancestor, upward_in_first, upward_in_tail));

    // Cut the tree into subtrees of at most `task_limit` middles.
    let mut subtree_size = vec![1_u32; edge_count];
    for &middle in &bundle.edge_order {
        let parent_index = parent[middle as usize];
        if parent_index != NO_VERTEX {
            subtree_size[parent_index as usize] += subtree_size[middle as usize];
        }
    }
    let task_limit = (edge_count / 256).max(1024) as u32;
    let mut task = vec![NO_VERTEX; edge_count];
    let mut task_count = 0_u32;
    for &middle in bundle.edge_order.iter().rev() {
        let middle_index = middle as usize;
        let parent_index = parent[middle_index];
        task[middle_index] = if subtree_size[middle_index] > task_limit {
            NO_VERTEX
        } else if parent_index != NO_VERTEX && task[parent_index as usize] != NO_VERTEX {
            task[parent_index as usize]
        } else {
            task_count += 1;
            task_count - 1
        };
    }
    let mut task_first = vec![0_u32; task_count as usize + 1];
    let mut top_middles = Vec::new();
    for &middle in &bundle.edge_order {
        match task[middle as usize] {
            NO_VERTEX => top_middles.push(middle),
            task_index => task_first[task_index as usize + 1] += 1,
        }
    }
    for index in 0..task_count as usize {
        task_first[index + 1] += task_first[index];
    }
    let mut task_middles = vec![0_u32; task_first[task_count as usize] as usize];
    let mut write_positions = task_first[..task_count as usize].to_vec();
    for &middle in &bundle.edge_order {
        let task_index = task[middle as usize];
        if task_index != NO_VERTEX {
            let write_index = &mut write_positions[task_index as usize];
            task_middles[*write_index as usize] = middle;
            *write_index += 1;
        }
    }

    (task_first, task_middles, top_middles)
}

impl<'a> CustomizationContext<'a> {
    fn new(
        bundle: &'a DatasetAccelerationBundle,
        topology: &TopologyBundle,
        edge_metrics: &[CompiledEdgeMetric],
        mode_bit: u16,
    ) -> Self {
        let edge_count = topology.edge_count();
        let (transition_arc, (reverse_downward, tasks)) = rayon::join(
            || transition_arcs(bundle, topology, edge_metrics, mode_bit),
            || {
                rayon::join(
                    || reverse_downward_index(bundle, edge_count),
                    || elimination_tasks(bundle, edge_count),
                )
            },
        );
        let (downward_tail, reverse_downward_first_out, reverse_downward_arc) = reverse_downward;
        let (task_first, task_middles, top_middles) = tasks;
        Self {
            bundle,
            transition_arc,
            downward_tail,
            reverse_downward_first_out,
            reverse_downward_arc,
            task_first,
            task_middles,
            top_middles,
        }
    }

    /// Basic customization of one metric. Phase 1 gives every base arc the
    /// minimum `base_weight(edge, next)` of its transitions; phase 2 relaxes
    /// lower triangles `w(x -> y) = min(w(x -> y), w(x -> v) + w(v -> y))`:
    /// independent elimination subtrees concurrently (each in rank order),
    /// then the remaining top middles in rank order. Both child arcs of a
    /// triangle are final before its middle runs, and `min` is
    /// order-independent, so the result is identical to a sequential pass
    /// in rank order.
    /// `on_progress` receives the number of processed middles.
    fn customize(
        &self,
        topology: &TopologyBundle,
        base_weight: impl Fn(usize, usize) -> f64 + Sync,
        mut on_progress: impl FnMut(usize),
    ) -> MetricWeights {
        let bundle = self.bundle;
        let transitions = &topology.edge_based_topology;
        let edge_rank = &bundle.edge_rank;
        let infinite = |len: usize| {
            (0..len)
                .map(|_| AtomicU32::new(CCH_WEIGHT_INFINITY))
                .collect::<Vec<_>>()
        };
        let upward = infinite(bundle.upward_head.len());
        let downward = infinite(bundle.downward_head.len());

        (0..topology.edge_count())
            .into_par_iter()
            .for_each(|edge_index| {
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
                    let slot = if arc & DOWNWARD_ARC != 0 {
                        &downward[(arc & !DOWNWARD_ARC) as usize]
                    } else {
                        &upward[arc as usize]
                    };
                    slot.fetch_min(encode_cch_weight(weight), Ordering::Relaxed);
                }
            });

        // Relaxes every triangle `tail -> middle -> head` of one incoming arc.
        let relax_incoming = |reverse_slot: usize, outgoing: std::ops::Range<usize>| {
            let incoming_arc = self.reverse_downward_arc[reverse_slot] as usize;
            let incoming_weight = downward[incoming_arc].load(Ordering::Relaxed);
            if incoming_weight == CCH_WEIGHT_INFINITY {
                return;
            }
            let tail = self.downward_tail[incoming_arc] as usize;
            for outgoing_arc in outgoing {
                let head = bundle.upward_head[outgoing_arc];
                if head as usize == tail {
                    continue;
                }
                let outgoing_weight = upward[outgoing_arc].load(Ordering::Relaxed);
                if outgoing_weight == CCH_WEIGHT_INFINITY {
                    continue;
                }
                let candidate = add_cch_weights(incoming_weight, outgoing_weight);
                let target = if edge_rank[tail] < edge_rank[head as usize] {
                    find_arc(&bundle.upward_first_out, &bundle.upward_head, tail, head)
                        .map(|slot| &upward[slot])
                } else {
                    find_arc(
                        &bundle.downward_first_out,
                        &bundle.downward_head,
                        tail,
                        head,
                    )
                    .map(|slot| &downward[slot])
                };
                if let Some(target) = target
                    && candidate < target.load(Ordering::Relaxed)
                {
                    target.fetch_min(candidate, Ordering::Relaxed);
                }
            }
        };
        let relax_middle = |middle: u32, parallel: bool| {
            let middle_index = middle as usize;
            let incoming = self.reverse_downward_first_out[middle_index] as usize
                ..self.reverse_downward_first_out[middle_index + 1] as usize;
            let outgoing = bundle.upward_first_out[middle_index] as usize
                ..bundle.upward_first_out[middle_index + 1] as usize;
            if parallel && incoming.len() * outgoing.len() >= PARALLEL_MIDDLE_TRIANGLES {
                // Distinct incoming arcs write distinct tail rows.
                incoming
                    .into_par_iter()
                    .for_each(|reverse_slot| relax_incoming(reverse_slot, outgoing.clone()));
            } else {
                for reverse_slot in incoming {
                    relax_incoming(reverse_slot, outgoing.clone());
                }
            }
        };
        let done = AtomicUsize::new(0);
        self.task_first.par_windows(2).for_each(|task| {
            let middles = &self.task_middles[task[0] as usize..task[1] as usize];
            for &middle in middles {
                relax_middle(middle, false);
            }
            done.fetch_add(middles.len(), Ordering::Relaxed);
        });
        let mut done = done.into_inner();
        on_progress(done);
        let parallel = rayon::current_num_threads() > 1;
        for (index, &middle) in self.top_middles.iter().enumerate() {
            relax_middle(middle, parallel);
            if index % 1024 == 1023 {
                done += 1024;
                on_progress(done);
            }
        }
        on_progress(self.task_middles.len() + self.top_middles.len());
        let into_weights = |weights: Vec<AtomicU32>| {
            weights
                .into_iter()
                .map(AtomicU32::into_inner)
                .collect::<Vec<_>>()
        };
        MetricWeights {
            upward: into_weights(upward),
            downward: into_weights(downward),
        }
    }
}

/// CCH basic customization for generalized cost and, when
/// `service_area_metrics` is set, the travel-time and distance weight sets
/// used by fixed-point one-to-all sweeps on time- and distance-limited
/// isochrones. The runs are independent and execute concurrently; only the
/// generalized-cost run reports progress.
#[allow(clippy::too_many_arguments)]
fn compile_acceleration_with_progress(
    bundle: &DatasetAccelerationBundle,
    source_acceleration_bundle_id: CacheBundleId,
    topology: &TopologyBundle,
    edge_metrics: &[CompiledEdgeMetric],
    turns: &CompiledTurnCostConfig,
    mode_bit: u16,
    service_area_metrics: bool,
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
    if !customization_shapes_match(bundle, topology, edge_metrics) {
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
    let mut reporter = PercentReporter::starting_at_zero();
    let (cost, time, distance) = std::thread::scope(|scope| {
        let time = service_area_metrics.then(|| {
            scope.spawn(|| {
                representable(context.customize(
                    topology,
                    time_base_weight(topology, edge_metrics, turns),
                    |_| {},
                ))
            })
        });
        let distance = service_area_metrics.then(|| {
            scope.spawn(|| {
                representable(context.customize(topology, distance_base_weight(topology), |_| {}))
            })
        });
        let cost = context.customize(
            topology,
            cost_base_weight(topology, edge_metrics, turns),
            |done| {
                reporter.emit_if_needed(
                    done as u64,
                    edge_count as u64,
                    ProfileCompileStage::CompileAcceleration,
                    progress,
                    |percent| format!("Customizing CCH {percent:.0}% ({done}/{edge_count})"),
                );
            },
        );
        let join = |handle: Option<std::thread::ScopedJoinHandle<'_, _>>| {
            handle
                .and_then(|handle| handle.join().expect("metric customization panicked"))
                .unwrap_or_default()
        };
        (cost, join(time), join(distance))
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
mod tests;
