use super::*;

/// A street-network start with travel time already spent before reaching it.
#[derive(Debug, Clone)]
pub struct ServiceAreaSeed {
    pub point: SnappedPoint,
    pub initial_time_s: f64,
}

pub(crate) fn execute_seeded_service_area_with_graph(
    topology: &TopologyBundle,
    metrics: &CompiledProfileBundle,
    graph: &RoutingGraph,
    origin_id: &str,
    seeds: &[ServiceAreaSeed],
    limit: f64,
    reverse: bool,
    returns: &ServiceAreaReturnOptions,
) -> Result<Vec<ServiceAreaFeature>> {
    if !limit.is_finite() || limit < 0.0 {
        bail!("isochrone travel time must be finite and nonnegative");
    }
    let automaton = if reverse {
        &graph.reverse_automaton
    } else {
        &graph.automaton
    };
    let mut best = HashMap::<SearchStateKey, f64>::new();
    let mut heap = BinaryHeap::new();
    let mut intervals = Vec::new();

    // Keep every seed's clipped interval, even if a different seed reaches
    // the edge's far end sooner. Mid-edge starts can cover disjoint spans.
    let mut visit = |edge_index: usize,
                     start_fraction: f64,
                     before: f64,
                     state: usize,
                     best: &mut HashMap<SearchStateKey, f64>,
                     heap: &mut BinaryHeap<State>| {
        if before > limit
            || !graph
                .edge_costs
                .get(edge_index)
                .is_some_and(|cost| cost.is_finite())
        {
            return;
        }
        let Some(edge_time) = service_area_edge_cost(
            topology,
            metrics,
            edge_index,
            ServiceAreaMetricKind::TravelTimeS,
        ) else {
            return;
        };
        let available_fraction = if reverse {
            start_fraction
        } else {
            1.0 - start_fraction
        };
        let fraction = if edge_time > 0.0 {
            available_fraction.min((limit - before) / edge_time)
        } else {
            available_fraction
        };
        let end_time = before + edge_time * fraction;
        if fraction > f64::EPSILON {
            let (start, end, start_cost, end_cost) = if reverse {
                (start_fraction - fraction, start_fraction, end_time, before)
            } else {
                (start_fraction, start_fraction + fraction, before, end_time)
            };
            intervals.push(ReachableEdgeInterval {
                edge_index,
                start_fraction: start,
                end_fraction: end,
                start_cost,
                end_cost,
                midpoint_cost: (start_cost + end_cost) / 2.0,
                start_components: BTreeMap::new(),
                end_components: BTreeMap::new(),
            });
        }
        let cost = before + edge_time * available_fraction;
        let key = SearchStateKey {
            edge_index,
            automaton_state: state,
        };
        if cost <= limit && cost < best.get(&key).copied().unwrap_or(f64::INFINITY) {
            best.insert(key, cost);
            heap.push(State {
                edge_index,
                automaton_state: state,
                cost,
                score: cost,
            });
        }
    };

    for seed in seeds {
        if !seed.initial_time_s.is_finite() || seed.initial_time_s < 0.0 {
            bail!("isochrone seed travel time must be finite and nonnegative");
        }
        let point = &seed.point;
        if let (Some(edge), Some(fraction)) = (point.snapped_edge_id, point.snapped_edge_fraction) {
            visit(
                edge as usize,
                fraction,
                seed.initial_time_s,
                automaton.transition(0, edge as usize),
                &mut best,
                &mut heap,
            );
        } else {
            let edges = if reverse {
                graph.incoming_edges(point.snapped_node_id as usize)
            } else {
                graph.outgoing_edges(point.snapped_node_id as usize)
            };
            for &edge in edges {
                visit(
                    edge as usize,
                    if reverse { 1.0 } else { 0.0 },
                    seed.initial_time_s,
                    automaton.transition(0, edge as usize),
                    &mut best,
                    &mut heap,
                );
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
        if cost
            > best[&SearchStateKey {
                edge_index,
                automaton_state,
            }]
        {
            continue;
        }
        let transitions = if reverse {
            graph.reverse_transition_range(edge_index)
        } else {
            graph.transition_range(edge_index)
        };
        for slot in transitions {
            let next = if reverse {
                graph.reverse_transition_edges[slot]
            } else {
                graph.transition_edges[slot]
            } as usize;
            if !automaton.is_transition_allowed(automaton_state, next) {
                continue;
            }
            let turn = if reverse {
                turn_penalty_seconds(topology, metrics, next, edge_index)
            } else {
                turn_penalty_seconds(topology, metrics, edge_index, next)
            };
            visit(
                next,
                if reverse { 1.0 } else { 0.0 },
                cost + turn,
                automaton.transition(automaton_state, next),
                &mut best,
                &mut heap,
            );
        }
    }
    let intervals = normalize_service_area_segments(intervals);
    if intervals.is_empty() {
        return Ok(Vec::new());
    }
    let bands = vec![ServiceAreaOriginBand {
        origin_id: origin_id.to_string(),
        origin_component_id: None,
        fallback_used: false,
        origin_hop_distance_m: None,
        threshold_id: None,
        band_start_limit: None,
        threshold_limit: limit,
        threshold_metric: ServiceAreaThresholdMetric::TravelTimeS,
        segments: intervals,
    }];
    let request = ServiceAreaRequest {
        analysis_id: origin_id.to_string(),
        origins: Vec::new(),
        thresholds: Vec::new(),
        snap: Default::default(),
        connectivity: Default::default(),
        fallback: Default::default(),
        output_mode: ServiceAreaOutputMode::Both,
        band_mode: ServiceAreaBandMode::Cumulative,
        boundary_mode: ServiceAreaBoundaryMode::CutAtBoundary,
        multi_origin_mode: ServiceAreaMultiOriginMode::Overlap,
        polygon: Default::default(),
        returns: returns.clone(),
        temporal: Default::default(),
    };
    ensure_service_area_output_limits(&request, &bands)?;
    Ok(build_service_area_features(
        topology, metrics, &request, &bands,
    ))
}
