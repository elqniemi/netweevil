use std::collections::{BTreeMap, BinaryHeap, HashMap};

use anyhow::{Context, Result, bail};
use netweevil_core::{CompiledProfileBundle, TopologyBundle};

use crate::*;
use rayon::prelude::*;

pub fn execute_service_area(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    request: &ServiceAreaRequest,
) -> Result<ServiceAreaResult> {
    validate_execution_inputs(topology, metrics)?;
    let routing_graph = build_routing_graph(topology, metrics)?;
    execute_service_area_with_graph(topology, metrics, &routing_graph, request)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ServiceAreaMetricKind {
    DistanceM,
    TravelTimeS,
}

impl From<ServiceAreaThresholdMetric> for ServiceAreaMetricKind {
    fn from(value: ServiceAreaThresholdMetric) -> Self {
        match value {
            ServiceAreaThresholdMetric::DistanceM => Self::DistanceM,
            ServiceAreaThresholdMetric::TravelTimeS => Self::TravelTimeS,
        }
    }
}

#[derive(Debug, Clone)]
struct ReachableEdgeInterval {
    edge_index: usize,
    start_fraction: f64,
    end_fraction: f64,
    start_cost: f64,
    end_cost: f64,
    midpoint_cost: f64,
    start_components: BTreeMap<String, f64>,
    end_components: BTreeMap<String, f64>,
}

#[derive(Debug, Clone)]
struct ServiceAreaEdgeComponentRange {
    /// Component totals on the winning path immediately before traversing
    /// the reachable span of this edge.
    before: Vec<f64>,
    /// Component totals at the end of that edge span.
    end: Vec<f64>,
}

#[derive(Debug, Clone)]
pub(crate) struct ServiceAreaOriginExpansion {
    pub(crate) origin_id: String,
    pub(crate) representative_origin: SnappedPoint,
    pub(crate) fallback_used: bool,
    pub(crate) origin_hop_distance_m: Option<f64>,
    pub(crate) edge_before_costs: Vec<f64>,
    pub(crate) edge_end_costs: Vec<f64>,
    pub(crate) edge_start_fractions: Vec<f64>,
    /// Sparse component labels for reached edges. The vectors are aligned
    /// with `CompiledProfileBundle::components`.
    edge_component_ranges: HashMap<usize, ServiceAreaEdgeComponentRange>,
    /// Edges reached by the expansion, so downstream consumers iterate the
    /// reachable ball instead of every edge in the dataset.
    pub(crate) reached_edges: Vec<u32>,
    pub(crate) diagnostics: Vec<AnalysisDiagnostic>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct ServiceAreaOriginBand {
    origin_id: String,
    origin_component_id: Option<u32>,
    fallback_used: bool,
    origin_hop_distance_m: Option<f64>,
    threshold_id: Option<String>,
    band_start_limit: Option<f64>,
    threshold_limit: f64,
    threshold_metric: ServiceAreaThresholdMetric,
    segments: Vec<ReachableEdgeInterval>,
}

#[derive(Debug, Clone)]
struct ServiceAreaOriginResolution {
    seed_candidates: Vec<SnappedPoint>,
    representative_origin: SnappedPoint,
    fallback_used: bool,
    origin_hop_distance_m: Option<f64>,
    diagnostics: Vec<AnalysisDiagnostic>,
    warnings: Vec<String>,
}

pub(crate) fn execute_service_area_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    request: &ServiceAreaRequest,
) -> Result<ServiceAreaResult> {
    let temporal_runtime = if request.temporal.is_temporal() {
        if request.temporal.arrive_by.is_some() {
            bail!("time-dependent service areas support departure_time, not arrive_by");
        }
        let departure = request
            .temporal
            .departure_time
            .as_deref()
            .context("a temporal service area requires departure_time")?;
        Some((
            crate::temporal::context_from_request(&request.temporal)?,
            parse_datetime(departure)?,
        ))
    } else {
        None
    };
    if let Some((context, _)) = &temporal_runtime {
        crate::temporal::validate_speed_factor_wait_policy(topology, metrics, context)?;
    }
    let temporal_routing_graph;
    let routing_graph =
        if temporal_runtime.is_some() && !routing_graph.includes_temporal_materialized_directions {
            temporal_routing_graph = build_temporal_routing_graph(topology, metrics)?;
            &temporal_routing_graph
        } else {
            routing_graph
        };
    let search_distance_m = request
        .connectivity
        .max_hop_distance_m
        .unwrap_or(request.snap.max_distance_m)
        .max(request.snap.max_distance_m);
    let thresholds_by_metric = thresholds_for_service_area(request);
    if temporal_runtime.is_some()
        && thresholds_by_metric
            .iter()
            .any(|(metric, _)| !matches!(metric, ServiceAreaMetricKind::TravelTimeS))
    {
        bail!("time-dependent service areas currently require travel_time_s thresholds");
    }
    let mut snap_cache = HashMap::new();
    let search_snap = SnapOptions {
        max_distance_m: search_distance_m,
        ..request.snap.clone()
    };
    let origin_candidate_sets = request
        .origins
        .iter()
        .map(|origin| {
            cached_snap_candidates(
                &mut snap_cache,
                topology,
                routing_graph,
                origin,
                &search_snap,
                true,
            )
        })
        .collect::<Vec<_>>();
    let (origin_refs, unique_origin_candidates) = intern_candidate_sets(origin_candidate_sets);

    // Expansions are independent per unique (origin, metric) pair; build
    // them in parallel up front and share them afterwards instead of cloning
    // the per-edge cost arrays per origin.
    let mut expansion_jobs: Vec<((usize, ServiceAreaMetricKind), (&LabeledPoint, f64))> =
        Vec::new();
    let mut seen_expansions = std::collections::HashSet::new();
    for (origin, origin_ref) in request.origins.iter().zip(&origin_refs) {
        let Ok(origin_set_id) = origin_ref else {
            continue;
        };
        for (metric_kind, thresholds) in &thresholds_by_metric {
            let max_threshold = thresholds
                .last()
                .map(|threshold| threshold.limit)
                .unwrap_or_default();
            if seen_expansions.insert((*origin_set_id, *metric_kind)) {
                expansion_jobs.push(((*origin_set_id, *metric_kind), (origin, max_threshold)));
            }
        }
    }
    let expansion_cache: HashMap<
        (usize, ServiceAreaMetricKind),
        std::result::Result<std::sync::Arc<ServiceAreaOriginExpansion>, AnalysisFailure>,
    > = expansion_jobs
        .into_par_iter()
        .map(|((origin_set_id, metric_kind), (origin, max_threshold))| {
            let expansion = if let Some((context, departure)) = temporal_runtime.as_ref() {
                build_temporal_service_area_expansion(
                    topology,
                    metrics,
                    routing_graph,
                    origin,
                    &unique_origin_candidates[origin_set_id],
                    request.snap.max_distance_m,
                    &request.connectivity,
                    *departure,
                    max_threshold,
                    context,
                )
            } else {
                build_service_area_expansion(
                    topology,
                    metrics,
                    routing_graph,
                    origin,
                    &unique_origin_candidates[origin_set_id],
                    request.snap.max_distance_m,
                    &request.connectivity,
                    metric_kind,
                    max_threshold,
                )
            }
            .map(std::sync::Arc::new)
            .map_err(|error| {
                analysis_failure(&error).cloned().unwrap_or_else(|| {
                    AnalysisFailure::new(
                        error.to_string(),
                        AnalysisOutcome::Unreachable,
                        Vec::new(),
                    )
                })
            });
            ((origin_set_id, metric_kind), expansion)
        })
        .collect();
    let mut diagnostics = Vec::new();
    let mut warnings = execution_warnings(metrics);
    let mut threshold_summaries = Vec::new();
    let mut origin_bands = Vec::new();
    let mut processed_origin_count = 0_usize;
    let mut skipped_origin_count = 0_usize;
    let mut fallback_origin_count = 0_usize;

    for (origin, origin_ref) in request.origins.iter().zip(&origin_refs) {
        let origin_set_id = match origin_ref {
            Ok(origin_set_id) => *origin_set_id,
            Err(error) => {
                if matches!(
                    request.connectivity.disconnected,
                    DisconnectedNetworkMode::IgnoreUnreachable
                ) {
                    skipped_origin_count += 1;
                    diagnostics.extend(error.diagnostics.clone());
                    warnings.push(format!(
                        "Service-area origin '{}' was ignored because it had no legal snap candidate within policy.",
                        origin.id
                    ));
                    continue;
                }
                return Err(anyhow::Error::new(error.clone()))
                    .with_context(|| format!("executing service-area origin '{}'", origin.id));
            }
        };

        let mut origin_processed = false;
        let mut origin_skipped = false;

        for (metric_kind, thresholds) in &thresholds_by_metric {
            let expansion = expansion_cache
                .get(&(origin_set_id, *metric_kind))
                .expect("expansion precomputed for every snapped origin/metric")
                .clone();

            let expansion = match expansion {
                Ok(expansion) => expansion,
                Err(error)
                    if matches!(
                        request.connectivity.disconnected,
                        DisconnectedNetworkMode::IgnoreUnreachable
                    ) =>
                {
                    origin_skipped = true;
                    diagnostics.extend(error.diagnostics);
                    warnings.push(format!(
                        "Service-area origin '{}' was ignored because it could not be reached under the current connectivity policy.",
                        origin.id
                    ));
                    continue;
                }
                Err(error) => {
                    return Err(anyhow::Error::new(error))
                        .with_context(|| format!("executing service-area origin '{}'", origin.id));
                }
            };

            origin_processed = true;
            if expansion.fallback_used {
                fallback_origin_count += 1;
            }
            diagnostics.extend(expansion.diagnostics.clone());
            warnings.extend(expansion.warnings.clone());

            if matches!(request.band_mode, ServiceAreaBandMode::Unbanded) {
                if let Some(threshold) = thresholds.last() {
                    let segments = service_area_intervals_for_threshold(
                        topology,
                        metrics,
                        expansion.as_ref(),
                        *metric_kind,
                        threshold.limit,
                        request.boundary_mode,
                    );

                    if request.returns.per_threshold_summary {
                        let (reachable_network_length_m, reachable_edge_count) =
                            summarize_service_area_segments(
                                topology,
                                &segments,
                                request.returns.attributes,
                            );
                        threshold_summaries.push(ServiceAreaThresholdSummary {
                            origin_id: Some(expansion.origin_id.clone()),
                            band_start_limit: None,
                            threshold_id: None,
                            threshold_limit: threshold.limit,
                            threshold_metric: threshold.metric,
                            fallback_used: expansion.fallback_used,
                            origin_component_id: expansion.representative_origin.component_id,
                            origin_hop_distance_m: expansion.origin_hop_distance_m,
                            reachable_network_length_m,
                            reachable_edge_count,
                        });
                    }

                    origin_bands.push(ServiceAreaOriginBand {
                        origin_id: expansion.origin_id.clone(),
                        origin_component_id: expansion.representative_origin.component_id,
                        fallback_used: expansion.fallback_used,
                        origin_hop_distance_m: expansion.origin_hop_distance_m,
                        threshold_id: None,
                        band_start_limit: None,
                        threshold_limit: threshold.limit,
                        threshold_metric: threshold.metric,
                        segments,
                    });
                }
            } else {
                let mut previous_limit = None;
                for threshold in thresholds {
                    let cumulative = service_area_intervals_for_threshold(
                        topology,
                        metrics,
                        expansion.as_ref(),
                        *metric_kind,
                        threshold.limit,
                        request.boundary_mode,
                    );
                    let segments = if matches!(request.band_mode, ServiceAreaBandMode::Ring) {
                        let previous = previous_limit.map(|limit| {
                            service_area_intervals_for_threshold(
                                topology,
                                metrics,
                                expansion.as_ref(),
                                *metric_kind,
                                limit,
                                request.boundary_mode,
                            )
                        });
                        difference_service_area_intervals(cumulative, previous.unwrap_or_default())
                    } else {
                        cumulative
                    };

                    if request.returns.per_threshold_summary {
                        let (reachable_network_length_m, reachable_edge_count) =
                            summarize_service_area_segments(
                                topology,
                                &segments,
                                request.returns.attributes,
                            );
                        threshold_summaries.push(ServiceAreaThresholdSummary {
                            origin_id: Some(expansion.origin_id.clone()),
                            band_start_limit: previous_limit,
                            threshold_id: threshold.id.clone(),
                            threshold_limit: threshold.limit,
                            threshold_metric: threshold.metric,
                            fallback_used: expansion.fallback_used,
                            origin_component_id: expansion.representative_origin.component_id,
                            origin_hop_distance_m: expansion.origin_hop_distance_m,
                            reachable_network_length_m,
                            reachable_edge_count,
                        });
                    }

                    origin_bands.push(ServiceAreaOriginBand {
                        origin_id: expansion.origin_id.clone(),
                        origin_component_id: expansion.representative_origin.component_id,
                        fallback_used: expansion.fallback_used,
                        origin_hop_distance_m: expansion.origin_hop_distance_m,
                        threshold_id: threshold.id.clone(),
                        band_start_limit: previous_limit,
                        threshold_limit: threshold.limit,
                        threshold_metric: threshold.metric,
                        segments,
                    });
                    previous_limit = Some(threshold.limit);
                }
            }
        }

        if origin_processed {
            processed_origin_count += 1;
        } else if origin_skipped {
            skipped_origin_count += 1;
        }
    }

    let origin_bands = match request.multi_origin_mode {
        ServiceAreaMultiOriginMode::Overlap => origin_bands,
        ServiceAreaMultiOriginMode::Merge => merge_service_area_bands(origin_bands),
        ServiceAreaMultiOriginMode::Cut => cut_service_area_bands(origin_bands),
    };
    if matches!(request.multi_origin_mode, ServiceAreaMultiOriginMode::Merge)
        && processed_origin_count > 1
        && !metrics.components.is_empty()
    {
        warnings.push(
            "Merged multi-origin service-area segments omit cumulative component vectors because the union has no single origin path; use multi_origin_mode=overlap or cut for path-owned labels."
                .to_string(),
        );
    }

    ensure_service_area_output_limits(request, &origin_bands)?;

    let segments =
        if request.returns.segments || matches!(request.band_mode, ServiceAreaBandMode::Unbanded) {
            build_service_area_segments(topology, metrics, request, &origin_bands)
        } else {
            Vec::new()
        };

    let features = if request.returns.geometry || request.returns.attributes {
        build_service_area_features(topology, metrics, request, &origin_bands)
    } else {
        Vec::new()
    };

    let diagnostics = if request.returns.diagnostics {
        diagnostics
    } else {
        Vec::new()
    };

    Ok(ServiceAreaResult {
        analysis_id: request.analysis_id.clone(),
        departure_time: request.temporal.departure_time.clone(),
        scenario_id: temporal_runtime
            .as_ref()
            .and_then(|(context, _)| context.scenario_id.clone()),
        outcome: service_area_result_outcome(
            processed_origin_count,
            skipped_origin_count,
            fallback_origin_count,
        ),
        output_mode: request.output_mode,
        band_mode: request.band_mode,
        boundary_mode: request.boundary_mode,
        multi_origin_mode: request.multi_origin_mode,
        origin_count: request.origins.len(),
        processed_origin_count,
        skipped_origin_count,
        fallback_origin_count,
        threshold_count: request.thresholds.len(),
        features,
        summaries: threshold_summaries,
        segments,
        diagnostics,
        warnings,
    })
}

fn ensure_service_area_output_limits(
    request: &ServiceAreaRequest,
    bands: &[ServiceAreaOriginBand],
) -> Result<()> {
    let source_segment_count = bands.iter().map(|band| band.segments.len()).sum::<usize>();
    let result_segment_count =
        if request.returns.segments || matches!(request.band_mode, ServiceAreaBandMode::Unbanded) {
            source_segment_count
        } else {
            0
        };
    ensure_service_area_limit(
        result_segment_count,
        request.returns.max_segments,
        "segments",
        "disable returns.segments, avoid band_mode=none, reduce thresholds/origins, or raise returns.max_segments explicitly",
    )?;

    let feature_count = if request.returns.geometry || request.returns.attributes {
        estimate_service_area_feature_count(request, bands)
    } else {
        0
    };
    ensure_service_area_limit(
        feature_count,
        request.returns.max_features,
        "features",
        "request polygon-only output, reduce thresholds/origins, avoid band_mode=none, or raise returns.max_features explicitly",
    )?;

    if request.returns.geometry {
        let geometry_point_count = estimate_service_area_geometry_points(request, bands);
        ensure_service_area_limit(
            geometry_point_count,
            request.returns.max_geometry_points,
            "geometry_points",
            "request polygon-only output, disable returns.geometry, reduce thresholds/origins, or raise returns.max_geometry_points explicitly",
        )?;
    }

    Ok(())
}

fn ensure_service_area_limit(
    count: usize,
    max_count: usize,
    label: &str,
    advice: &str,
) -> Result<()> {
    if count > max_count {
        bail!(
            "service-area output would contain {} {} but returns.max_{} is {}; {}",
            count,
            label,
            label,
            max_count,
            advice
        );
    }
    Ok(())
}

fn estimate_service_area_feature_count(
    request: &ServiceAreaRequest,
    bands: &[ServiceAreaOriginBand],
) -> usize {
    if matches!(request.band_mode, ServiceAreaBandMode::Unbanded) {
        if matches!(
            request.output_mode,
            ServiceAreaOutputMode::Network | ServiceAreaOutputMode::Both
        ) {
            return bands.iter().map(|band| band.segments.len()).sum();
        }
        return 0;
    }

    bands
        .iter()
        .map(|_| {
            let mut count = 0_usize;
            if matches!(
                request.output_mode,
                ServiceAreaOutputMode::Network | ServiceAreaOutputMode::Both
            ) {
                count += 1;
            }
            if matches!(
                request.output_mode,
                ServiceAreaOutputMode::Polygon | ServiceAreaOutputMode::Both
            ) {
                count += 1;
            }
            count
        })
        .sum()
}

fn estimate_service_area_geometry_points(
    request: &ServiceAreaRequest,
    bands: &[ServiceAreaOriginBand],
) -> usize {
    let source_segment_count = bands.iter().map(|band| band.segments.len()).sum::<usize>();
    let mut point_count = 0_usize;

    if matches!(request.band_mode, ServiceAreaBandMode::Unbanded) {
        if matches!(
            request.output_mode,
            ServiceAreaOutputMode::Network | ServiceAreaOutputMode::Both
        ) {
            point_count = point_count.saturating_add(source_segment_count.saturating_mul(2));
        }
    } else {
        for band in bands {
            if matches!(
                request.output_mode,
                ServiceAreaOutputMode::Network | ServiceAreaOutputMode::Both
            ) {
                point_count = point_count.saturating_add(band.segments.len().saturating_mul(2));
            }
            if matches!(
                request.output_mode,
                ServiceAreaOutputMode::Polygon | ServiceAreaOutputMode::Both
            ) {
                point_count = point_count.saturating_add(band.segments.len().saturating_mul(4) + 1);
            }
        }
    }

    if request.returns.segments || matches!(request.band_mode, ServiceAreaBandMode::Unbanded) {
        point_count = point_count.saturating_add(source_segment_count.saturating_mul(2));
    }

    point_count
}

fn thresholds_for_service_area(
    request: &ServiceAreaRequest,
) -> Vec<(ServiceAreaMetricKind, Vec<&ServiceAreaThreshold>)> {
    let mut distance = request
        .thresholds
        .iter()
        .filter(|threshold| matches!(threshold.metric, ServiceAreaThresholdMetric::DistanceM))
        .collect::<Vec<_>>();
    let mut time = request
        .thresholds
        .iter()
        .filter(|threshold| matches!(threshold.metric, ServiceAreaThresholdMetric::TravelTimeS))
        .collect::<Vec<_>>();
    distance.sort_by(|left, right| left.limit.total_cmp(&right.limit));
    time.sort_by(|left, right| left.limit.total_cmp(&right.limit));

    let mut groups = Vec::new();
    if !distance.is_empty() {
        groups.push((ServiceAreaMetricKind::DistanceM, distance));
    }
    if !time.is_empty() {
        groups.push((ServiceAreaMetricKind::TravelTimeS, time));
    }
    groups
}

pub(crate) fn build_service_area_expansion(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    point: &LabeledPoint,
    candidates: &[SnappedPoint],
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    metric_kind: ServiceAreaMetricKind,
    max_cost: f64,
) -> Result<ServiceAreaOriginExpansion> {
    build_service_area_expansion_with_settle_limit(
        topology,
        metrics,
        routing_graph,
        point,
        candidates,
        snap_max_distance_m,
        connectivity,
        metric_kind,
        max_cost,
        default_phast_settle_limit(topology.edge_count()),
    )
}

/// Exact FIFO one-to-many expansion for departure-time service areas. The
/// settled key retains the restriction-automaton state; per-edge output
/// keeps the best arrival across states for the existing isochrone renderer.
pub(crate) fn build_temporal_service_area_expansion(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    point: &LabeledPoint,
    candidates: &[SnappedPoint],
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    departure: time::OffsetDateTime,
    max_cost: f64,
    context: &TemporalContext,
) -> Result<ServiceAreaOriginExpansion> {
    let resolution =
        resolve_service_area_origin(point, candidates, snap_max_distance_m, connectivity)?;
    let edge_count = topology.edge_count();
    let mut edge_before_costs = vec![f64::INFINITY; edge_count];
    let mut edge_end_costs = vec![f64::INFINITY; edge_count];
    let mut edge_start_fractions = vec![0.0_f64; edge_count];
    let mut edge_component_ranges = HashMap::new();
    let mut reached_edges = Vec::new();
    let mut dist = HashMap::<SearchStateKey, f64>::new();
    let mut state_components = HashMap::<SearchStateKey, Vec<f64>>::new();
    let track_components = !metrics.components.is_empty();
    let mut heap = BinaryHeap::new();

    for candidate in &resolution.seed_candidates {
        let seeds = if let (Some(edge_id), Some(fraction)) =
            (candidate.snapped_edge_id, candidate.snapped_edge_fraction)
        {
            vec![(edge_id as usize, 1.0 - fraction, fraction)]
        } else {
            routing_graph
                .outgoing_edges(candidate.snapped_node_id as usize)
                .iter()
                .map(|edge| (*edge as usize, 1.0, 0.0))
                .collect()
        };
        for (edge_index, fraction, start_fraction) in seeds {
            let Some(edge_cost) = crate::temporal::evaluate_edge_at(
                topology, metrics, context, edge_index, departure, fraction,
            ) else {
                continue;
            };
            let before = edge_cost.wait_s;
            if before > max_cost {
                continue;
            }
            let end = before + edge_cost.travel_time_s;
            let automaton_state = routing_graph.automaton.transition(0, edge_index);
            let key = SearchStateKey {
                edge_index,
                automaton_state,
            };
            if end + f64::EPSILON >= dist.get(&key).copied().unwrap_or(f64::INFINITY) {
                continue;
            }
            dist.insert(key, end);
            let before_components = zero_component_vector(metrics);
            let end_components = component_vector_from_map(metrics, &edge_cost.components);
            if track_components {
                state_components.insert(key, end_components.clone());
            }
            update_service_area_edge_best(
                &mut edge_before_costs,
                &mut edge_end_costs,
                &mut edge_start_fractions,
                &mut edge_component_ranges,
                &mut reached_edges,
                edge_index,
                before,
                end,
                start_fraction,
                before_components,
                end_components,
            );
            if end <= max_cost {
                heap.push(State {
                    edge_index,
                    automaton_state,
                    cost: end,
                    score: end,
                });
            }
        }
    }

    while let Some(State {
        edge_index,
        automaton_state,
        cost,
        ..
    }) = heap.pop()
    {
        let key = SearchStateKey {
            edge_index,
            automaton_state,
        };
        if cost > dist.get(&key).copied().unwrap_or(f64::INFINITY) || cost > max_cost {
            continue;
        }
        let path_components = if track_components {
            let Some(components) = state_components.get(&key).cloned() else {
                continue;
            };
            components
        } else {
            Vec::new()
        };
        for transition_index in routing_graph.transition_range(edge_index) {
            let next_edge = routing_graph.transition_edges[transition_index] as usize;
            if !routing_graph
                .automaton
                .is_transition_allowed(automaton_state, next_edge)
            {
                continue;
            }
            let turn_time_s = turn_penalty_seconds(topology, metrics, edge_index, next_edge);
            let requested_entry = departure + time::Duration::seconds_f64(cost + turn_time_s);
            let Some(edge_cost) = crate::temporal::evaluate_edge_at(
                topology,
                metrics,
                context,
                next_edge,
                requested_entry,
                1.0,
            ) else {
                continue;
            };
            let before = cost + turn_time_s + edge_cost.wait_s;
            if before > max_cost {
                continue;
            }
            let end = before + edge_cost.travel_time_s;
            let next_state = routing_graph
                .automaton
                .transition(automaton_state, next_edge);
            let next_key = SearchStateKey {
                edge_index: next_edge,
                automaton_state: next_state,
            };
            if end + f64::EPSILON >= dist.get(&next_key).copied().unwrap_or(f64::INFINITY) {
                continue;
            }
            dist.insert(next_key, end);
            let before_components = path_components.clone();
            let mut end_components = before_components.clone();
            add_component_vector(
                &mut end_components,
                &component_vector_from_map(metrics, &edge_cost.components),
            );
            if track_components {
                state_components.insert(next_key, end_components.clone());
            }
            update_service_area_edge_best(
                &mut edge_before_costs,
                &mut edge_end_costs,
                &mut edge_start_fractions,
                &mut edge_component_ranges,
                &mut reached_edges,
                next_edge,
                before,
                end,
                0.0,
                before_components,
                end_components,
            );
            if end <= max_cost {
                heap.push(State {
                    edge_index: next_edge,
                    automaton_state: next_state,
                    cost: end,
                    score: end,
                });
            }
        }
    }

    let mut warnings = resolution.warnings;
    warnings.push(
        "Time-dependent service-area expansion used exact FIFO Dijkstra; static PHAST/CCH acceleration was intentionally bypassed."
            .to_string(),
    );
    Ok(ServiceAreaOriginExpansion {
        origin_id: point.id.clone(),
        representative_origin: resolution.representative_origin,
        fallback_used: resolution.fallback_used,
        origin_hop_distance_m: resolution.origin_hop_distance_m,
        edge_before_costs,
        edge_end_costs,
        edge_start_fractions,
        edge_component_ranges,
        reached_edges,
        diagnostics: resolution.diagnostics,
        warnings,
    })
}

/// Bounded Dijkstra beats a full hierarchy sweep while the reachable ball is
/// small; once the ball grows past a fraction of the graph, the linear PHAST
/// sweep wins. The limit is where the expansion aborts and switches (both
/// algorithms are exact, so this is purely a cost crossover).
fn default_phast_settle_limit(edge_count: usize) -> usize {
    (edge_count / 8).max(10_000)
}

pub(crate) fn build_service_area_expansion_with_settle_limit(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    point: &LabeledPoint,
    candidates: &[SnappedPoint],
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
    metric_kind: ServiceAreaMetricKind,
    max_cost: f64,
    phast_settle_limit: usize,
) -> Result<ServiceAreaOriginExpansion> {
    let resolution =
        resolve_service_area_origin(point, candidates, snap_max_distance_m, connectivity)?;
    let edge_count = topology.edge_count();
    let mut edge_before_costs = vec![f64::INFINITY; edge_count];
    let mut edge_end_costs = vec![f64::INFINITY; edge_count];
    let mut edge_start_fractions = vec![0.0_f64; edge_count];
    let mut edge_component_ranges = HashMap::new();
    let mut reached_edges = Vec::new();
    let track_components = !metrics.components.is_empty();

    if routing_graph.has_restriction_sequences() {
        let mut dist = HashMap::<SearchStateKey, f64>::new();
        let mut state_components = HashMap::<SearchStateKey, Vec<f64>>::new();
        let mut heap = BinaryHeap::new();
        for candidate in &resolution.seed_candidates {
            for seed in
                service_area_seed_specs(routing_graph, topology, metrics, candidate, metric_kind)
            {
                let automaton_state = routing_graph.automaton.transition(0, seed.edge_index);
                let key = SearchStateKey {
                    edge_index: seed.edge_index,
                    automaton_state,
                };
                if seed.before_cost > max_cost {
                    continue;
                }
                let previous = dist.get(&key).copied().unwrap_or(f64::INFINITY);
                if seed.end_cost + f64::EPSILON >= previous {
                    continue;
                }
                dist.insert(key, seed.end_cost);
                let before_components = zero_component_vector(metrics);
                let end_components =
                    static_component_vector(metrics, seed.edge_index, 1.0 - seed.start_fraction);
                if track_components {
                    state_components.insert(key, end_components.clone());
                }
                update_service_area_edge_best(
                    &mut edge_before_costs,
                    &mut edge_end_costs,
                    &mut edge_start_fractions,
                    &mut edge_component_ranges,
                    &mut reached_edges,
                    seed.edge_index,
                    seed.before_cost,
                    seed.end_cost,
                    seed.start_fraction,
                    before_components,
                    end_components,
                );
                if seed.end_cost <= max_cost {
                    heap.push(State {
                        edge_index: seed.edge_index,
                        automaton_state,
                        cost: seed.end_cost,
                        score: seed.end_cost,
                    });
                }
            }
        }

        while let Some(State {
            edge_index,
            automaton_state,
            cost,
            score: _,
        }) = heap.pop()
        {
            let key = SearchStateKey {
                edge_index,
                automaton_state,
            };
            if cost > dist.get(&key).copied().unwrap_or(f64::INFINITY) {
                continue;
            }
            if cost > max_cost {
                continue;
            }
            let path_components = if track_components {
                let Some(components) = state_components.get(&key).cloned() else {
                    continue;
                };
                components
            } else {
                Vec::new()
            };

            for transition_index in routing_graph.transition_range(edge_index) {
                let next_edge = routing_graph.transition_edges[transition_index] as usize;
                if !routing_graph
                    .automaton
                    .is_transition_allowed(automaton_state, next_edge)
                {
                    continue;
                }
                let Some(edge_cost) =
                    service_area_edge_cost(topology, metrics, next_edge, metric_kind)
                else {
                    continue;
                };
                let next_before = cost
                    + service_area_turn_cost(topology, metrics, edge_index, next_edge, metric_kind);
                if next_before > max_cost {
                    continue;
                }
                let next_end = next_before + edge_cost;
                let next_state = routing_graph
                    .automaton
                    .transition(automaton_state, next_edge);
                let next_key = SearchStateKey {
                    edge_index: next_edge,
                    automaton_state: next_state,
                };
                let previous = dist.get(&next_key).copied().unwrap_or(f64::INFINITY);
                if next_end + f64::EPSILON >= previous {
                    continue;
                }
                dist.insert(next_key, next_end);
                let before_components = path_components.clone();
                let mut end_components = before_components.clone();
                add_component_vector(
                    &mut end_components,
                    &static_component_vector(metrics, next_edge, 1.0),
                );
                if track_components {
                    state_components.insert(next_key, end_components.clone());
                }
                update_service_area_edge_best(
                    &mut edge_before_costs,
                    &mut edge_end_costs,
                    &mut edge_start_fractions,
                    &mut edge_component_ranges,
                    &mut reached_edges,
                    next_edge,
                    next_before,
                    next_end,
                    0.0,
                    before_components,
                    end_components,
                );
                if next_end <= max_cost {
                    heap.push(State {
                        edge_index: next_edge,
                        automaton_state: next_state,
                        cost: next_end,
                        score: next_end,
                    });
                }
            }
        }
    } else {
        let phast_ready = routing_graph
            .acceleration
            .as_ref()
            .is_some_and(|acceleration| acceleration.metric_weights(metric_kind).is_some());
        // CCH/PHAST stores only scalar customized weights. Without unpacking
        // every winning hierarchy path it cannot recover an exact additive
        // component label, so component-bearing profiles remain on the exact
        // threshold-bounded edge Dijkstra.
        let settle_limit = if phast_ready && metrics.components.is_empty() {
            phast_settle_limit
        } else {
            usize::MAX
        };
        let aborted = SINGLE_SOURCE_SEARCH_SCRATCH.with(|scratch| {
            let mut scratch = scratch.borrow_mut();
            scratch.prepare(edge_count);
            let mut path_components = HashMap::<usize, Vec<f64>>::new();
            let mut settled = 0_usize;
            for candidate in &resolution.seed_candidates {
                for seed in service_area_seed_specs(
                    routing_graph,
                    topology,
                    metrics,
                    candidate,
                    metric_kind,
                ) {
                    if seed.before_cost > max_cost {
                        continue;
                    }
                    if !scratch.update(seed.edge_index, seed.end_cost, NO_PREVIOUS_EDGE) {
                        continue;
                    }
                    let before_components = zero_component_vector(metrics);
                    let end_components = static_component_vector(
                        metrics,
                        seed.edge_index,
                        1.0 - seed.start_fraction,
                    );
                    if track_components {
                        path_components.insert(seed.edge_index, end_components.clone());
                    }
                    update_service_area_edge_best(
                        &mut edge_before_costs,
                        &mut edge_end_costs,
                        &mut edge_start_fractions,
                        &mut edge_component_ranges,
                        &mut reached_edges,
                        seed.edge_index,
                        seed.before_cost,
                        seed.end_cost,
                        seed.start_fraction,
                        before_components,
                        end_components,
                    );
                    if seed.end_cost <= max_cost {
                        scratch.heap.push(State {
                            edge_index: seed.edge_index,
                            automaton_state: 0,
                            cost: seed.end_cost,
                            score: seed.end_cost,
                        });
                    }
                }
            }

            while let Some(State {
                edge_index,
                automaton_state: _,
                cost,
                score: _,
            }) = scratch.heap.pop()
            {
                if cost > scratch.dist[edge_index] {
                    continue;
                }
                if cost > max_cost {
                    continue;
                }
                settled += 1;
                if settled > settle_limit {
                    return true;
                }
                let current_components = path_components
                    .get(&edge_index)
                    .cloned()
                    .unwrap_or_else(|| zero_component_vector(metrics));

                for transition_index in routing_graph.transition_range(edge_index) {
                    let next_edge = routing_graph.transition_edges[transition_index] as usize;
                    let Some(edge_cost) =
                        service_area_edge_cost(topology, metrics, next_edge, metric_kind)
                    else {
                        continue;
                    };
                    let next_before = cost
                        + service_area_turn_cost(
                            topology,
                            metrics,
                            edge_index,
                            next_edge,
                            metric_kind,
                        );
                    if next_before > max_cost {
                        continue;
                    }
                    let next_end = next_before + edge_cost;
                    if !scratch.update(next_edge, next_end, edge_index as u32) {
                        continue;
                    }
                    let before_components = current_components.clone();
                    let mut end_components = before_components.clone();
                    add_component_vector(
                        &mut end_components,
                        &static_component_vector(metrics, next_edge, 1.0),
                    );
                    if track_components {
                        path_components.insert(next_edge, end_components.clone());
                    }
                    update_service_area_edge_best(
                        &mut edge_before_costs,
                        &mut edge_end_costs,
                        &mut edge_start_fractions,
                        &mut edge_component_ranges,
                        &mut reached_edges,
                        next_edge,
                        next_before,
                        next_end,
                        0.0,
                        before_components,
                        end_components,
                    );
                    if next_end <= max_cost {
                        scratch.heap.push(State {
                            edge_index: next_edge,
                            automaton_state: 0,
                            cost: next_end,
                            score: next_end,
                        });
                    }
                }
            }
            false
        });

        if aborted {
            // The reachable ball outgrew the Dijkstra sweet spot: reset the
            // partial recording and redo the expansion with the exact
            // hierarchy sweep instead.
            for &edge_index in &reached_edges {
                edge_before_costs[edge_index as usize] = f64::INFINITY;
                edge_end_costs[edge_index as usize] = f64::INFINITY;
                edge_start_fractions[edge_index as usize] = 0.0;
                edge_component_ranges.remove(&(edge_index as usize));
            }
            reached_edges.clear();
            phast_service_area_expansion(
                topology,
                metrics,
                routing_graph,
                &resolution.seed_candidates,
                metric_kind,
                max_cost,
                &mut edge_before_costs,
                &mut edge_end_costs,
                &mut edge_start_fractions,
                &mut edge_component_ranges,
                &mut reached_edges,
            );
        }
    }

    let mut warnings = resolution.warnings;
    if phast_ready_for_metric(routing_graph, metric_kind) && !metrics.components.is_empty() {
        warnings.push(
            "Service-area component vectors used exact threshold-bounded Dijkstra; PHAST was intentionally bypassed because its scalar labels cannot reconstruct additive component vectors."
                .to_string(),
        );
    }
    Ok(ServiceAreaOriginExpansion {
        origin_id: point.id.clone(),
        representative_origin: resolution.representative_origin,
        fallback_used: resolution.fallback_used,
        origin_hop_distance_m: resolution.origin_hop_distance_m,
        edge_before_costs,
        edge_end_costs,
        edge_start_fractions,
        edge_component_ranges,
        reached_edges,
        diagnostics: resolution.diagnostics,
        warnings,
    })
}

/// Exact bounded one-to-all over the customized hierarchy for a
/// service-area metric: an upward Dijkstra from the seeds followed by a
/// rank-descending sweep of the downward arcs. Every edge state receives its
/// exact metric distance through its own up-down path, so no unpacking is
/// needed. States whose distance exceeds `max_cost` are kept (they may
/// still contribute a partially reachable edge) but never propagated —
/// earlier edges on any within-bound path are themselves within bound, so
/// the cutoff loses nothing.
fn phast_service_area_expansion(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    routing_graph: &RoutingGraph,
    seed_candidates: &[SnappedPoint],
    metric_kind: ServiceAreaMetricKind,
    max_cost: f64,
    edge_before_costs: &mut [f64],
    edge_end_costs: &mut [f64],
    edge_start_fractions: &mut [f64],
    edge_component_ranges: &mut HashMap<usize, ServiceAreaEdgeComponentRange>,
    reached_edges: &mut Vec<u32>,
) {
    let acceleration = routing_graph
        .acceleration
        .as_ref()
        .expect("PHAST expansion requires acceleration");
    let (upward_weight, downward_weight) = acceleration
        .metric_weights(metric_kind)
        .expect("PHAST expansion requires per-metric weights");
    let edge_count = routing_graph.edge_costs.len();

    let mut dist = vec![f64::INFINITY; edge_count];
    let mut heap = BinaryHeap::new();
    for candidate in seed_candidates {
        for seed in
            service_area_seed_specs(routing_graph, topology, metrics, candidate, metric_kind)
        {
            if seed.before_cost > max_cost {
                continue;
            }
            update_service_area_edge_best(
                edge_before_costs,
                edge_end_costs,
                edge_start_fractions,
                edge_component_ranges,
                reached_edges,
                seed.edge_index,
                seed.before_cost,
                seed.end_cost,
                seed.start_fraction,
                Vec::new(),
                Vec::new(),
            );
            if seed.end_cost < dist[seed.edge_index] {
                dist[seed.edge_index] = seed.end_cost;
                heap.push(State {
                    edge_index: seed.edge_index,
                    automaton_state: 0,
                    cost: seed.end_cost,
                    score: seed.end_cost,
                });
            }
        }
    }

    // Upward phase: settle the hierarchy states reachable through upward
    // arcs only.
    while let Some(State {
        edge_index, cost, ..
    }) = heap.pop()
    {
        if cost > dist[edge_index] {
            continue;
        }
        if cost > max_cost {
            continue;
        }
        #[allow(clippy::needless_range_loop)]
        for slot in acceleration.source.upward_first_out[edge_index] as usize
            ..acceleration.source.upward_first_out[edge_index + 1] as usize
        {
            let next_edge = acceleration.source.upward_head[slot] as usize;
            let next_cost = cost + upward_weight[slot];
            if next_cost < dist[next_edge] {
                dist[next_edge] = next_cost;
                heap.push(State {
                    edge_index: next_edge,
                    automaton_state: 0,
                    cost: next_cost,
                    score: next_cost,
                });
            }
        }
    }

    // Downward sweep in descending rank order: each state's distance is
    // final before any of its downward arcs are relaxed.
    for &state in acceleration.source.edge_order.iter().rev() {
        let state = state as usize;
        let cost = dist[state];
        if !cost.is_finite() || cost > max_cost {
            continue;
        }
        #[allow(clippy::needless_range_loop)]
        for slot in acceleration.source.downward_first_out[state] as usize
            ..acceleration.source.downward_first_out[state + 1] as usize
        {
            let head = acceleration.source.downward_head[slot] as usize;
            let next_cost = cost + downward_weight[slot];
            if next_cost < dist[head] {
                dist[head] = next_cost;
            }
        }
    }

    for (edge_index, &end_cost) in dist.iter().enumerate() {
        if !end_cost.is_finite() {
            continue;
        }
        let Some(edge_cost) = service_area_edge_cost(topology, metrics, edge_index, metric_kind)
        else {
            continue;
        };
        let before_cost = (end_cost - edge_cost).max(0.0);
        if before_cost > max_cost {
            continue;
        }
        update_service_area_edge_best(
            edge_before_costs,
            edge_end_costs,
            edge_start_fractions,
            edge_component_ranges,
            reached_edges,
            edge_index,
            before_cost,
            end_cost,
            0.0,
            Vec::new(),
            Vec::new(),
        );
    }
}

#[derive(Debug, Clone, Copy)]
struct ServiceAreaSeedSpec {
    edge_index: usize,
    before_cost: f64,
    end_cost: f64,
    start_fraction: f64,
}

fn service_area_seed_specs(
    routing_graph: &RoutingGraph,
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    candidate: &SnappedPoint,
    metric_kind: ServiceAreaMetricKind,
) -> Vec<ServiceAreaSeedSpec> {
    if let (Some(edge_id), Some(fraction)) =
        (candidate.snapped_edge_id, candidate.snapped_edge_fraction)
    {
        let edge_index = edge_id as usize;
        let Some(full_cost) = service_area_edge_cost(topology, metrics, edge_index, metric_kind)
        else {
            return Vec::new();
        };
        let remaining = full_cost * (1.0 - fraction);
        if remaining <= f64::EPSILON {
            return Vec::new();
        }
        return vec![ServiceAreaSeedSpec {
            edge_index,
            before_cost: 0.0,
            end_cost: remaining,
            start_fraction: fraction,
        }];
    }

    routing_graph
        .outgoing_edges(candidate.snapped_node_id as usize)
        .iter()
        .filter_map(|&edge_index| {
            let edge_index = edge_index as usize;
            service_area_edge_cost(topology, metrics, edge_index, metric_kind).and_then(|cost| {
                (cost > f64::EPSILON).then_some(ServiceAreaSeedSpec {
                    edge_index,
                    before_cost: 0.0,
                    end_cost: cost,
                    start_fraction: 0.0,
                })
            })
        })
        .collect()
}

fn update_service_area_edge_best(
    edge_before_costs: &mut [f64],
    edge_end_costs: &mut [f64],
    edge_start_fractions: &mut [f64],
    edge_component_ranges: &mut HashMap<usize, ServiceAreaEdgeComponentRange>,
    reached_edges: &mut Vec<u32>,
    edge_index: usize,
    before_cost: f64,
    end_cost: f64,
    start_fraction: f64,
    before_components: Vec<f64>,
    end_components: Vec<f64>,
) {
    if end_cost + f64::EPSILON < edge_end_costs[edge_index] {
        if !edge_end_costs[edge_index].is_finite() {
            reached_edges.push(edge_index as u32);
        }
        edge_before_costs[edge_index] = before_cost;
        edge_end_costs[edge_index] = end_cost;
        edge_start_fractions[edge_index] = start_fraction;
        if !before_components.is_empty() || !end_components.is_empty() {
            edge_component_ranges.insert(
                edge_index,
                ServiceAreaEdgeComponentRange {
                    before: before_components,
                    end: end_components,
                },
            );
        }
    }
}

fn zero_component_vector(metrics: &CompiledProfileBundle) -> Vec<f64> {
    vec![0.0; metrics.components.len()]
}

fn static_component_vector(
    metrics: &CompiledProfileBundle,
    edge_index: usize,
    factor: f64,
) -> Vec<f64> {
    metrics
        .components
        .iter()
        .map(|component| {
            let overlay_multiplier =
                if component.overlay_name.is_some() && !component.invert_overlay {
                    0.0
                } else {
                    1.0
                };
            f64::from(*component.edge_values.get(edge_index).unwrap_or(&0.0))
                * factor
                * overlay_multiplier
        })
        .collect()
}

fn component_vector_from_map(
    metrics: &CompiledProfileBundle,
    components: &BTreeMap<String, f64>,
) -> Vec<f64> {
    metrics
        .components
        .iter()
        .map(|component| components.get(&component.name).copied().unwrap_or_default())
        .collect()
}

fn add_component_vector(target: &mut [f64], increment: &[f64]) {
    for (target, increment) in target.iter_mut().zip(increment) {
        *target += increment;
    }
}

fn component_map(metrics: &CompiledProfileBundle, values: &[f64]) -> BTreeMap<String, f64> {
    metrics
        .components
        .iter()
        .zip(values)
        .map(|(component, value)| (component.name.clone(), *value))
        .collect()
}

fn phast_ready_for_metric(
    routing_graph: &RoutingGraph,
    metric_kind: ServiceAreaMetricKind,
) -> bool {
    routing_graph
        .acceleration
        .as_ref()
        .is_some_and(|acceleration| acceleration.metric_weights(metric_kind).is_some())
}

fn resolve_service_area_origin(
    point: &LabeledPoint,
    candidates: &[SnappedPoint],
    snap_max_distance_m: f64,
    connectivity: &ConnectivityPolicy,
) -> Result<ServiceAreaOriginResolution> {
    let mut warnings = Vec::new();
    let max_hop_distance_m = connectivity
        .max_hop_distance_m
        .unwrap_or(snap_max_distance_m);

    let nearest_candidates = |limit_m: f64| -> Vec<SnappedPoint> {
        let min_distance = candidates
            .iter()
            .filter(|candidate| candidate.snap_distance_m <= limit_m)
            .map(|candidate| candidate.snap_distance_m)
            .min_by(|left, right| left.total_cmp(right));
        candidates
            .iter()
            .filter(|candidate| {
                min_distance.is_some_and(|distance| {
                    candidate.snap_distance_m <= limit_m
                        && (candidate.snap_distance_m - distance).abs() <= 1e-6
                })
            })
            .cloned()
            .collect()
    };

    let strict_candidates = nearest_candidates(snap_max_distance_m);
    let hop_candidates = nearest_candidates(max_hop_distance_m);

    let seed_candidates = match connectivity.disconnected {
        DisconnectedNetworkMode::Strict
        | DisconnectedNetworkMode::IgnoreUnreachable
        | DisconnectedNetworkMode::HopDestinationToNearestReachableComponent => {
            if matches!(
                connectivity.disconnected,
                DisconnectedNetworkMode::HopDestinationToNearestReachableComponent
            ) {
                warnings.push(
                    "connectivity.disconnected=hop_destination_to_nearest_reachable_component has no effect for service-area origins; strict origin snapping was used.".to_string(),
                );
            }
            strict_candidates
        }
        DisconnectedNetworkMode::HopOriginToNearestReachableComponent
        | DisconnectedNetworkMode::HopEitherEnd => {
            if !strict_candidates.is_empty() {
                strict_candidates
            } else {
                hop_candidates
            }
        }
    };

    let Some(representative_origin) = seed_candidates
        .iter()
        .min_by(|left, right| left.snap_distance_m.total_cmp(&right.snap_distance_m))
        .cloned()
    else {
        return Err(route_snap_failure(point, snap_max_distance_m).into());
    };

    let fallback_used = representative_origin.snap_distance_m > snap_max_distance_m;
    let origin_hop_distance_m = fallback_used.then_some(representative_origin.snap_distance_m);
    let mut diagnostics = Vec::new();
    if fallback_used {
        diagnostics.push(AnalysisDiagnostic {
            code: AnalysisDiagnosticCode::FallbackUsed,
            severity: AnalysisDiagnosticSeverity::Warning,
            message: format!(
                "Connectivity fallback hopped service-area origin '{}' {:.1} m to reach component {}.",
                point.id,
                representative_origin.snap_distance_m,
                representative_origin.component_id.unwrap_or_default()
            ),
            point_ids: vec![point.id.clone()],
            component_ids: representative_origin.component_id.into_iter().collect(),
            suggested_actions: vec![],
        });
        warnings.push(format!(
            "Service-area connectivity fallback used a non-network origin hop of {:.1} m for '{}'.",
            representative_origin.snap_distance_m, point.id
        ));
    }

    Ok(ServiceAreaOriginResolution {
        seed_candidates,
        representative_origin,
        fallback_used,
        origin_hop_distance_m,
        diagnostics,
        warnings,
    })
}

pub(crate) fn service_area_edge_cost(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    edge_index: usize,
    metric_kind: ServiceAreaMetricKind,
) -> Option<f64> {
    match metric_kind {
        ServiceAreaMetricKind::DistanceM => Some(topology.routing_edge(edge_index).length_m as f64),
        ServiceAreaMetricKind::TravelTimeS => metrics.edge_metrics[edge_index].travel_time_s,
    }
}

fn service_area_turn_cost(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    previous_edge_index: usize,
    next_edge_index: usize,
    metric_kind: ServiceAreaMetricKind,
) -> f64 {
    match metric_kind {
        ServiceAreaMetricKind::DistanceM => 0.0,
        ServiceAreaMetricKind::TravelTimeS => {
            turn_penalty_seconds(topology, metrics, previous_edge_index, next_edge_index)
        }
    }
}

fn service_area_intervals_for_threshold(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    expansion: &ServiceAreaOriginExpansion,
    metric_kind: ServiceAreaMetricKind,
    threshold_limit: f64,
    boundary_mode: ServiceAreaBoundaryMode,
) -> Vec<ReachableEdgeInterval> {
    let mut segments = Vec::new();
    for &edge_index in &expansion.reached_edges {
        let edge_index = edge_index as usize;
        let before_cost = expansion.edge_before_costs[edge_index];
        let end_cost = expansion.edge_end_costs[edge_index];
        if !before_cost.is_finite() || !end_cost.is_finite() || threshold_limit <= before_cost {
            continue;
        }
        let start_fraction = expansion.edge_start_fractions[edge_index];
        let mut end_fraction = 1.0_f64;
        if matches!(boundary_mode, ServiceAreaBoundaryMode::CutAtBoundary)
            && threshold_limit + f64::EPSILON < end_cost
        {
            let remaining_cost = end_cost - before_cost;
            if remaining_cost <= f64::EPSILON {
                continue;
            }
            let progress = ((threshold_limit - before_cost) / remaining_cost).clamp(0.0, 1.0);
            end_fraction = start_fraction + (1.0 - start_fraction) * progress;
        }
        if end_fraction <= start_fraction + f64::EPSILON {
            continue;
        }
        let midpoint_fraction = (start_fraction + end_fraction) / 2.0;
        let midpoint_progress = if start_fraction >= 1.0 - f64::EPSILON {
            1.0
        } else {
            ((midpoint_fraction - start_fraction) / (1.0 - start_fraction)).clamp(0.0, 1.0)
        };
        let full_edge_cost =
            service_area_edge_cost(topology, metrics, edge_index, metric_kind).unwrap_or_default();
        // The start point sits at progress 0 of the remaining edge span by
        // definition (the generic `(f - start_fraction) / (1 - start_fraction)`
        // formula evaluated at `f = start_fraction`).
        let start_progress = if start_fraction >= 1.0 - f64::EPSILON {
            1.0
        } else {
            0.0
        };
        let end_progress = if start_fraction >= 1.0 - f64::EPSILON {
            1.0
        } else {
            ((end_fraction - start_fraction) / (1.0 - start_fraction)).clamp(0.0, 1.0)
        };
        let start_cost = before_cost + full_edge_cost * (1.0 - start_fraction) * start_progress;
        let end_cost = before_cost + full_edge_cost * (1.0 - start_fraction) * end_progress;
        let midpoint_cost =
            before_cost + full_edge_cost * (1.0 - start_fraction) * midpoint_progress;
        let component_range = expansion.edge_component_ranges.get(&edge_index);
        let before_components = component_range
            .map(|range| range.before.as_slice())
            .unwrap_or(&[]);
        let end_components = component_range
            .map(|range| range.end.as_slice())
            .unwrap_or(&[]);
        let interpolate_components = |progress: f64| {
            let values = before_components
                .iter()
                .zip(end_components)
                .map(|(before, end)| before + (end - before) * progress)
                .collect::<Vec<_>>();
            component_map(metrics, &values)
        };
        segments.push(ReachableEdgeInterval {
            edge_index,
            start_fraction,
            end_fraction,
            start_cost,
            end_cost,
            midpoint_cost,
            start_components: interpolate_components(start_progress),
            end_components: interpolate_components(end_progress),
        });
    }
    normalize_service_area_segments(segments)
}

fn difference_service_area_intervals(
    current: Vec<ReachableEdgeInterval>,
    previous: Vec<ReachableEdgeInterval>,
) -> Vec<ReachableEdgeInterval> {
    let mut previous_by_edge = BTreeMap::<usize, Vec<ReachableEdgeInterval>>::new();
    for interval in previous {
        previous_by_edge
            .entry(interval.edge_index)
            .or_default()
            .push(interval);
    }

    let mut ring = Vec::new();
    for interval in current {
        let mut start = interval.start_fraction;
        let mut start_cost = interval.start_cost;
        let mut start_components = interval.start_components.clone();
        if let Some(previous_intervals) = previous_by_edge.get(&interval.edge_index) {
            for previous in previous_intervals {
                if previous.end_fraction <= start + f64::EPSILON {
                    continue;
                }
                if previous.end_fraction > start {
                    start = previous.end_fraction;
                    start_cost = previous.end_cost;
                    start_components = previous.end_components.clone();
                }
            }
        }
        if interval.end_fraction > start + f64::EPSILON {
            ring.push(ReachableEdgeInterval {
                start_fraction: start,
                start_cost,
                start_components,
                ..interval
            });
        }
    }

    normalize_service_area_segments(ring)
}

fn normalize_service_area_segments(
    mut segments: Vec<ReachableEdgeInterval>,
) -> Vec<ReachableEdgeInterval> {
    segments.sort_by(|left, right| {
        left.edge_index
            .cmp(&right.edge_index)
            .then_with(|| left.start_fraction.total_cmp(&right.start_fraction))
            .then_with(|| left.end_fraction.total_cmp(&right.end_fraction))
    });

    let mut merged: Vec<ReachableEdgeInterval> = Vec::new();
    for segment in segments {
        if let Some(previous) = merged.last_mut()
            && previous.edge_index == segment.edge_index
            && segment.start_fraction <= previous.end_fraction + 1e-9
        {
            if segment.end_fraction > previous.end_fraction {
                previous.end_fraction = segment.end_fraction;
                previous.end_cost = segment.end_cost;
                previous.end_components = segment.end_components.clone();
            }
            previous.midpoint_cost = previous.midpoint_cost.min(segment.midpoint_cost);
            continue;
        }
        merged.push(segment);
    }
    merged
}

fn summarize_service_area_segments(
    topology: &TopologyBundle,
    segments: &[ReachableEdgeInterval],
    include_attributes: bool,
) -> (Option<f64>, Option<u64>) {
    if !include_attributes {
        return (None, None);
    }
    let length = segments
        .iter()
        .map(|segment| {
            topology.routing_edge(segment.edge_index).length_m as f64
                * (segment.end_fraction - segment.start_fraction)
        })
        .sum::<f64>();
    let edge_count = segments
        .iter()
        .map(|segment| segment.edge_index)
        .collect::<std::collections::BTreeSet<_>>()
        .len() as u64;
    (Some(length), Some(edge_count))
}

fn merge_service_area_bands(bands: Vec<ServiceAreaOriginBand>) -> Vec<ServiceAreaOriginBand> {
    let mut merged = BTreeMap::<
        (
            Option<String>,
            Option<String>,
            u64,
            ServiceAreaThresholdMetric,
        ),
        Vec<ServiceAreaOriginBand>,
    >::new();
    for band in bands {
        let key = (
            band.threshold_id.clone(),
            band.band_start_limit
                .map(f64::to_bits)
                .map(|bits| bits.to_string()),
            band.threshold_limit.to_bits(),
            band.threshold_metric,
        );
        merged.entry(key).or_default().push(band);
    }

    merged
        .into_values()
        .map(|group| {
            let has_multiple_origins = group
                .iter()
                .map(|band| &band.origin_id)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                > 1;
            let mut segments = Vec::new();
            let mut fallback_used = false;
            let mut origin_hop_distance_m = None;
            let threshold_id = group[0].threshold_id.clone();
            let band_start_limit = group[0].band_start_limit;
            let threshold_limit = group[0].threshold_limit;
            let threshold_metric = group[0].threshold_metric;
            for band in group {
                fallback_used |= band.fallback_used;
                origin_hop_distance_m = origin_hop_distance_m.or(band.origin_hop_distance_m);
                segments.extend(band.segments);
            }
            let mut segments = normalize_service_area_segments(segments);
            if has_multiple_origins {
                // A union band intentionally erases origin/path ownership.
                // There is no single cumulative vector for its segments.
                for segment in &mut segments {
                    segment.start_components.clear();
                    segment.end_components.clear();
                }
            }
            ServiceAreaOriginBand {
                origin_id: String::new(),
                origin_component_id: None,
                fallback_used,
                origin_hop_distance_m,
                threshold_id,
                band_start_limit,
                threshold_limit,
                threshold_metric,
                segments,
            }
        })
        .collect()
}

fn cut_service_area_bands(bands: Vec<ServiceAreaOriginBand>) -> Vec<ServiceAreaOriginBand> {
    let mut groups = BTreeMap::<
        (
            Option<String>,
            Option<String>,
            u64,
            ServiceAreaThresholdMetric,
        ),
        Vec<ServiceAreaOriginBand>,
    >::new();
    for band in bands {
        let key = (
            band.threshold_id.clone(),
            band.band_start_limit
                .map(f64::to_bits)
                .map(|bits| bits.to_string()),
            band.threshold_limit.to_bits(),
            band.threshold_metric,
        );
        groups.entry(key).or_default().push(band);
    }

    let mut cut = Vec::new();
    for group in groups.into_values() {
        let mut per_origin = BTreeMap::<String, ServiceAreaOriginBand>::new();
        let mut winners = BTreeMap::<usize, (String, ReachableEdgeInterval)>::new();

        for band in group {
            let entry =
                per_origin
                    .entry(band.origin_id.clone())
                    .or_insert_with(|| ServiceAreaOriginBand {
                        origin_id: band.origin_id.clone(),
                        origin_component_id: band.origin_component_id,
                        fallback_used: band.fallback_used,
                        origin_hop_distance_m: band.origin_hop_distance_m,
                        threshold_id: band.threshold_id.clone(),
                        band_start_limit: band.band_start_limit,
                        threshold_limit: band.threshold_limit,
                        threshold_metric: band.threshold_metric,
                        segments: Vec::new(),
                    });
            entry.fallback_used |= band.fallback_used;
            entry.origin_hop_distance_m =
                entry.origin_hop_distance_m.or(band.origin_hop_distance_m);

            for segment in band.segments {
                let winner = winners
                    .entry(segment.edge_index)
                    .or_insert_with(|| (band.origin_id.clone(), segment.clone()));
                if segment.midpoint_cost + f64::EPSILON < winner.1.midpoint_cost
                    || ((segment.midpoint_cost - winner.1.midpoint_cost).abs() <= 1e-9
                        && band.origin_id < winner.0)
                {
                    *winner = (band.origin_id.clone(), segment.clone());
                }
            }
        }

        for (edge_index, (origin_id, winner_segment)) in winners {
            if let Some(band) = per_origin.get_mut(&origin_id) {
                band.segments.push(ReachableEdgeInterval {
                    edge_index,
                    ..winner_segment
                });
            }
        }

        cut.extend(per_origin.into_values().map(|mut band| {
            band.segments = normalize_service_area_segments(band.segments);
            band
        }));
    }

    cut
}

fn build_service_area_features(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    request: &ServiceAreaRequest,
    bands: &[ServiceAreaOriginBand],
) -> Vec<ServiceAreaFeature> {
    let mut features = Vec::new();
    for band in bands {
        if matches!(request.band_mode, ServiceAreaBandMode::Unbanded) {
            if matches!(
                request.output_mode,
                ServiceAreaOutputMode::Network | ServiceAreaOutputMode::Both
            ) {
                features.extend(
                    service_area_segments_for_band(topology, metrics, request, band)
                        .into_iter()
                        .map(service_area_segment_feature),
                );
            }
            continue;
        }

        let (reachable_network_length_m, reachable_edge_count) =
            summarize_service_area_segments(topology, &band.segments, request.returns.attributes);
        let origin_id = (!band.origin_id.is_empty()).then_some(band.origin_id.clone());

        if matches!(
            request.output_mode,
            ServiceAreaOutputMode::Network | ServiceAreaOutputMode::Both
        ) {
            features.push(ServiceAreaFeature {
                origin_id: origin_id.clone(),
                band_start_limit: band.band_start_limit,
                threshold_id: band.threshold_id.clone(),
                threshold_limit: band.threshold_limit,
                threshold_metric: band.threshold_metric,
                geometry_type: ServiceAreaGeometryType::Network,
                fallback_used: band.fallback_used,
                origin_component_id: band.origin_component_id,
                origin_hop_distance_m: band.origin_hop_distance_m,
                reachable_network_length_m,
                reachable_edge_count,
                edge_id: None,
                edge_index: None,
                source_way_id: None,
                from_node_id: None,
                to_node_id: None,
                start_fraction: None,
                end_fraction: None,
                start_cost: None,
                end_cost: None,
                segment_distance_m: None,
                segment_travel_time_s: None,
                start_components: BTreeMap::new(),
                end_components: BTreeMap::new(),
                geometry: request
                    .returns
                    .geometry
                    .then(|| service_area_network_geometry(topology, &band.segments)),
            });
        }

        if matches!(
            request.output_mode,
            ServiceAreaOutputMode::Polygon | ServiceAreaOutputMode::Both
        ) {
            features.push(ServiceAreaFeature {
                origin_id,
                band_start_limit: band.band_start_limit,
                threshold_id: band.threshold_id.clone(),
                threshold_limit: band.threshold_limit,
                threshold_metric: band.threshold_metric,
                geometry_type: ServiceAreaGeometryType::Polygon,
                fallback_used: band.fallback_used,
                origin_component_id: band.origin_component_id,
                origin_hop_distance_m: band.origin_hop_distance_m,
                reachable_network_length_m,
                reachable_edge_count,
                edge_id: None,
                edge_index: None,
                source_way_id: None,
                from_node_id: None,
                to_node_id: None,
                start_fraction: None,
                end_fraction: None,
                start_cost: None,
                end_cost: None,
                segment_distance_m: None,
                segment_travel_time_s: None,
                start_components: BTreeMap::new(),
                end_components: BTreeMap::new(),
                geometry: request.returns.geometry.then(|| {
                    service_area_polygon_geometry(topology, &band.segments, &request.polygon)
                }),
            });
        }
    }

    features
}

fn build_service_area_segments(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    request: &ServiceAreaRequest,
    bands: &[ServiceAreaOriginBand],
) -> Vec<ServiceAreaSegment> {
    bands
        .iter()
        .flat_map(|band| service_area_segments_for_band(topology, metrics, request, band))
        .collect()
}

fn service_area_segments_for_band(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    request: &ServiceAreaRequest,
    band: &ServiceAreaOriginBand,
) -> Vec<ServiceAreaSegment> {
    band.segments
        .iter()
        .map(|segment| {
            let edge = topology.routing_edge(segment.edge_index);
            let full_distance_m = edge.length_m as f64;
            let segment_distance_m =
                full_distance_m * (segment.end_fraction - segment.start_fraction);
            let segment_travel_time_s = metrics.edge_metrics[segment.edge_index]
                .travel_time_s
                .map(|time_s| time_s * (segment.end_fraction - segment.start_fraction));
            ServiceAreaSegment {
                origin_id: (!band.origin_id.is_empty()).then_some(band.origin_id.clone()),
                band_start_limit: band.band_start_limit,
                threshold_id: band.threshold_id.clone(),
                threshold_limit: band.threshold_limit,
                threshold_metric: band.threshold_metric,
                edge_id: edge.edge_id.0,
                edge_index: segment.edge_index as u32,
                source_way_id: edge.source_way_id,
                from_node_id: edge.from.0,
                to_node_id: edge.to.0,
                start_fraction: segment.start_fraction,
                end_fraction: segment.end_fraction,
                start_cost: segment.start_cost,
                end_cost: segment.end_cost,
                segment_distance_m,
                segment_travel_time_s,
                fallback_used: band.fallback_used,
                origin_component_id: band.origin_component_id,
                origin_hop_distance_m: band.origin_hop_distance_m,
                start_components: segment.start_components.clone(),
                end_components: segment.end_components.clone(),
                geometry: request.returns.geometry.then(|| {
                    service_area_network_geometry(topology, std::slice::from_ref(segment))
                }),
            }
        })
        .collect()
}

fn service_area_segment_feature(segment: ServiceAreaSegment) -> ServiceAreaFeature {
    ServiceAreaFeature {
        origin_id: segment.origin_id,
        band_start_limit: segment.band_start_limit,
        threshold_id: segment.threshold_id,
        threshold_limit: segment.threshold_limit,
        threshold_metric: segment.threshold_metric,
        geometry_type: ServiceAreaGeometryType::Segment,
        fallback_used: segment.fallback_used,
        origin_component_id: segment.origin_component_id,
        origin_hop_distance_m: segment.origin_hop_distance_m,
        reachable_network_length_m: Some(segment.segment_distance_m),
        reachable_edge_count: Some(1),
        edge_id: Some(segment.edge_id),
        edge_index: Some(segment.edge_index),
        source_way_id: Some(segment.source_way_id),
        from_node_id: Some(segment.from_node_id),
        to_node_id: Some(segment.to_node_id),
        start_fraction: Some(segment.start_fraction),
        end_fraction: Some(segment.end_fraction),
        start_cost: Some(segment.start_cost),
        end_cost: Some(segment.end_cost),
        segment_distance_m: Some(segment.segment_distance_m),
        segment_travel_time_s: segment.segment_travel_time_s,
        start_components: segment.start_components,
        end_components: segment.end_components,
        geometry: segment.geometry,
    }
}

fn service_area_result_outcome(
    processed_origin_count: usize,
    skipped_origin_count: usize,
    fallback_origin_count: usize,
) -> AnalysisOutcome {
    if processed_origin_count == 0 {
        AnalysisOutcome::Unreachable
    } else if skipped_origin_count > 0 {
        AnalysisOutcome::Partial
    } else if fallback_origin_count > 0 {
        AnalysisOutcome::Degraded
    } else {
        AnalysisOutcome::Legal
    }
}

fn service_area_network_geometry(
    topology: &TopologyBundle,
    segments: &[ReachableEdgeInterval],
) -> serde_json::Value {
    let coordinates = segments
        .iter()
        .map(|segment| service_area_segment_coords(topology, segment))
        .collect::<Vec<_>>();
    serde_json::json!({
        "type": "MultiLineString",
        "coordinates": coordinates,
    })
}

fn service_area_polygon_geometry(
    topology: &TopologyBundle,
    segments: &[ReachableEdgeInterval],
    options: &ServiceAreaPolygonOptions,
) -> serde_json::Value {
    // Trace a concave boundary around the reachable network on a metric
    // grid instead of a convex hull: the polygon hugs the roads that are
    // actually reachable, and enclosed unreachable pockets (water,
    // restricted areas, missing network) become holes.
    let cell_size_m = options
        .cell_size_m
        .unwrap_or(25.0 * options.hull_aggressiveness.max(0.25))
        .max(10.0);
    let tolerance = options.simplification_tolerance_m.unwrap_or(0.0);

    let subsegments = segments
        .iter()
        .map(|segment| {
            let coords = service_area_segment_coords_2d(topology, segment);
            (coords[0], coords[1])
        })
        .collect::<Vec<_>>();
    let polygons =
        crate::isochrone_polygon::trace_reachable_polygons(&subsegments, cell_size_m, tolerance);

    if polygons.is_empty() {
        return serde_json::json!({
            "type": "MultiPolygon",
            "coordinates": Vec::<Vec<Vec<[f64; 2]>>>::new(),
        });
    }

    let rings_of = |polygon: &crate::isochrone_polygon::TracedPolygon| -> Vec<Vec<[f64; 2]>> {
        let mut rings = vec![polygon.exterior.clone()];
        rings.extend(polygon.holes.iter().cloned());
        rings
    };

    if polygons.len() == 1 {
        serde_json::json!({
            "type": "Polygon",
            "coordinates": rings_of(&polygons[0]),
        })
    } else {
        serde_json::json!({
            "type": "MultiPolygon",
            "coordinates": polygons.iter().map(rings_of).collect::<Vec<_>>(),
        })
    }
}

fn service_area_segment_coords(
    topology: &TopologyBundle,
    segment: &ReachableEdgeInterval,
) -> Vec<[f64; 3]> {
    let edge = topology.routing_edge(segment.edge_index);
    let from_node = &topology.nodes[edge.from.0 as usize];
    let to_node = &topology.nodes[edge.to.0 as usize];
    let from = [
        from_node.lon,
        from_node.lat,
        if from_node.z.is_finite() {
            from_node.z
        } else {
            0.0
        },
    ];
    let to = [
        to_node.lon,
        to_node.lat,
        if to_node.z.is_finite() {
            to_node.z
        } else {
            0.0
        },
    ];
    vec![
        interpolate_edge_point_3d(from, to, segment.start_fraction),
        interpolate_edge_point_3d(from, to, segment.end_fraction),
    ]
}

fn service_area_segment_coords_2d(
    topology: &TopologyBundle,
    segment: &ReachableEdgeInterval,
) -> Vec<[f64; 2]> {
    service_area_segment_coords(topology, segment)
        .into_iter()
        .map(|[lon, lat, _]| [lon, lat])
        .collect()
}
