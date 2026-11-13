//! Demand allocation: match ride requests to vehicles by network travel
//! time, and price the trips with a surge multiplier from the demand and
//! supply balance of H3 cells.
//!
//! One many-to-many matrix gives the pickup times. The assignment
//! minimizes the total pickup time over the largest possible number of
//! requests, or takes the shortest pickups first for large problems.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use netweevil_h3::{
    SurgeCell, SurgeConfig, SurgeIndex, WeightedCell, aggregate_points, cell_at, compute_surge,
    parse_resolution,
};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::{
    BatchItemStatus, LabeledPoint, OdPair, OdPairsDocument, PointSetDocument,
    PreparedRoutingEngine, SnapOptions,
};

/// Most vehicle and request pairs one dispatch evaluates.
pub const DISPATCH_PAIR_LIMIT: usize = 4_000_000;
/// Largest side of a problem that `auto` solves optimally.
const OPTIMAL_SIDE_LIMIT: usize = 600;
/// Cost of leaving a request without a vehicle; above any sum of pickups.
const UNASSIGNED_COST: f64 = 1e12;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchPoint {
    pub lon: f64,
    pub lat: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchVehicle {
    pub id: String,
    pub lon: f64,
    pub lat: f64,
    /// Product class of the vehicle, such as `standard` or `van`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchRide {
    pub id: String,
    pub pickup: DispatchPoint,
    /// Trip end; needed for trip times and fares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropoff: Option<DispatchPoint>,
    /// Only vehicles of this type serve the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchingStrategy {
    /// `optimal` up to 600 requests or vehicles, `greedy` above.
    #[default]
    Auto,
    /// Serves the most requests, then minimizes the total pickup time.
    Optimal,
    /// Takes the shortest pickup first.
    Greedy,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MatchingConfig {
    pub strategy: MatchingStrategy,
    /// A vehicle farther away than this does not serve a request.
    pub max_pickup_time_s: Option<f64>,
    /// Only vehicles within this many H3 rings of the pickup are
    /// candidates. Unset considers every vehicle.
    pub candidate_rings: Option<u32>,
    /// Resolution of the candidate rings (default 8).
    pub candidate_resolution: Option<u8>,
}

/// Surge inputs. Without `demand`, the pickups are the demand; without
/// `supply`, the vehicles are the supply.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DispatchSurge {
    pub demand: Option<Vec<WeightedCell>>,
    pub supply: Option<Vec<WeightedCell>>,
    pub config: SurgeConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PricingConfig {
    pub base_fare: f64,
    pub per_km: f64,
    pub per_minute: f64,
    pub minimum_fare: f64,
    /// Fare factor per vehicle type; a type without one has factor 1.
    pub vehicle_type_factors: BTreeMap<String, f64>,
}

impl PricingConfig {
    /// Fare of a trip before surge.
    pub fn base_price(&self, distance_m: f64, time_s: f64, vehicle_type: Option<&str>) -> f64 {
        let factor = vehicle_type
            .and_then(|vehicle_type| self.vehicle_type_factors.get(vehicle_type))
            .copied()
            .unwrap_or(1.0);
        ((self.base_fare + self.per_km * distance_m / 1000.0 + self.per_minute * time_s / 60.0)
            * factor)
            .max(self.minimum_fare)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchRequest {
    pub vehicles: Vec<DispatchVehicle>,
    pub requests: Vec<DispatchRide>,
    #[serde(default)]
    pub matching: MatchingConfig,
    #[serde(default)]
    pub surge: Option<DispatchSurge>,
    #[serde(default)]
    pub pricing: Option<PricingConfig>,
    #[serde(default)]
    pub snap: SnapOptions,
}

#[derive(Debug, Clone, Serialize)]
pub struct DispatchAssignment {
    pub request_id: String,
    pub vehicle_id: String,
    pub pickup_time_s: f64,
    pub pickup_distance_m: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trip_time_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trip_distance_m: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surge_multiplier: Option<f64>,
    /// Fare with surge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fare: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DispatchUnassigned {
    pub request_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DispatchSummary {
    pub request_count: usize,
    pub vehicle_count: usize,
    pub assigned_count: usize,
    /// Vehicle and request pairs that the assignment considered.
    pub candidate_pair_count: usize,
    pub strategy: MatchingStrategy,
    pub total_pickup_time_s: f64,
    pub mean_pickup_time_s: Option<f64>,
    pub max_pickup_time_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_fare: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DispatchResult {
    pub summary: DispatchSummary,
    pub assignments: Vec<DispatchAssignment>,
    pub unassigned: Vec<DispatchUnassigned>,
    pub idle_vehicle_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surge: Option<Vec<SurgeCell>>,
}

/// Assigns rows to columns of a cost matrix. `costs[row][column]` is
/// infinite for a pair that must not be matched. Returns the column of
/// each row. `optimal` matches the most rows and then minimizes the total
/// cost; otherwise the cheapest pairs are taken first.
pub fn solve_assignment(costs: &[Vec<f64>], optimal: bool) -> Vec<Option<usize>> {
    let rows = costs.len();
    let columns = costs.first().map_or(0, Vec::len);
    if rows == 0 || columns == 0 {
        return vec![None; rows];
    }
    if !optimal {
        let mut pairs = Vec::new();
        for (row, row_costs) in costs.iter().enumerate() {
            for (column, cost) in row_costs.iter().enumerate() {
                if cost.is_finite() {
                    pairs.push((*cost, row, column));
                }
            }
        }
        pairs.sort_by(|left, right| {
            left.0
                .total_cmp(&right.0)
                .then(left.1.cmp(&right.1))
                .then(left.2.cmp(&right.2))
        });
        let mut assigned = vec![None; rows];
        let mut taken = vec![false; columns];
        for (_, row, column) in pairs {
            if assigned[row].is_none() && !taken[column] {
                assigned[row] = Some(column);
                taken[column] = true;
            }
        }
        return assigned;
    }

    // Shortest augmenting paths with potentials, on a matrix with at least
    // as many columns as rows.
    let transposed = rows > columns;
    let (n, m) = if transposed {
        (columns, rows)
    } else {
        (rows, columns)
    };
    let cost = |row: usize, column: usize| {
        let value = if transposed {
            costs[column][row]
        } else {
            costs[row][column]
        };
        if value.is_finite() {
            value
        } else {
            UNASSIGNED_COST
        }
    };
    let mut row_potential = vec![0.0; n + 1];
    let mut column_potential = vec![0.0; m + 1];
    let mut column_row = vec![0usize; m + 1];
    let mut way = vec![0usize; m + 1];
    for row in 1..=n {
        column_row[0] = row;
        let mut column = 0;
        let mut min_reduced = vec![f64::INFINITY; m + 1];
        let mut used = vec![false; m + 1];
        loop {
            used[column] = true;
            let current_row = column_row[column];
            let mut delta = f64::INFINITY;
            let mut next_column = 0;
            for candidate in 1..=m {
                if used[candidate] {
                    continue;
                }
                let reduced = cost(current_row - 1, candidate - 1)
                    - row_potential[current_row]
                    - column_potential[candidate];
                if reduced < min_reduced[candidate] {
                    min_reduced[candidate] = reduced;
                    way[candidate] = column;
                }
                if min_reduced[candidate] < delta {
                    delta = min_reduced[candidate];
                    next_column = candidate;
                }
            }
            for candidate in 0..=m {
                if used[candidate] {
                    row_potential[column_row[candidate]] += delta;
                    column_potential[candidate] -= delta;
                } else {
                    min_reduced[candidate] -= delta;
                }
            }
            column = next_column;
            if column_row[column] == 0 {
                break;
            }
        }
        while column != 0 {
            let previous = way[column];
            column_row[column] = column_row[previous];
            column = previous;
        }
    }
    let mut assigned = vec![None; rows];
    for (column, &row) in column_row.iter().enumerate().skip(1) {
        if row == 0 {
            continue;
        }
        let (row, column) = if transposed {
            (column - 1, row - 1)
        } else {
            (row - 1, column - 1)
        };
        if costs[row][column].is_finite() {
            assigned[row] = Some(column);
        }
    }
    assigned
}

fn point_set(points: Vec<LabeledPoint>, snap: &SnapOptions) -> PointSetDocument {
    PointSetDocument {
        points,
        snap: snap.clone(),
        connectivity: Default::default(),
        fallback: Default::default(),
        returns: Default::default(),
        alternatives: Default::default(),
        temporal: Default::default(),
    }
}

fn labeled(id: String, lon: f64, lat: f64) -> LabeledPoint {
    LabeledPoint {
        id,
        lon,
        lat,
        z: None,
    }
}

/// Matches requests to vehicles on the network of `engine`.
pub fn execute_dispatch(
    engine: &PreparedRoutingEngine,
    request: &DispatchRequest,
) -> Result<DispatchResult> {
    let vehicles = &request.vehicles;
    let rides = &request.requests;
    for (kind, mut ids) in [
        (
            "vehicle",
            vehicles.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
        ),
        (
            "request",
            rides.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        ),
    ] {
        ids.sort_unstable();
        if let Some(pair) = ids.windows(2).find(|pair| pair[0] == pair[1]) {
            bail!("{kind} id '{}' is used more than once", pair[0]);
        }
    }
    if let Some(limit) = request.matching.max_pickup_time_s
        && !(limit.is_finite() && limit > 0.0)
    {
        bail!("max_pickup_time_s must be positive");
    }

    // Candidate pairs: compatible type, and near enough on the grid.
    let candidate_resolution =
        parse_resolution(request.matching.candidate_resolution.unwrap_or(8))?;
    let vehicles_by_cell = request.matching.candidate_rings.map(|_| {
        let mut by_cell = FxHashMap::<_, Vec<usize>>::default();
        for (index, vehicle) in vehicles.iter().enumerate() {
            if let Some(cell) = cell_at(vehicle.lon, vehicle.lat, candidate_resolution) {
                by_cell.entry(cell).or_default().push(index);
            }
        }
        by_cell
    });
    let compatible = |ride: &DispatchRide, vehicle: &DispatchVehicle| {
        ride.vehicle_type.is_none() || ride.vehicle_type == vehicle.vehicle_type
    };
    let mut candidates = vec![Vec::<usize>::new(); rides.len()];
    for (ride_index, ride) in rides.iter().enumerate() {
        match (&vehicles_by_cell, request.matching.candidate_rings) {
            (Some(by_cell), Some(rings)) => {
                let Some(cell) = cell_at(ride.pickup.lon, ride.pickup.lat, candidate_resolution)
                else {
                    continue;
                };
                for near in cell.grid_disk::<Vec<_>>(rings) {
                    for &vehicle_index in by_cell.get(&near).into_iter().flatten() {
                        if compatible(ride, &vehicles[vehicle_index]) {
                            candidates[ride_index].push(vehicle_index);
                        }
                    }
                }
                candidates[ride_index].sort_unstable();
            }
            _ => candidates[ride_index]
                .extend((0..vehicles.len()).filter(|&index| compatible(ride, &vehicles[index]))),
        }
    }

    // One matrix over the vehicles and requests that have a candidate.
    let mut vehicle_slot = vec![usize::MAX; vehicles.len()];
    let mut used_vehicles = Vec::new();
    for &vehicle_index in candidates.iter().flatten() {
        if vehicle_slot[vehicle_index] == usize::MAX {
            vehicle_slot[vehicle_index] = used_vehicles.len();
            used_vehicles.push(vehicle_index);
        }
    }
    let used_rides = (0..rides.len())
        .filter(|&index| !candidates[index].is_empty())
        .collect::<Vec<_>>();
    if used_vehicles.len().saturating_mul(used_rides.len()) > DISPATCH_PAIR_LIMIT {
        bail!(
            "{} vehicles and {} requests make more than {DISPATCH_PAIR_LIMIT} pairs; set matching.candidate_rings or split the batch",
            used_vehicles.len(),
            used_rides.len()
        );
    }
    let mut costs = vec![vec![f64::INFINITY; used_vehicles.len()]; used_rides.len()];
    let mut distances = vec![vec![0.0; used_vehicles.len()]; used_rides.len()];
    let mut candidate_pair_count = 0;
    if !used_rides.is_empty() {
        let origins = point_set(
            used_vehicles
                .iter()
                .enumerate()
                .map(|(slot, &index)| {
                    labeled(format!("{slot}"), vehicles[index].lon, vehicles[index].lat)
                })
                .collect(),
            &request.snap,
        );
        let destinations = point_set(
            used_rides
                .iter()
                .enumerate()
                .map(|(slot, &index)| {
                    labeled(
                        format!("{slot}"),
                        rides[index].pickup.lon,
                        rides[index].pickup.lat,
                    )
                })
                .collect(),
            &request.snap,
        );
        let matrix = engine.execute_matrix(&origins, &destinations)?;
        let mut is_candidate = vec![vec![false; used_vehicles.len()]; used_rides.len()];
        for (slot, &ride_index) in used_rides.iter().enumerate() {
            for &vehicle_index in &candidates[ride_index] {
                is_candidate[slot][vehicle_slot[vehicle_index]] = true;
            }
        }
        for cell in &matrix.cells {
            let (Ok(vehicle), Ok(ride)) = (
                cell.origin_id.parse::<usize>(),
                cell.destination_id.parse::<usize>(),
            ) else {
                continue;
            };
            if !is_candidate[ride][vehicle] || cell.status != BatchItemStatus::Succeeded {
                continue;
            }
            let Some(time) = cell.total_travel_time_s else {
                continue;
            };
            if request
                .matching
                .max_pickup_time_s
                .is_some_and(|limit| time > limit)
            {
                continue;
            }
            candidate_pair_count += 1;
            costs[ride][vehicle] = time;
            distances[ride][vehicle] = cell.total_distance_m.unwrap_or(0) as f64;
        }
    }

    let strategy = match request.matching.strategy {
        MatchingStrategy::Auto
            if used_rides.len().max(used_vehicles.len()) <= OPTIMAL_SIDE_LIMIT =>
        {
            MatchingStrategy::Optimal
        }
        MatchingStrategy::Auto => MatchingStrategy::Greedy,
        chosen => chosen,
    };
    let solution = solve_assignment(&costs, strategy == MatchingStrategy::Optimal);

    let mut assigned_vehicle = vec![None; rides.len()];
    for (slot, column) in solution.iter().enumerate() {
        if let Some(column) = column {
            assigned_vehicle[used_rides[slot]] = Some((used_vehicles[*column], slot, *column));
        }
    }

    // Trip legs of the assigned requests that have a dropoff.
    let trip_pairs = rides
        .iter()
        .enumerate()
        .filter(|(index, _)| assigned_vehicle[*index].is_some())
        .filter_map(|(index, ride)| {
            let dropoff = ride.dropoff?;
            Some(OdPair {
                pair_id: format!("{index}"),
                origin: labeled("pickup".into(), ride.pickup.lon, ride.pickup.lat),
                destination: labeled("dropoff".into(), dropoff.lon, dropoff.lat),
            })
        })
        .collect::<Vec<_>>();
    let mut trips = FxHashMap::<usize, (f64, f64)>::default();
    if !trip_pairs.is_empty() {
        let result = engine.execute_od(&OdPairsDocument {
            pairs: trip_pairs,
            snap: request.snap.clone(),
            connectivity: Default::default(),
            fallback: Default::default(),
            returns: Default::default(),
            alternatives: Default::default(),
            temporal: Default::default(),
        })?;
        for pair in result.pairs {
            if let (Ok(index), Some(time)) =
                (pair.pair_id.parse::<usize>(), pair.total_travel_time_s)
            {
                trips.insert(index, (time, pair.total_distance_m.unwrap_or(0) as f64));
            }
        }
    }

    let surge = request
        .surge
        .as_ref()
        .map(|surge| -> Result<_> {
            let resolution = parse_resolution(surge.config.resolution.unwrap_or(8))?;
            let counted = |points: Vec<(f64, f64, f64)>| {
                aggregate_points(points, &[resolution])
                    .0
                    .into_iter()
                    .map(|stat| WeightedCell {
                        cell: stat.cell,
                        weight: stat.weight,
                    })
                    .collect::<Vec<_>>()
            };
            let demand = surge.demand.clone().unwrap_or_else(|| {
                counted(
                    rides
                        .iter()
                        .map(|ride| (ride.pickup.lon, ride.pickup.lat, 1.0))
                        .collect(),
                )
            });
            let supply = surge.supply.clone().unwrap_or_else(|| {
                counted(
                    vehicles
                        .iter()
                        .map(|vehicle| (vehicle.lon, vehicle.lat, 1.0))
                        .collect(),
                )
            });
            let mut config = surge.config.clone();
            config.resolution.get_or_insert(u8::from(resolution));
            let cells = compute_surge(&demand, &supply, &config)?;
            let index = SurgeIndex::new(&cells, &config);
            Ok((cells, index))
        })
        .transpose()?;

    let mut assignments = Vec::new();
    let mut unassigned = Vec::new();
    let mut vehicle_used = vec![false; vehicles.len()];
    for (index, ride) in rides.iter().enumerate() {
        let Some((vehicle_index, slot, column)) = assigned_vehicle[index] else {
            let reason = if candidates[index].is_empty() {
                "no candidate vehicle"
            } else if used_rides
                .iter()
                .position(|&used| used == index)
                .is_some_and(|slot| costs[slot].iter().any(|cost| cost.is_finite()))
            {
                "every reachable vehicle serves another request"
            } else {
                "no vehicle reaches the pickup within the limits"
            };
            unassigned.push(DispatchUnassigned {
                request_id: ride.id.clone(),
                reason: reason.to_string(),
            });
            continue;
        };
        vehicle_used[vehicle_index] = true;
        let trip = trips.get(&index).copied();
        let surge_multiplier = surge
            .as_ref()
            .map(|(_, index)| index.multiplier_at(ride.pickup.lon, ride.pickup.lat));
        let fare = request.pricing.as_ref().zip(trip).map(|(pricing, trip)| {
            pricing.base_price(
                trip.1,
                trip.0,
                vehicles[vehicle_index].vehicle_type.as_deref(),
            ) * surge_multiplier.unwrap_or(1.0)
        });
        assignments.push(DispatchAssignment {
            request_id: ride.id.clone(),
            vehicle_id: vehicles[vehicle_index].id.clone(),
            pickup_time_s: costs[slot][column],
            pickup_distance_m: distances[slot][column],
            trip_time_s: trip.map(|trip| trip.0),
            trip_distance_m: trip.map(|trip| trip.1),
            surge_multiplier,
            fare,
        });
    }
    let total_pickup_time_s = assignments.iter().map(|a| a.pickup_time_s).sum::<f64>();
    let fares = assignments
        .iter()
        .filter_map(|a| a.fare)
        .collect::<Vec<_>>();
    Ok(DispatchResult {
        summary: DispatchSummary {
            request_count: rides.len(),
            vehicle_count: vehicles.len(),
            assigned_count: assignments.len(),
            candidate_pair_count,
            strategy,
            total_pickup_time_s,
            mean_pickup_time_s: (!assignments.is_empty())
                .then(|| total_pickup_time_s / assignments.len() as f64),
            max_pickup_time_s: assignments
                .iter()
                .map(|a| a.pickup_time_s)
                .max_by(f64::total_cmp),
            total_fare: (!fares.is_empty()).then(|| fares.iter().sum()),
        },
        assignments,
        unassigned,
        idle_vehicle_ids: vehicles
            .iter()
            .zip(&vehicle_used)
            .filter(|(_, used)| !**used)
            .map(|(vehicle, _)| vehicle.id.clone())
            .collect(),
        surge: surge.map(|(cells, _)| cells),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const X: f64 = f64::INFINITY;

    #[test]
    fn optimal_assignment_minimizes_the_total_and_serves_the_most_rows() {
        // Greedy takes (0, 0) and leaves row 1 with the expensive column.
        let costs = vec![vec![1.0, 2.0], vec![2.0, 10.0]];
        assert_eq!(solve_assignment(&costs, false), [Some(0), Some(1)]);
        assert_eq!(solve_assignment(&costs, true), [Some(1), Some(0)]);

        // Serving both rows wins over the cheapest single pair.
        let costs = vec![vec![1.0, X], vec![2.0, 50.0]];
        assert_eq!(solve_assignment(&costs, true), [Some(0), Some(1)]);
        let costs = vec![vec![1.0, 30.0], vec![2.0, X]];
        assert_eq!(solve_assignment(&costs, true), [Some(1), Some(0)]);
        assert_eq!(solve_assignment(&costs, false), [Some(0), None]);

        // More rows than columns, and rows without any feasible column.
        let costs = vec![vec![5.0], vec![3.0], vec![X]];
        assert_eq!(solve_assignment(&costs, true), [None, Some(0), None]);
        let costs = vec![vec![X, X], vec![4.0, X]];
        assert_eq!(solve_assignment(&costs, true), [None, Some(0)]);
        assert_eq!(solve_assignment(&[], true), Vec::<Option<usize>>::new());
    }

    #[test]
    fn optimal_matches_brute_force_on_small_matrices() {
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..200 {
            let rows = 1 + (next() % 4) as usize;
            let columns = 1 + (next() % 4) as usize;
            let costs = (0..rows)
                .map(|_| {
                    (0..columns)
                        .map(|_| {
                            if next() % 5 == 0 {
                                X
                            } else {
                                (next() % 100) as f64
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            fn best(costs: &[Vec<f64>], row: usize, used: &mut Vec<bool>) -> (usize, f64) {
                if row == costs.len() {
                    return (0, 0.0);
                }
                let mut result = best(costs, row + 1, used);
                for column in 0..used.len() {
                    if used[column] || !costs[row][column].is_finite() {
                        continue;
                    }
                    used[column] = true;
                    let (count, total) = best(costs, row + 1, used);
                    used[column] = false;
                    let candidate = (count + 1, total + costs[row][column]);
                    if candidate.0 > result.0 || (candidate.0 == result.0 && candidate.1 < result.1)
                    {
                        result = candidate;
                    }
                }
                result
            }
            let expected = best(&costs, 0, &mut vec![false; columns]);
            let solution = solve_assignment(&costs, true);
            let mut columns_used = solution.iter().flatten().collect::<Vec<_>>();
            let count = columns_used.len();
            columns_used.sort_unstable();
            columns_used.dedup();
            assert_eq!(columns_used.len(), count, "a column is used twice");
            let total = solution
                .iter()
                .enumerate()
                .filter_map(|(row, column)| Some(costs[row][(*column)?]))
                .sum::<f64>();
            assert_eq!((count, total), expected, "{costs:?}");
        }
    }

    #[test]
    fn pricing_applies_type_factor_and_minimum() {
        let pricing = PricingConfig {
            base_fare: 2.0,
            per_km: 1.5,
            per_minute: 0.25,
            minimum_fare: 6.0,
            vehicle_type_factors: BTreeMap::from([("van".to_string(), 1.5)]),
        };
        assert_eq!(pricing.base_price(4000.0, 600.0, None), 10.5);
        assert_eq!(pricing.base_price(4000.0, 600.0, Some("van")), 15.75);
        assert_eq!(pricing.base_price(500.0, 60.0, Some("standard")), 6.0);
    }
}
