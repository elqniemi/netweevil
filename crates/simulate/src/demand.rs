//! Demand generation and agent dispatch: sample origin/destination pairs
//! from the configured distributions, draw departure times and per-agent
//! behavior parameters, then route every agent through the prepared routing
//! engine (parallel, deterministic per agent id).
//!
//! Dispatch is batched: every unique endpoint is snapped once and every
//! unique (origin, destination, alternatives) pair is routed once, so
//! demand with shared destinations or explicit OD pairs scales with the
//! number of unique pairs instead of the number of agents.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::{Result, bail};
use netweevil_profile::{ReturnConfig, ReturnGeometry};
use netweevil_query::{
    AlternativeRouteOptions, LabeledPoint, PreparedRoutingEngine, RouteRequest, RouteResult,
    SnapOptions, SnappedPoint,
};
use rayon::prelude::*;

use crate::rng::SimRng;
use crate::scenario::{
    BehaviorConfig, DepartureConfig, EndpointDistribution, FleetConfig, ZoneEffect,
    default_pcu_for_mode,
};
use crate::zones::{PreparedPolygon, PreparedZone};

/// Fully resolved agent ready for the engine.
#[derive(Debug, Clone)]
pub struct DispatchedAgent {
    pub agent_id: u32,
    pub fleet: u16,
    pub depart_s: f64,
    pub origin: [f64; 2],
    pub destination: [f64; 2],
    /// Edge indices to traverse, in order.
    pub route: Vec<u32>,
    pub desired_speed_mult: f32,
    pub max_speed_mps: f32,
    pub overtake_eagerness: f32,
    pub reroute_eagerness: f32,
    pub jam_speed_threshold: f32,
    pub reroute_cooldown_s: f32,
    pub obey_signals: bool,
    pub no_collision: bool,
    pub pcu: f32,
}

#[derive(Debug, Clone, Default)]
pub struct DispatchReport {
    pub requested: u32,
    pub dispatched: u32,
    pub failed: u32,
    /// First few failure messages, for diagnostics.
    pub sample_errors: Vec<String>,
}

struct AgentDraft {
    agent_id: u32,
    depart_s: f64,
    rng: SimRng,
    overtake: f32,
    reroute: f32,
    jam: f32,
    mult: f32,
    pcu: f32,
    cooldown: f32,
    wants_alternative: bool,
    origin: [f64; 2],
    destination: [f64; 2],
}

/// Quantized coordinate key (1e-7 deg ~ 1 cm) for snap/pair deduplication.
type CoordKey = (i64, i64);

fn coord_key(point: [f64; 2]) -> CoordKey {
    (
        (point[0] * 1e7).round() as i64,
        (point[1] * 1e7).round() as i64,
    )
}

fn jitter(rng: &mut SimRng, mean: f64, heterogeneity: f64, low: f64, high: f64) -> f64 {
    if heterogeneity <= 0.0 {
        return mean.clamp(low, high);
    }
    rng.normal(mean, mean.abs().max(0.05) * heterogeneity)
        .clamp(low, high)
}

fn sample_departure(config: &DepartureConfig, rng: &mut SimRng, sequence_t: &mut f64) -> f64 {
    match config {
        DepartureConfig::Uniform { start_s, end_s } => {
            let end = end_s.max(*start_s + 1e-9);
            rng.uniform(*start_s, end)
        }
        DepartureConfig::Peak { mean_s, std_s } => rng.normal(*mean_s, std_s.max(0.0)).max(0.0),
        DepartureConfig::Instant { at_s } => at_s.max(0.0),
        DepartureConfig::Poisson {
            start_s,
            rate_per_s,
        } => {
            if *sequence_t < *start_s {
                *sequence_t = *start_s;
            }
            *sequence_t += rng.exponential(*rate_per_s);
            *sequence_t
        }
    }
}

