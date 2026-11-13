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

use crate::output::RideSurgePeriod;
use crate::rng::SimRng;
use crate::scenario::{
    BehaviorConfig, DepartureConfig, EndpointDistribution, FleetConfig, RideHailingConfig,
    VehicleTypeConfig, ZoneEffect, default_pcu_for_mode,
};
use crate::zones::{PreparedCells, PreparedPolygon, PreparedZone, ZoneShape};
use netweevil_h3::{CellIndex, WeightedCell};

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
    /// Index into the fleet's `vehicle_types`, or [`NO_VEHICLE_TYPE`].
    pub vehicle_type: u16,
    /// Position in `route` of the first edge after the pickup of a
    /// ride-hailing trip, or [`NO_VIA`].
    pub via_pos: u32,
    /// Time of the ride request; `NaN` for agents that are not rides.
    pub request_s: f64,
}

/// The agent has no vehicle type.
pub const NO_VEHICLE_TYPE: u16 = u16::MAX;
/// The route has no pickup.
pub const NO_VIA: u32 = u32::MAX;

#[derive(Debug, Clone, Default)]
pub struct DispatchReport {
    pub requested: u32,
    pub dispatched: u32,
    pub failed: u32,
    /// First few failure messages, for diagnostics.
    pub sample_errors: Vec<String>,
}

/// Sampled-but-unrouted agent. Planning is cheap (sampling only); routing is
/// the expensive step and happens in departure-time windows during the run.
pub(crate) struct AgentDraft {
    agent_id: u32,
    pub(crate) depart_s: f64,
    rng: SimRng,
    overtake: f32,
    reroute: f32,
    jam: f32,
    mult: f32,
    pcu: f32,
    cooldown: f32,
    wants_alternative: bool,
    pub(crate) origin: [f64; 2],
    pub(crate) destination: [f64; 2],
    vehicle_type: u16,
    max_speed_mps: f32,
    /// Pickup of a ride-hailing trip, between origin and destination.
    via: Option<[f64; 2]>,
    request_s: f64,
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

/// Picks a cell in proportion to the running totals in `cumulative`, then
/// a uniform point inside its hexagon.
fn sample_in_cells(cells: &[CellIndex], cumulative: &[f64], rng: &mut SimRng) -> [f64; 2] {
    let total = cumulative.last().copied().unwrap_or(0.0);
    let draw = rng.uniform(0.0, total);
    let index = cumulative
        .partition_point(|running| *running <= draw)
        .min(cells.len() - 1);
    sample_in_polygon(
        &PreparedPolygon::new(netweevil_h3::cell_ring(cells[index])),
        rng,
    )
}

enum PreparedDistribution<'z> {
    Bbox([f64; 4]),
    Polygon(PreparedPolygon),
    Points(Vec<[f64; 2]>, Vec<f64>),
    Zones(Vec<&'z PreparedZone>, Vec<f64>),
    Cells {
        cells: Vec<CellIndex>,
        cumulative_weight: Vec<f64>,
    },
}

impl PreparedDistribution<'_> {
    fn sample(&self, rng: &mut SimRng) -> [f64; 2] {
        match self {
            Self::Bbox(bbox) => [rng.uniform(bbox[0], bbox[2]), rng.uniform(bbox[1], bbox[3])],
            Self::Polygon(polygon) => sample_in_polygon(polygon, rng),
            Self::Points(points, weights) => points[rng.weighted_index(weights)],
            Self::Zones(zones, weights) => {
                let zone = zones[rng.weighted_index(weights)];
                match &zone.shape {
                    ZoneShape::Polygon(polygon) => sample_in_polygon(polygon, rng),
                    ZoneShape::Cells(PreparedCells {
                        cells,
                        cumulative_area,
                        ..
                    }) => sample_in_cells(cells, cumulative_area, rng),
                }
            }
            Self::Cells {
                cells,
                cumulative_weight,
            } => sample_in_cells(cells, cumulative_weight, rng),
        }
    }

