//! Demand and supply balance per cell, in the style of ride-hailing surge
//! pricing: both sides are smoothed over neighbouring cells, and a cell with
//! more demand than supply gets a price multiplier above one. The same
//! balance drives a rebalancing plan that moves idle supply towards unmet
//! demand.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use h3o::{CellIndex, Resolution};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::io::{WeightedCell, deserialize_cell, serialize_cell};
use crate::{CellMap, parse_resolution};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SurgeConfig {
    /// Resolution of the surge cells. Defaults to the coarsest resolution
    /// of the inputs.
    pub resolution: Option<u8>,
    /// Rings of neighbours that demand and supply spread over.
    pub smoothing_rings: u32,
    /// Share of a ring's weight relative to the ring inside it (0 to 1).
    pub ring_decay: f64,
    /// Demand per unit of supply at which the multiplier starts to rise.
    pub balance_ratio: f64,
    /// Multiplier increase per unit of relative excess demand.
    pub sensitivity: f64,
    pub min_multiplier: f64,
    pub max_multiplier: f64,
    /// Multipliers round to this step; zero keeps them exact.
    pub step: f64,
    /// Lower bound of the smoothed supply in the ratio, so that a cell
    /// without supply has a finite ratio.
    pub min_supply: f64,
}

impl Default for SurgeConfig {
    fn default() -> Self {
        Self {
            resolution: None,
            smoothing_rings: 1,
            ring_decay: 0.5,
            balance_ratio: 1.0,
            sensitivity: 0.5,
            min_multiplier: 1.0,
            max_multiplier: 3.0,
            step: 0.1,
            min_supply: 1.0,
        }
    }
}

impl SurgeConfig {
    pub fn validate(&self) -> Result<()> {
        if let Some(resolution) = self.resolution {
            parse_resolution(resolution)?;
        }
        if self.smoothing_rings > 10 {
            bail!("smoothing_rings must be at most 10");
        }
        let positive = |name: &str, value: f64| {
            if value.is_finite() && value > 0.0 {
                Ok(())
            } else {
                bail!("{name} must be positive")
            }
        };
        positive("balance_ratio", self.balance_ratio)?;
        positive("min_multiplier", self.min_multiplier)?;
        positive("min_supply", self.min_supply)?;
        if !(self.ring_decay.is_finite() && (0.0..=1.0).contains(&self.ring_decay)) {
            bail!("ring_decay must be between 0 and 1");
        }
        if !(self.sensitivity.is_finite() && self.sensitivity >= 0.0) {
            bail!("sensitivity must not be negative");
        }
        if !(self.max_multiplier.is_finite() && self.max_multiplier >= self.min_multiplier) {
            bail!("max_multiplier must be at least min_multiplier");
        }
        if !(self.step.is_finite() && self.step >= 0.0) {
            bail!("step must not be negative");
        }
        Ok(())
    }

    /// The multiplier for a demand to supply ratio.
    pub fn multiplier(&self, ratio: f64) -> f64 {
        let raw = 1.0 + self.sensitivity * (ratio / self.balance_ratio - 1.0);
        let stepped = if self.step > 0.0 {
            (raw / self.step).round() * self.step
        } else {
            raw
        };
        stepped.clamp(self.min_multiplier, self.max_multiplier)
    }
}

/// The balance of one cell.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SurgeCell {
    #[serde(
        serialize_with = "serialize_cell",
        deserialize_with = "deserialize_cell"
    )]
    pub cell: CellIndex,
    pub demand: f64,
    pub supply: f64,
    pub smoothed_demand: f64,
    pub smoothed_supply: f64,
    /// Smoothed demand per unit of smoothed supply.
    pub ratio: f64,
    pub multiplier: f64,
}

fn at_resolution(cells: &[WeightedCell], resolution: Resolution) -> FxHashMap<CellIndex, f64> {
    let mut out = FxHashMap::default();
    for row in cells {
        if !(row.weight.is_finite() && row.weight > 0.0) {
            continue;
        }
        if let Some(parent) = row.cell.parent(resolution) {
            *out.entry(parent).or_default() += row.weight;
        } else {
            // A coarser cell counts at its centre.
            if let Some(child) = row.cell.center_child(resolution) {
                *out.entry(child).or_default() += row.weight;
            }
        }
    }
    out
}

fn smooth(values: &FxHashMap<CellIndex, f64>, rings: u32, decay: f64) -> FxHashMap<CellIndex, f64> {
    if rings == 0 {
        return values.clone();
    }
    let mut out = FxHashMap::default();
    for (cell, value) in values {
        let disk = cell.grid_disk_distances::<Vec<_>>(rings);
        let total = disk
            .iter()
            .map(|(_, distance)| decay.powi(*distance as i32))
            .sum::<f64>();
        for (neighbour, distance) in disk {
            *out.entry(neighbour).or_default() += value * decay.powi(distance as i32) / total;
        }
    }
    out
}