fn sample_in_polygon(polygon: &PreparedPolygon, rng: &mut SimRng) -> [f64; 2] {
    for _ in 0..200 {
        let lon = rng.uniform(polygon.min_lon, polygon.max_lon);
        let lat = rng.uniform(polygon.min_lat, polygon.max_lat);
        if polygon.contains(lon, lat) {
            return [lon, lat];
        }
    }
    // Degenerate polygon: fall back to its bbox centre.
    [
        (polygon.min_lon + polygon.max_lon) / 2.0,
        (polygon.min_lat + polygon.max_lat) / 2.0,
    ]
}

enum PreparedDistribution<'z> {
    Bbox([f64; 4]),
    Polygon(PreparedPolygon),
    Points(Vec<([f64; 2], f64)>),
    Zones(Vec<(&'z PreparedZone, f64)>),
}

impl PreparedDistribution<'_> {
    fn sample(&self, rng: &mut SimRng) -> [f64; 2] {
        match self {
            Self::Bbox(bbox) => [rng.uniform(bbox[0], bbox[2]), rng.uniform(bbox[1], bbox[3])],
            Self::Polygon(polygon) => sample_in_polygon(polygon, rng),
            Self::Points(points) => {
                let weights: Vec<f64> = points.iter().map(|(_, w)| *w).collect();
                points[rng.weighted_index(&weights)].0
            }
            Self::Zones(zones) => {
                let weights: Vec<f64> = zones.iter().map(|(_, w)| *w).collect();
                let zone = zones[rng.weighted_index(&weights)].0;
                sample_in_polygon(&zone.polygon, rng)
            }
        }
    }

    /// Random distributions can retry on routing failures; fixed points
    /// should report the failure instead.
    fn retryable(&self) -> bool {
        !matches!(self, Self::Points(points) if points.len() <= 1)
    }
}

fn prepare_distribution<'z>(
    distribution: &EndpointDistribution,
    network_bbox: [f64; 4],
    zones: &'z [PreparedZone],
    for_origin: bool,
    fleet_id: &str,
) -> Result<PreparedDistribution<'z>> {
    Ok(match distribution {
        EndpointDistribution::RandomBounds => PreparedDistribution::Bbox(network_bbox),
        EndpointDistribution::RandomBbox { bbox } => PreparedDistribution::Bbox(*bbox),
        EndpointDistribution::RandomPolygon { polygon } => {
            PreparedDistribution::Polygon(PreparedPolygon::new(polygon.clone()))
        }
        EndpointDistribution::Points { points } => {
            if points.is_empty() {
                bail!("fleet '{fleet_id}': empty point distribution");
            }
            PreparedDistribution::Points(
                points
                    .iter()
                    .map(|point| ([point.lon, point.lat], point.weight.max(0.0)))
                    .collect(),
            )
        }
        EndpointDistribution::Zones { zone_ids } => {
            let mut selected = Vec::new();
            for zone in zones {
                let weight = match (&zone.effect, for_origin) {
                    (ZoneEffect::Spawn { weight }, true) => Some(*weight),
                    (ZoneEffect::Attract { weight }, false) => Some(*weight),
                    // Allow any named zone to act as a sampling area too.
                    _ => None,
                };
                let explicitly_listed = zone_ids.contains(&zone.zone_id);
                if !zone_ids.is_empty() && !explicitly_listed {
                    continue;
                }
                if let Some(weight) = weight {
                    selected.push((zone, weight.max(0.0)));
                } else if explicitly_listed {
                    selected.push((zone, 1.0));
                }
            }
            if selected.is_empty() {
                bail!(
                    "fleet '{fleet_id}': no matching {} zones for zone distribution",
                    if for_origin { "spawn" } else { "attract" }
                );
            }
            PreparedDistribution::Zones(selected)
        }
    })
}