    /// Random distributions can retry on routing failures; fixed points
    /// should report the failure instead.
    fn retryable(&self) -> bool {
        !matches!(self, Self::Points(points, _) if points.len() <= 1)
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
                points.iter().map(|point| [point.lon, point.lat]).collect(),
                points.iter().map(|point| point.weight.max(0.0)).collect(),
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
                if matches!(&zone.shape, ZoneShape::Cells(cells) if cells.cells.is_empty()) {
                    continue;
                }
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
            let (zones, weights) = selected.into_iter().unzip();
            PreparedDistribution::Zones(zones, weights)
        }
        EndpointDistribution::CellLayer { layer_id, .. } => {
            bail!(
                "fleet '{fleet_id}': cell layer '{layer_id}' is not resolved; run the scenario from a workspace"
            );
        }
        EndpointDistribution::Cells { cells } => {
            let mut total = 0.0;
            let mut cumulative_weight = Vec::with_capacity(cells.len());
            for cell in cells {
                total += cell.weight.max(0.0);
                cumulative_weight.push(total);
            }
            if total <= 0.0 {
                bail!("fleet '{fleet_id}': cell distribution has no positive weight");
            }
            PreparedDistribution::Cells {
                cells: cells.iter().map(|cell| cell.cell).collect(),
                cumulative_weight,
            }
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
            z: None,
        },
        destination: LabeledPoint {
            id: "destination".to_string(),
            lon: destination[0],
            lat: destination[1],
            z: None,
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
            admin_areas: false,
            elevation: false,
        },
        alternatives: if wants_alternative {
            AlternativeRouteOptions {
                max_routes: fleet.behavior.max_alternatives.max(1),
                ..AlternativeRouteOptions::default()
            }
        } else {
            AlternativeRouteOptions::default()
        },
        temporal: Default::default(),
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

/// Planning record of a ride-hailing fleet.
#[derive(Debug, Clone, Default)]
pub(crate) struct RidePlan {
    pub(crate) requests: u32,
    pub(crate) vehicles: u32,
    pub(crate) unserved: u32,
    pub(crate) priced_out: u32,
    pub(crate) planned_wait_sum_s: f64,
    pub(crate) surge_sum: f64,
    pub(crate) surge_periods: Vec<RideSurgePeriod>,
}

struct RideVehicle {
    position: [f64; 2],
    free_at_s: f64,
    vehicle_type: u16,
}

/// Draws a vehicle type index by `weight`; [`NO_VEHICLE_TYPE`] without
/// types or without a positive weight.
fn sample_vehicle_type(
    types: &[VehicleTypeConfig],
    weight: impl Fn(&VehicleTypeConfig) -> f64,
    rng: &mut SimRng,
) -> u16 {
    let weights: Vec<f64> = types.iter().map(weight).collect();
    if weights.iter().all(|weight| *weight <= 0.0) {
        return NO_VEHICLE_TYPE;
    }
    rng.weighted_index(&weights) as u16
}

/// Serves ride requests in time order with the vehicle that reaches the
/// pickup first by estimate. A served draft departs from the vehicle's
/// position, picks the rider up at `via` and ends at the dropoff. Requests
/// without a vehicle in time, and requests priced out by surge, are removed.
fn plan_rides(
    fleet: &FleetConfig,
    ride: &RideHailingConfig,
    fleet_index: u16,
    network_bbox: [f64; 4],
    zones: &[PreparedZone],
    seed: u64,
    id_offset: u32,
    drafts: Vec<AgentDraft>,
) -> Result<(Vec<AgentDraft>, RidePlan)> {
    let pool = prepare_distribution(&ride.vehicles, network_bbox, zones, true, &fleet.fleet_id)?;
    let mut vehicles = (0..ride.vehicle_count)
        .map(|index| {
            let mut rng = SimRng::derive(
                seed,
                0x71DE_0000_0000_0000 ^ ((fleet_index as u64) << 32) ^ (id_offset + index) as u64,
            );
            RideVehicle {
                position: pool.sample(&mut rng),
                free_at_s: 0.0,
                vehicle_type: sample_vehicle_type(&fleet.vehicle_types, |t| t.share, &mut rng),
            }
        })
        .collect::<Vec<_>>();
    // Door-to-door estimate: straight line with a detour factor.
    let speed_mps = ride.planning_speed_kph / 3.6;
    let estimate_s = |from: [f64; 2], to: [f64; 2]| {
        1.3 * crate::network::haversine_m(from[0], from[1], to[0], to[1]) / speed_mps
    };
    let typed_requests = fleet.vehicle_types.iter().any(|t| t.request_share > 0.0);

    let mut requests = drafts;
    requests.sort_by(|a, b| {
        a.depart_s
            .total_cmp(&b.depart_s)
            .then(a.agent_id.cmp(&b.agent_id))
    });
    let mut plan = RidePlan {
        requests: requests.len() as u32,
        vehicles: ride.vehicle_count,
        ..RidePlan::default()
    };
    let surge_resolution = ride
        .surge
        .as_ref()
        .map(|surge| netweevil_h3::parse_resolution(surge.config.resolution.unwrap_or(8)))
        .transpose()?;
    let mut period = u64::MAX;
    let mut surge_index: Option<netweevil_h3::SurgeIndex> = None;
    let mut served = Vec::with_capacity(requests.len());
    for index in 0..requests.len() {
        let request_s = requests[index].depart_s;
        if let (Some(surge), Some(resolution)) = (&ride.surge, surge_resolution) {
            let current = (request_s / surge.interval_s).floor().max(0.0) as u64;
            if current != period {
                period = current;
                let start_s = current as f64 * surge.interval_s;
                let end_s = start_s + surge.interval_s;
                let count = |points: &mut dyn Iterator<Item = [f64; 2]>| {
                    netweevil_h3::aggregate_points(
                        points.map(|point| (point[0], point[1], 1.0)),
                        &[resolution],
                    )
                    .0
                    .into_iter()
                    .map(|stat| WeightedCell {
                        cell: stat.cell,
                        weight: stat.weight,
                    })
                    .collect::<Vec<_>>()
                };
                let demand = count(
                    &mut requests[index..]
                        .iter()
                        .take_while(|request| request.depart_s < end_s)
                        .map(|request| request.origin),
                );
                let supply = count(
                    &mut vehicles
                        .iter()
                        .filter(|vehicle| vehicle.free_at_s <= start_s)
                        .map(|vehicle| vehicle.position),
                );
                let mut config = surge.config.clone();
                config.resolution = Some(u8::from(resolution));
                let cells = netweevil_h3::compute_surge(&demand, &supply, &config)?;
                surge_index = Some(netweevil_h3::SurgeIndex::new(&cells, &config));
                plan.surge_periods.push(RideSurgePeriod {
                    start_s,
                    end_s,
                    cells,
                });
            }
        }
        let request = &mut requests[index];
        let pickup = request.origin;
        let dropoff = request.destination;
        let multiplier = surge_index
            .as_ref()
            .map_or(1.0, |surge| surge.multiplier_at(pickup[0], pickup[1]));
        if let Some(surge) = &ride.surge
            && surge.price_elasticity > 0.0
            && request
                .rng
                .chance(1.0 - multiplier.powf(-surge.price_elasticity))
        {
            plan.priced_out += 1;
            continue;
        }
        let wanted = if typed_requests {
            sample_vehicle_type(&fleet.vehicle_types, |t| t.request_share, &mut request.rng)
        } else {
            NO_VEHICLE_TYPE
        };
        let best = vehicles
            .iter()
            .enumerate()
            .filter(|(_, vehicle)| wanted == NO_VEHICLE_TYPE || vehicle.vehicle_type == wanted)
            .map(|(vehicle_index, vehicle)| {
                let start_s = vehicle.free_at_s.max(request_s);
                (
                    start_s + estimate_s(vehicle.position, pickup),
                    vehicle_index,
                )
            })
            .min_by(|left, right| left.0.total_cmp(&right.0).then(left.1.cmp(&right.1)));
        let Some((pickup_s, vehicle_index)) =
            best.filter(|best| best.0 - request_s <= ride.max_wait_s)
        else {
            plan.unserved += 1;
            continue;
        };
        let vehicle = &mut vehicles[vehicle_index];
        request.depart_s = vehicle.free_at_s.max(request_s);
        request.request_s = request_s;
        request.origin = vehicle.position;
        request.via = Some(pickup);
        request.wants_alternative = false;
        apply_vehicle_type(fleet, vehicle.vehicle_type, request);
        vehicle.free_at_s = pickup_s + estimate_s(pickup, dropoff);
        vehicle.position = dropoff;
        plan.planned_wait_sum_s += pickup_s - request_s;
        plan.surge_sum += multiplier;
        served.push(index);
    }
    let mut keep = vec![false; requests.len()];
    for index in served {
        keep[index] = true;
    }
    let mut keep = keep.into_iter();
    requests.retain(|_| keep.next().unwrap_or(false));
    Ok((requests, plan))
}

/// Sets the type of a draft and the pcu, speed and cap that come with it.
fn apply_vehicle_type(fleet: &FleetConfig, vehicle_type: u16, draft: &mut AgentDraft) {
    let Some(config) = fleet.vehicle_types.get(vehicle_type as usize) else {
        return;
    };
    draft.vehicle_type = vehicle_type;
    if let Some(pcu) = config.pcu {
        draft.pcu = pcu.max(0.01) as f32;
    }
    draft.mult = (draft.mult * config.speed_factor as f32).clamp(0.1, 3.0);
    if let Some(kph) = config.max_speed_kph {
        draft.max_speed_mps = (kph / 3.6) as f32;
    }
}

/// Sample departures, behavior, and OD pairs for every agent of a fleet
/// without routing anything. `id_offset` keeps agent ids globally unique and
/// deterministic across fleets and mid-run spawns.
pub(crate) fn plan_fleet(
    fleet: &FleetConfig,
    fleet_index: u16,
    engine: &Arc<PreparedRoutingEngine>,
    network_bbox: [f64; 4],
    zones: &[PreparedZone],
    seed: u64,
    id_offset: u32,
    depart_offset_s: f64,
) -> Result<(Vec<AgentDraft>, Option<RidePlan>)> {
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
    let fleet_max_speed_mps = fleet
        .speed
        .max_speed_kph
        .map(|kph| (kph / 3.6) as f32)
        .unwrap_or(f32::INFINITY);
    // Ride-hailing agents take the type of their vehicle.
    let typed_agents = !fleet.vehicle_types.is_empty() && fleet.ride_hailing.is_none();

    // Departure times drawn sequentially so Poisson arrivals stay ordered.
    let mut departure_rng = SimRng::derive(
        seed,
        0xDE9A_0000_0000_0000 ^ ((fleet_index as u64) << 32) ^ id_offset as u64,
    );
    let mut sequence_t = 0.0f64;

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
        let mut draft = AgentDraft {
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
            vehicle_type: NO_VEHICLE_TYPE,
            max_speed_mps: fleet_max_speed_mps,
            via: None,
            request_s: f64::NAN,
        };
        if typed_agents {
            // A stream of its own, so that types do not shift the other
            // draws of the agent.
            let mut type_rng = SimRng::derive(seed, 0x7E41_C1E5_0000_0000 ^ agent_id as u64);
            let vehicle_type =
                sample_vehicle_type(&fleet.vehicle_types, |t| t.share, &mut type_rng);
            apply_vehicle_type(fleet, vehicle_type, &mut draft);
        }
        drafts.push(draft);
    }
    match &fleet.ride_hailing {
        Some(ride) => {
            let (drafts, plan) = plan_rides(
                fleet,
                ride,
                fleet_index,
                network_bbox,
                zones,
                seed,
                id_offset,
                drafts,
            )?;
            Ok((drafts, Some(plan)))
        }
        None => Ok((drafts, None)),
    }
}