/// Computes the balance of every cell that has demand or supply, ordered by
/// cell. Weights that are not positive are ignored. A cell coarser than the
/// surge resolution counts at its centre.
pub fn compute_surge(
    demand: &[WeightedCell],
    supply: &[WeightedCell],
    config: &SurgeConfig,
) -> Result<Vec<SurgeCell>> {
    config.validate()?;
    let resolution = match config.resolution {
        Some(resolution) => parse_resolution(resolution)?,
        None => match demand
            .iter()
            .chain(supply)
            .map(|row| row.cell.resolution())
            .min()
        {
            Some(resolution) => resolution,
            None => return Ok(Vec::new()),
        },
    };
    let demand = at_resolution(demand, resolution);
    let supply = at_resolution(supply, resolution);
    let smoothed_demand = smooth(&demand, config.smoothing_rings, config.ring_decay);
    let smoothed_supply = smooth(&supply, config.smoothing_rings, config.ring_decay);
    let mut cells = BTreeMap::new();
    for cell in demand.keys().chain(supply.keys()) {
        cells.entry(*cell).or_insert_with(|| {
            let value =
                |values: &FxHashMap<CellIndex, f64>| values.get(cell).copied().unwrap_or(0.0);
            let ratio = value(&smoothed_demand) / value(&smoothed_supply).max(config.min_supply);
            SurgeCell {
                cell: *cell,
                demand: value(&demand),
                supply: value(&supply),
                smoothed_demand: value(&smoothed_demand),
                smoothed_supply: value(&smoothed_supply),
                ratio,
                multiplier: config.multiplier(ratio),
            }
        });
    }
    Ok(cells.into_values().collect())
}

/// Multipliers for coordinate lookups; a position outside every surge cell
/// has the minimum multiplier.
#[derive(Debug, Clone)]
pub struct SurgeIndex {
    cells: CellMap<f64>,
    default: f64,
}

impl SurgeIndex {
    pub fn new(cells: &[SurgeCell], config: &SurgeConfig) -> Self {
        let mut map = CellMap::new();
        for cell in cells {
            map.insert(cell.cell, cell.multiplier);
        }
        Self {
            cells: map,
            default: config.min_multiplier,
        }
    }

    pub fn multiplier_at(&self, lon: f64, lat: f64) -> f64 {
        self.cells
            .lookup(lon, lat)
            .map_or(self.default, |(_, multiplier)| *multiplier)
    }
}

/// Supply to move from one cell to another.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct RebalanceMove {
    #[serde(serialize_with = "serialize_cell")]
    pub from: CellIndex,
    #[serde(serialize_with = "serialize_cell")]
    pub to: CellIndex,
    pub amount: f64,
    /// Cells between the two, along the grid.
    pub grid_distance: i32,
}

/// Plans moves of surplus supply to cells with unmet demand. A cell needs
/// `demand / balance_ratio` supply. The largest shortage is served first,
/// from the nearest cells with a surplus within `max_rings`.
pub fn plan_rebalance(
    cells: &[SurgeCell],
    config: &SurgeConfig,
    max_rings: u32,
) -> Vec<RebalanceMove> {
    let need = |cell: &SurgeCell| cell.demand / config.balance_ratio - cell.supply;
    let mut shortages = cells
        .iter()
        .filter(|cell| need(cell) > 1e-9)
        .map(|cell| (cell.cell, need(cell)))
        .collect::<Vec<_>>();
    shortages.sort_by(|left, right| right.1.total_cmp(&left.1).then(left.0.cmp(&right.0)));
    let mut surplus = cells
        .iter()
        .filter(|cell| need(cell) < -1e-9)
        .map(|cell| (cell.cell, -need(cell)))
        .collect::<Vec<_>>();
    let mut moves = Vec::new();
    for (to, mut shortage) in shortages {
        let mut sources = surplus
            .iter()
            .enumerate()
            .filter(|(_, (_, amount))| *amount > 1e-9)
            .filter_map(|(index, (from, _))| {
                let distance = from.grid_distance(to).ok()?;
                (distance as u32 <= max_rings).then_some((distance, *from, index))
            })
            .collect::<Vec<_>>();
        sources.sort_unstable();
        for (grid_distance, from, index) in sources {
            if shortage <= 1e-9 {
                break;
            }
            let amount = shortage.min(surplus[index].1);
            surplus[index].1 -= amount;
            shortage -= amount;
            moves.push(RebalanceMove {
                from,
                to,
                amount,
                grid_distance,
            });
        }
    }
    moves
}