fn behavior_for_agent(
    behavior: &BehaviorConfig,
    speed: &crate::scenario::FleetSpeedConfig,
    pcu_default: f64,
    rng: &mut SimRng,
) -> (f32, f32, f32, f32, f32, f32) {
    let het = behavior.heterogeneity;
    let overtake = jitter(rng, behavior.overtake_eagerness, het, 0.0, 1.0) as f32;
    let reroute = jitter(rng, behavior.reroute_eagerness, het, 0.0, 1.0) as f32;
    let jam = jitter(rng, behavior.jam_speed_threshold, het * 0.5, 0.05, 0.95) as f32;
    let mult = rng
        .normal(speed.speed_multiplier_mean, speed.speed_multiplier_std)
        .clamp(0.5, 2.0) as f32;
    let pcu = speed.pcu.unwrap_or(pcu_default).max(0.01) as f32;
    let cooldown = behavior.reroute_cooldown_s.max(5.0) as f32;
    (overtake, reroute, jam, mult, pcu, cooldown)
}

fn dispatch_request(
    fleet: &FleetConfig,
    route_id: String,
    origin: [f64; 2],
    destination: [f64; 2],
    wants_alternative: bool,
) -> RouteRequest {
    RouteRequest {
        route_id,
        origin: LabeledPoint {
            id: "origin".to_string(),
            lon: origin[0],
            lat: origin[1],
        },
        destination: LabeledPoint {
            id: "destination".to_string(),
            lon: destination[0],
            lat: destination[1],
        },
        snap: SnapOptions::default(),
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: ReturnConfig {
            geometry: ReturnGeometry::None,
            segment_rows: false,
            road_type_breakdown: Vec::new(),
            surface_breakdown: Vec::new(),
            // Cheapest flag that makes the engine return the edge path
            // (geometry/segments stay off).
            penalty_breakdown: true,
            explain_cost_derivation: false,
        },
        alternatives: if wants_alternative {
            AlternativeRouteOptions {
                max_routes: fleet.behavior.max_alternatives.max(1),
                ..AlternativeRouteOptions::default()
            }
        } else {
            AlternativeRouteOptions::default()
        },
    }
}

/// Best route first, then ranked alternatives.
fn candidate_paths(result: &RouteResult) -> Vec<Vec<u32>> {
    let mut paths = Vec::with_capacity(1 + result.alternatives.len());
    if !result.edge_path.is_empty() {
        paths.push(result.edge_path.clone());
    }
    for alternative in &result.alternatives {
        if !alternative.edge_path.is_empty() {
            paths.push(alternative.edge_path.clone());
        }
    }
    paths
}

fn choose_path(paths: &[Vec<u32>], wants_alternative: bool, rng: &mut SimRng) -> Vec<u32> {
    if wants_alternative && paths.len() > 1 {
        // Weight the best route highest, alternatives less.
        let weights: Vec<f64> = (0..paths.len())
            .map(|rank| 1.0 / (rank as f64 + 1.0))
            .collect();
        paths[rng.weighted_index(&weights)].clone()
    } else {
        paths[0].clone()
    }
}

