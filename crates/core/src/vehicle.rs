//! Physical vehicle restrictions (height, weight, hazardous goods, ...)
//! imported from the source data, and the vehicle parameters a profile or
//! request checks them against.
//!
//! Restrictions are sparse: most edges carry none. The topology therefore
//! stores a deduplicated restriction table plus `(edge, restriction)` rows
//! sorted by edge instead of a per-edge column.

use serde::{Deserialize, Serialize};

use crate::AccessMask;

/// ADR tunnel category of a tunnel (`hazmat:adr_tunnel_cat`) or tunnel
/// restriction code of a load. Category A carries no restriction and is not
/// represented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum AdrTunnelCategory {
    B,
    C,
    D,
    E,
}

impl AdrTunnelCategory {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "B" | "b" => Some(Self::B),
            "C" | "c" => Some(Self::C),
            "D" | "d" => Some(Self::D),
            "E" | "e" => Some(Self::E),
            _ => None,
        }
    }
}

/// One restriction kind. Lengths are centimetres and weights kilograms, so
/// records stay small and compare exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VehicleRestrictionKind {
    MaxHeightCm(u32),
    MaxWidthCm(u32),
    MaxLengthCm(u32),
    MaxWeightKg(u32),
    MaxAxleLoadKg(u32),
    /// `hazmat=no`: no dangerous goods.
    NoHazmat,
    /// `hazmat:water=no`: no water-polluting goods.
    NoHazmatWater,
    /// Tunnel of this ADR category: loads whose tunnel restriction code is
    /// at most the category may not pass.
    AdrTunnel(AdrTunnelCategory),
    /// `trailer=no`.
    NoTrailer,
    /// `hgv_articulated=no`.
    NoArticulated,
}

/// A restriction scoped to the travel modes (`AccessMask` bits) it binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VehicleRestriction {
    pub modes: AccessMask,
    pub kind: VehicleRestrictionKind,
}

impl VehicleRestriction {
    /// True when a vehicle of `mode_bit` with `vehicle`'s parameters may not
    /// use an edge carrying this restriction. Unset parameters never
    /// violate a restriction.
    pub fn excludes(&self, vehicle: &VehicleParams, mode_bit: u16) -> bool {
        if !self.modes.contains(mode_bit) {
            return false;
        }
        let exceeds = |value: Option<f64>, scale: f64, limit: u32| {
            // Parameters are compared at the restriction's resolution, so a
            // 3.999 m vehicle passes a 4 m sign.
            value.is_some_and(|value| (value * scale).round() > f64::from(limit))
        };
        match self.kind {
            VehicleRestrictionKind::MaxHeightCm(limit) => exceeds(vehicle.height_m, 100.0, limit),
            VehicleRestrictionKind::MaxWidthCm(limit) => exceeds(vehicle.width_m, 100.0, limit),
            VehicleRestrictionKind::MaxLengthCm(limit) => exceeds(vehicle.length_m, 100.0, limit),
            VehicleRestrictionKind::MaxWeightKg(limit) => exceeds(vehicle.weight_t, 1000.0, limit),
            VehicleRestrictionKind::MaxAxleLoadKg(limit) => {
                exceeds(vehicle.axle_load_t, 1000.0, limit)
            }
            VehicleRestrictionKind::NoHazmat => vehicle.carries_hazmat(),
            VehicleRestrictionKind::NoHazmatWater => vehicle.hazmat_water == Some(true),
            VehicleRestrictionKind::AdrTunnel(category) => {
                vehicle.adr_tunnel_code.is_some_and(|code| code <= category)
            }
            VehicleRestrictionKind::NoTrailer | VehicleRestrictionKind::NoArticulated => {
                vehicle.trailers.is_some_and(|trailers| trailers > 0)
            }
        }
    }
}

/// Physical vehicle parameters. Every field is optional: profiles provide
/// defaults and requests override individual fields.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct VehicleParams {
    #[serde(default)]
    pub height_m: Option<f64>,
    #[serde(default)]
    pub width_m: Option<f64>,
    #[serde(default)]
    pub length_m: Option<f64>,
    /// Actual gross weight in metric tonnes.
    #[serde(default)]
    pub weight_t: Option<f64>,
    /// Heaviest axle load in metric tonnes.
    #[serde(default)]
    pub axle_load_t: Option<f64>,
    /// Carries dangerous goods.
    #[serde(default)]
    pub hazmat: Option<bool>,
    /// Carries water-polluting goods.
    #[serde(default)]
    pub hazmat_water: Option<bool>,
    /// ADR tunnel restriction code of the load. Implies `hazmat`.
    #[serde(default)]
    pub adr_tunnel_code: Option<AdrTunnelCategory>,
    /// Number of trailers towed.
    #[serde(default)]
    pub trailers: Option<u8>,
}

