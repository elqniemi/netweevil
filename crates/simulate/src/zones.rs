//! Zone handling: point-in-polygon and H3 cell tests and per-edge effect
//! masks.

use crate::scenario::{ZoneConfig, ZoneEffect};
use netweevil_core::TravelMode;
use netweevil_h3::{CellIndex, CellSet, parse_cell};

#[derive(Debug, Clone)]
pub struct PreparedPolygon {
    pub ring: Vec<[f64; 2]>,
    pub min_lon: f64,
    pub min_lat: f64,
    pub max_lon: f64,
    pub max_lat: f64,
}

impl PreparedPolygon {
    pub fn new(mut ring: Vec<[f64; 2]>) -> Self {
        if ring.len() >= 2 && ring.first() == ring.last() {
            ring.pop();
        }
        let mut min_lon = f64::INFINITY;
        let mut min_lat = f64::INFINITY;
        let mut max_lon = f64::NEG_INFINITY;
        let mut max_lat = f64::NEG_INFINITY;
        for point in &ring {
            min_lon = min_lon.min(point[0]);
            min_lat = min_lat.min(point[1]);
            max_lon = max_lon.max(point[0]);
            max_lat = max_lat.max(point[1]);
        }
        Self {
            ring,
            min_lon,
            min_lat,
            max_lon,
            max_lat,
        }
    }

    pub fn contains(&self, lon: f64, lat: f64) -> bool {
        if lon < self.min_lon || lon > self.max_lon || lat < self.min_lat || lat > self.max_lat {
            return false;
        }
        // Even-odd ray casting.
        let mut inside = false;
        let count = self.ring.len();
        if count < 3 {
            return false;
        }
        let mut j = count - 1;
        for i in 0..count {
            let pi = self.ring[i];
            let pj = self.ring[j];
            if ((pi[1] > lat) != (pj[1] > lat))
                && lon < (pj[0] - pi[0]) * (lat - pi[1]) / (pj[1] - pi[1]) + pi[0]
            {
                inside = !inside;
            }
            j = i;
        }
        inside
    }
}

/// H3 cells of a zone. `cumulative_area` holds the running area of
/// `cells`, for sampling a cell in proportion to its area.
#[derive(Debug, Clone)]
pub struct PreparedCells {
    pub set: CellSet,
    pub cells: Vec<CellIndex>,
    pub cumulative_area: Vec<f64>,
}