/// Generate and route every agent of a fleet. `id_offset` keeps agent ids
/// globally unique and deterministic across fleets and mid-run spawns.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_fleet(
    fleet: &FleetConfig,
    fleet_index: u16,
    engine: &Arc<PreparedRoutingEngine>,
    network_bbox: [f64; 4],
    zones: &[PreparedZone],
    seed: u64,
    id_offset: u32,
    depart_offset_s: f64,
) -> Result<(Vec<DispatchedAgent>, DispatchReport)> {
    let mode = engine.metrics().mode;
    let pcu_default = default_pcu_for_mode(mode);

    let use_explicit_pairs = !fleet.demand.od_pairs.is_empty();
    let origins = prepare_distribution(
        &fleet.demand.origins,
        network_bbox,
        zones,
        true,
        &fleet.fleet_id,
    )?;
    let destinations = prepare_distribution(
        &fleet.demand.destinations,
        network_bbox,
        zones,
        false,
        &fleet.fleet_id,
    )?;

    // Departure times drawn sequentially so Poisson arrivals stay ordered.
    let mut departure_rng = SimRng::derive(
        seed,
        0xDE9A_0000_0000_0000 ^ ((fleet_index as u64) << 32) ^ id_offset as u64,
    );
    let mut sequence_t = 0.0f64;

    // Phase 1: draw departures, behavior, and first-attempt OD per agent.
    let mut drafts = Vec::with_capacity(fleet.agent_count as usize);
    for index in 0..fleet.agent_count {
        let agent_id = id_offset + index;
        let depart_s = depart_offset_s
            + sample_departure(&fleet.departures, &mut departure_rng, &mut sequence_t).max(0.0);
        let mut rng = SimRng::derive(seed, 0xA9E27 ^ agent_id as u64);
        let (overtake, reroute, jam, mult, pcu, cooldown) =
            behavior_for_agent(&fleet.behavior, &fleet.speed, pcu_default, &mut rng);
        let wants_alternative = rng.chance(fleet.behavior.alternative_route_share);
        let (origin, destination) = if use_explicit_pairs {
            let pair = &fleet.demand.od_pairs[agent_id as usize % fleet.demand.od_pairs.len()];
            (pair.origin, pair.destination)
        } else {
            (origins.sample(&mut rng), destinations.sample(&mut rng))
        };
        drafts.push(AgentDraft {
            agent_id,
            depart_s,
            rng,
            overtake,
            reroute,
            jam,
            mult,
            pcu,
            cooldown,
            wants_alternative,
            origin,
            destination,
        });
    }

    // Phase 2: snap every unique endpoint once (origin and destination
    // snapping differ, so they are cached separately).
    let search_distance_m = SnapOptions::default().max_distance_m;
    let snap_unique = |unique: BTreeMap<CoordKey, [f64; 2]>,
                       is_origin: bool|
     -> HashMap<CoordKey, Result<Vec<SnappedPoint>, String>> {
        unique
            .into_iter()
            .collect::<Vec<_>>()
            .par_iter()
            .map(|(key, point)| {
                let labeled = LabeledPoint {
                    id: if is_origin { "origin" } else { "destination" }.to_string(),
                    lon: point[0],
                    lat: point[1],
                };
                let snapped = engine
                    .snap_route_candidates(&labeled, search_distance_m, is_origin)
                    .map_err(|error| error.to_string());
                (*key, snapped)
            })
            .collect()
    };
    let mut unique_origins: BTreeMap<CoordKey, [f64; 2]> = BTreeMap::new();
    let mut unique_destinations: BTreeMap<CoordKey, [f64; 2]> = BTreeMap::new();
    for draft in &drafts {
        unique_origins.insert(coord_key(draft.origin), draft.origin);
        unique_destinations.insert(coord_key(draft.destination), draft.destination);
    }
    let origin_snaps = snap_unique(unique_origins, true);
    let destination_snaps = snap_unique(unique_destinations, false);

    // Phase 3: route every unique (origin, destination, alternatives) pair
    // once against the shared snap candidates.
    type PairKey = (CoordKey, CoordKey, bool);
    let mut unique_pairs: BTreeMap<PairKey, ([f64; 2], [f64; 2])> = BTreeMap::new();
    for draft in &drafts {
        unique_pairs.insert(
            (
                coord_key(draft.origin),
                coord_key(draft.destination),
                draft.wants_alternative,
            ),
            (draft.origin, draft.destination),
        );
    }
    let pair_routes: HashMap<PairKey, Result<Vec<Vec<u32>>, String>> = unique_pairs
        .into_iter()
        .collect::<Vec<_>>()
        .par_iter()
        .map(|(key, (origin, destination))| {
            let (origin_key, destination_key, wants_alternative) = key;
            let route = (|| -> Result<Vec<Vec<u32>>, String> {
                let origin_candidates = origin_snaps
                    .get(origin_key)
                    .expect("snapped origin present")
                    .as_ref()
                    .map_err(Clone::clone)?;
                let destination_candidates = destination_snaps
                    .get(destination_key)
                    .expect("snapped destination present")
                    .as_ref()
                    .map_err(Clone::clone)?;
                let request = dispatch_request(
                    fleet,
                    format!("sim-{}-batch", fleet.fleet_id),
                    *origin,
                    *destination,
                    *wants_alternative,
                );
                let result = engine
                    .execute_route_between_candidates(
                        &request,
                        origin_candidates,
                        destination_candidates,
                    )
                    .map_err(|error| error.to_string())?;
                let paths = candidate_paths(&result);
                if paths.is_empty() {
                    return Err("route returned no edge path".to_string());
                }
                Ok(paths)
            })();
            (*key, route)
        })
        .collect();

    // Phase 4: assign shared paths; agents on failed pairs retry with
    // fresh samples when the distribution is random.
    let retryable = !use_explicit_pairs && origins.retryable() && destinations.retryable();
    let retry_attempts = if retryable { 5 } else { 0 };
    let max_speed_mps = fleet
        .speed
        .max_speed_kph
        .map(|kph| (kph / 3.6) as f32)
        .unwrap_or(f32::INFINITY);

    let build_agent = |draft: &AgentDraft,
                       origin: [f64; 2],
                       destination: [f64; 2],
                       route: Vec<u32>| DispatchedAgent {
        agent_id: draft.agent_id,
        fleet: fleet_index,
        depart_s: draft.depart_s,
        origin,
        destination,
        route,
        desired_speed_mult: draft.mult,
        max_speed_mps,
        overtake_eagerness: draft.overtake,
        reroute_eagerness: draft.reroute,
        jam_speed_threshold: draft.jam,
        reroute_cooldown_s: draft.cooldown,
        obey_signals: fleet.behavior.obey_traffic_signals,
        no_collision: fleet.behavior.no_collision,
        pcu: draft.pcu,
    };

    let results: Vec<Result<DispatchedAgent, String>> = drafts
        .par_iter_mut()
        .map(|draft| {
            let pair_key = (
                coord_key(draft.origin),
                coord_key(draft.destination),
                draft.wants_alternative,
            );
            let mut failure = match pair_routes.get(&pair_key).expect("routed pair present") {
                Ok(paths) => {
                    let path = choose_path(paths, draft.wants_alternative, &mut draft.rng);
                    return Ok(build_agent(draft, draft.origin, draft.destination, path));
                }
                Err(error) => error.clone(),
            };

            // Cold path: resample and route individually.
            for _ in 0..retry_attempts {
                let origin = origins.sample(&mut draft.rng);
                let destination = destinations.sample(&mut draft.rng);
                let request = dispatch_request(
                    fleet,
                    format!("sim-{}-{}", fleet.fleet_id, draft.agent_id),
                    origin,
                    destination,
                    draft.wants_alternative,
                );
                match engine.execute_route(&request) {
                    Ok(result) => {
                        let paths = candidate_paths(&result);
                        if paths.is_empty() {
                            failure = format!("route '{}' returned no edge path", request.route_id);
                            continue;
                        }
                        let path = choose_path(&paths, draft.wants_alternative, &mut draft.rng);
                        return Ok(build_agent(draft, origin, destination, path));
                    }
                    Err(error) => failure = error.to_string(),
                }
            }
            Err(failure)
        })
        .collect();

    let mut agents = Vec::with_capacity(results.len());
    let mut report = DispatchReport {
        requested: fleet.agent_count,
        ..DispatchReport::default()
    };
    for result in results {
        match result {
            Ok(agent) => {
                report.dispatched += 1;
                agents.push(agent);
            }
            Err(error) => {
                report.failed += 1;
                if report.sample_errors.len() < 5 {
                    report.sample_errors.push(error);
                }
            }
        }
    }
    Ok((agents, report))
}