impl VehicleParams {
    /// `self` with every field set in `overrides` replaced.
    pub fn merged(&self, overrides: &VehicleParams) -> VehicleParams {
        VehicleParams {
            height_m: overrides.height_m.or(self.height_m),
            width_m: overrides.width_m.or(self.width_m),
            length_m: overrides.length_m.or(self.length_m),
            weight_t: overrides.weight_t.or(self.weight_t),
            axle_load_t: overrides.axle_load_t.or(self.axle_load_t),
            hazmat: overrides.hazmat.or(self.hazmat),
            hazmat_water: overrides.hazmat_water.or(self.hazmat_water),
            adr_tunnel_code: overrides.adr_tunnel_code.or(self.adr_tunnel_code),
            trailers: overrides.trailers.or(self.trailers),
        }
    }

    pub fn carries_hazmat(&self) -> bool {
        self.hazmat == Some(true) || self.adr_tunnel_code.is_some()
    }

    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("height_m", self.height_m),
            ("width_m", self.width_m),
            ("length_m", self.length_m),
            ("weight_t", self.weight_t),
            ("axle_load_t", self.axle_load_t),
        ] {
            if let Some(value) = value
                && !(value.is_finite() && value > 0.0)
            {
                return Err(format!("vehicle.{name} must be finite and > 0"));
            }
        }
        Ok(())
    }
}

/// Deduplicated restrictions plus `(edge, restriction)` rows sorted by edge
/// index. An edge may carry several restrictions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct VehicleRestrictionTable {
    pub restrictions: Vec<VehicleRestriction>,
    pub edges: Vec<u32>,
    pub edge_restrictions: Vec<u32>,
}

impl VehicleRestrictionTable {
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    pub fn validate(&self, edge_count: usize) -> Result<(), String> {
        if self.edges.len() != self.edge_restrictions.len() {
            return Err("vehicle restriction rows have mismatched columns".to_string());
        }
        if self.edges.windows(2).any(|pair| pair[0] > pair[1]) {
            return Err("vehicle restriction rows are not sorted by edge".to_string());
        }
        if self
            .edges
            .last()
            .is_some_and(|edge| *edge as usize >= edge_count)
        {
            return Err("vehicle restriction row references a missing edge".to_string());
        }
        if self
            .edge_restrictions
            .iter()
            .any(|id| *id as usize >= self.restrictions.len())
        {
            return Err("vehicle restriction row references a missing restriction".to_string());
        }
        Ok(())
    }

    /// Restrictions on `edge_index`.
    pub fn for_edge(&self, edge_index: usize) -> impl Iterator<Item = &VehicleRestriction> {
        let edge = edge_index as u32;
        let start = self.edges.partition_point(|candidate| *candidate < edge);
        let end = start + self.edges[start..].partition_point(|candidate| *candidate == edge);
        self.edge_restrictions[start..end]
            .iter()
            .map(|id| &self.restrictions[*id as usize])
    }

    /// Per-restriction exclusion flags for one vehicle and mode; the flags
    /// identify the vehicle's dimension class on this dataset.
    pub fn excluded_restrictions(&self, vehicle: &VehicleParams, mode_bit: u16) -> Vec<bool> {
        self.restrictions
            .iter()
            .map(|restriction| restriction.excludes(vehicle, mode_bit))
            .collect()
    }

    /// Sorted, deduplicated edge indexes excluded by `excluded` flags from
    /// [`Self::excluded_restrictions`].
    pub fn excluded_edges(&self, excluded: &[bool]) -> Vec<u32> {
        let mut edges = self
            .edges
            .iter()
            .zip(&self.edge_restrictions)
            .filter(|(_, id)| excluded[**id as usize])
            .map(|(edge, _)| *edge)
            .collect::<Vec<_>>();
        edges.dedup();
        edges
    }
}

#[cfg(test)]
mod tests;