/// Generate and route every agent of a fleet in one call (used for mid-run
/// fleet spawns; the initial dispatch routes drafts lazily in departure
/// windows instead).
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
    let (mut drafts, _) = plan_fleet(
        fleet,
        fleet_index,
        engine,
        network_bbox,
        zones,
        seed,
        id_offset,
        depart_offset_s,
    )?;
    route_drafts(fleet, fleet_index, engine, network_bbox, zones, &mut drafts)
}

/// Route a batch of drafts (phases 2-4 of dispatch): snap unique endpoints,
/// route unique (origin, destination, alternatives) pairs, then assign paths.
#[allow(clippy::too_many_arguments)]
pub(crate) fn route_drafts(
    fleet: &FleetConfig,
    fleet_index: u16,
    engine: &Arc<PreparedRoutingEngine>,
    network_bbox: [f64; 4],
    zones: &[PreparedZone],
    drafts: &mut [AgentDraft],
) -> Result<(Vec<DispatchedAgent>, DispatchReport)> {
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
                    z: None,
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
    for draft in drafts.iter() {
        unique_origins.insert(coord_key(draft.origin), draft.origin);
        unique_destinations.insert(coord_key(draft.destination), draft.destination);
        if let Some(via) = draft.via {
            unique_origins.insert(coord_key(via), via);
            unique_destinations.insert(coord_key(via), via);
        }
    }
    let origin_snaps = snap_unique(unique_origins, true);
    let destination_snaps = snap_unique(unique_destinations, false);

    // Phase 3: route every unique (origin, destination, alternatives) pair
    // once against the shared snap candidates.
    type PairKey = (CoordKey, CoordKey, bool);
    let mut unique_pairs: BTreeMap<PairKey, ([f64; 2], [f64; 2])> = BTreeMap::new();
    for draft in drafts.iter() {
        match draft.via {
            Some(via) => {
                for (from, to) in [(draft.origin, via), (via, draft.destination)] {
                    unique_pairs.insert((coord_key(from), coord_key(to), false), (from, to));
                }
            }
            None => {
                unique_pairs.insert(
                    (
                        coord_key(draft.origin),
                        coord_key(draft.destination),
                        draft.wants_alternative,
                    ),
                    (draft.origin, draft.destination),
                );
            }
        }
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
        max_speed_mps: draft.max_speed_mps,
        overtake_eagerness: draft.overtake,
        reroute_eagerness: draft.reroute,
        jam_speed_threshold: draft.jam,
        reroute_cooldown_s: draft.cooldown,
        obey_signals: fleet.behavior.obey_traffic_signals,
        no_collision: fleet.behavior.no_collision,
        pcu: draft.pcu,
        vehicle_type: draft.vehicle_type,
        via_pos: NO_VIA,
        request_s: draft.request_s,
    };

    let results: Vec<Result<DispatchedAgent, String>> = drafts
        .par_iter_mut()
        .map(|draft| {
            if let Some(via) = draft.via {
                // Two legs joined at the pickup; no resampling for rides.
                let leg = |from: [f64; 2], to: [f64; 2]| {
                    pair_routes
                        .get(&(coord_key(from), coord_key(to), false))
                        .expect("routed leg present")
                        .as_ref()
                        .map(|paths| &paths[0])
                        .map_err(Clone::clone)
                };
                let to_pickup = leg(draft.origin, via)?;
                let trip = leg(via, draft.destination)?;
                let shared_edge = to_pickup.last() == trip.first();
                let mut route = to_pickup.clone();
                route.extend_from_slice(&trip[usize::from(shared_edge)..]);
                let via_pos = to_pickup.len() as u32;
                let mut agent = build_agent(draft, draft.origin, draft.destination, route);
                if (via_pos as usize) < agent.route.len() {
                    agent.via_pos = via_pos;
                }
                return Ok(agent);
            }
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
        requested: drafts.len() as u32,
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

#[cfg(test)]
mod tests {
    use netweevil_h3::{WeightedCell, cell_at, parse_resolution};

    use super::*;
    use crate::scenario::SimulationScenario;

    #[test]
    fn cell_distribution_samples_by_weight_inside_the_cells() {
        let resolution = parse_resolution(8).unwrap();
        let heavy = cell_at(6.5665, 53.2194, resolution).unwrap();
        let light = cell_at(6.70, 53.25, resolution).unwrap();
        let empty = cell_at(6.40, 53.10, resolution).unwrap();
        let yaml = format!(
            r#"
scenario: {{id: cells}}
fleets:
  - fleet_id: commuters
    profile_id: car
    agent_count: 10
    demand:
      origins:
        kind: cells
        cells:
          - {{cell: {heavy}, weight: 3}}
          - {{cell: {light}}}
          - {{cell: {empty}, weight: 0}}
"#
        );
        let scenario: SimulationScenario = serde_yaml::from_str(&yaml).unwrap();
        scenario.validate().unwrap();
        let distribution = prepare_distribution(
            &scenario.fleets[0].demand.origins,
            [0.0, 0.0, 1.0, 1.0],
            &[],
            true,
            "commuters",
        )
        .unwrap();
        let mut rng = SimRng::seeded(7);
        let mut in_heavy = 0;
        for _ in 0..4000 {
            let [lon, lat] = distribution.sample(&mut rng);
            let cell = cell_at(lon, lat, resolution).unwrap();
            assert!(cell == heavy || cell == light, "sampled outside the cells");
            in_heavy += u32::from(cell == heavy);
        }
        assert!((2850..3150).contains(&in_heavy), "{in_heavy} of 4000");

        let weightless = EndpointDistribution::Cells {
            cells: vec![WeightedCell {
                cell: heavy,
                weight: 0.0,
            }],
        };
        assert!(prepare_distribution(&weightless, [0.0; 4], &[], true, "f").is_err());
    }
}
