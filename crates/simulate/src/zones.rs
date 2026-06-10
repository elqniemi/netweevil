//! Polygon zone handling: point-in-polygon tests and per-edge effect masks.

use crate::scenario::{ZoneConfig, ZoneEffect};
use netweevil_core::TravelMode;

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

#[derive(Debug, Clone)]
pub struct PreparedZone {
    pub zone_id: String,
    pub effect: ZoneEffect,
    pub polygon: PreparedPolygon,
}

pub fn prepare_zones(zones: &[ZoneConfig]) -> Vec<PreparedZone> {
    zones
        .iter()
        .map(|zone| PreparedZone {
            zone_id: zone.zone_id.clone(),
            effect: zone.effect.clone(),
            polygon: PreparedPolygon::new(zone.polygon.clone()),
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
        if !zone.polygon.contains(lon, lat) {
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

    fn square() -> PreparedPolygon {
        PreparedPolygon::new(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]])
    }

    #[test]
    fn contains_inside_point() {
        assert!(square().contains(0.5, 0.5));
    }

    #[test]
    fn excludes_outside_point() {
        assert!(!square().contains(1.5, 0.5));
        assert!(!square().contains(0.5, -0.1));
    }

    #[test]
    fn handles_closed_ring() {
        let poly = PreparedPolygon::new(vec![
            [0.0, 0.0],
            [1.0, 0.0],
            [1.0, 1.0],
            [0.0, 1.0],
            [0.0, 0.0],
        ]);
        assert!(poly.contains(0.5, 0.5));
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