impl PreparedCells {
    pub fn new(cells: Vec<CellIndex>) -> Self {
        let mut set = CellSet::new();
        let mut area = 0.0;
        let mut cumulative_area = Vec::with_capacity(cells.len());
        for cell in &cells {
            set.insert(*cell, ());
            area += cell.area_m2();
            cumulative_area.push(area);
        }
        Self {
            set,
            cells,
            cumulative_area,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ZoneShape {
    Polygon(PreparedPolygon),
    Cells(PreparedCells),
}

impl ZoneShape {
    pub fn contains(&self, lon: f64, lat: f64) -> bool {
        match self {
            Self::Polygon(polygon) => polygon.contains(lon, lat),
            Self::Cells(cells) => cells.set.contains(lon, lat),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PreparedZone {
    pub zone_id: String,
    pub effect: ZoneEffect,
    pub shape: ZoneShape,
}

/// Prepares zones for lookups. [`ZoneConfig::validate`] rejects malformed
/// cell ids; one that still arrives here is left out of its zone.
pub fn prepare_zones(zones: &[ZoneConfig]) -> Vec<PreparedZone> {
    zones
        .iter()
        .map(|zone| PreparedZone {
            zone_id: zone.zone_id.clone(),
            effect: zone.effect.clone(),
            shape: if zone.cells.is_empty() {
                ZoneShape::Polygon(PreparedPolygon::new(zone.polygon.clone()))
            } else {
                ZoneShape::Cells(PreparedCells::new(
                    zone.cells
                        .iter()
                        .filter_map(|cell| parse_cell(cell).ok())
                        .collect(),
                ))
            },
        })
        .collect()
}

/// Aggregated per-edge zone effects, applied at network build time and when
/// zones are added mid-run.
#[derive(Debug, Clone, Copy)]
pub struct EdgeZoneEffects {
    pub speed_factor: f32,
    pub capacity_factor: f32,
    pub background_load: f32,
    /// Bitmask of `TravelMode::access_bit` values blocked inside no-access zones.
    pub closed_mode_mask: u16,
}

impl Default for EdgeZoneEffects {
    fn default() -> Self {
        Self {
            speed_factor: 1.0,
            capacity_factor: 1.0,
            background_load: 0.0,
            closed_mode_mask: 0,
        }
    }
}

pub fn closed_mask_for_modes(modes: &[TravelMode]) -> u16 {
    if modes.is_empty() {
        return u16::MAX;
    }
    modes
        .iter()
        .fold(0u16, |mask, mode| mask | mode.access_bit())
}

/// Combine all zones covering a point into a single effect set.
pub fn effects_at(zones: &[PreparedZone], lon: f64, lat: f64) -> EdgeZoneEffects {
    let mut effects = EdgeZoneEffects::default();
    for zone in zones {
        if !zone.shape.contains(lon, lat) {
            continue;
        }
        match &zone.effect {
            ZoneEffect::NoAccess { modes } => {
                effects.closed_mode_mask |= closed_mask_for_modes(modes);
            }
            ZoneEffect::SpeedFactor { factor } => {
                effects.speed_factor *= factor.clamp(0.01, 10.0) as f32;
            }
            ZoneEffect::CapacityFactor { factor } => {
                effects.capacity_factor *= factor.clamp(0.01, 10.0) as f32;
            }
            ZoneEffect::HighTraffic { background_load } => {
                effects.background_load =
                    (effects.background_load + background_load.clamp(0.0, 1.0) as f32).min(1.0);
            }
            ZoneEffect::Spawn { .. } | ZoneEffect::Attract { .. } => {}
        }
    }
    effects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polygon_contains_inside_points_for_open_and_closed_rings() {
        let open = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let mut closed = open.clone();
        closed.push([0.0, 0.0]);
        for ring in [open, closed] {
            let polygon = PreparedPolygon::new(ring);
            assert!(polygon.contains(0.5, 0.5));
            assert!(!polygon.contains(1.5, 0.5));
            assert!(!polygon.contains(0.5, -0.1));
        }
    }

    #[test]
    fn cell_zone_applies_its_effect_inside_its_cells() {
        let resolution = netweevil_h3::parse_resolution(8).unwrap();
        let cell = netweevil_h3::cell_at(6.5665, 53.2194, resolution).unwrap();
        let zone = ZoneConfig {
            zone_id: "slow".to_string(),
            label: String::new(),
            polygon: Vec::new(),
            cells: vec![cell.to_string()],
            effect: ZoneEffect::SpeedFactor { factor: 0.5 },
        };
        zone.validate().unwrap();
        let zones = prepare_zones(std::slice::from_ref(&zone));
        let [lon, lat] = netweevil_h3::cell_center(cell);
        assert_eq!(effects_at(&zones, lon, lat).speed_factor, 0.5);
        assert_eq!(effects_at(&zones, lon + 1.0, lat).speed_factor, 1.0);

        let mut both = zone.clone();
        both.polygon = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]];
        assert!(both.validate().is_err());
        let mut neither = zone.clone();
        neither.cells.clear();
        assert!(neither.validate().is_err());
        let mut malformed = zone;
        malformed.cells = vec!["nope".to_string()];
        assert!(malformed.validate().is_err());
    }

    #[test]
    fn empty_mode_list_blocks_all_modes() {
        assert_eq!(closed_mask_for_modes(&[]), u16::MAX);
        assert_eq!(
            closed_mask_for_modes(&[TravelMode::Car]),
            TravelMode::Car.access_bit()
        );
    }
}
