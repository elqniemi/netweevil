//! Administrative areas and IANA timezones assigned to topology nodes at
//! import. Both layers intern their values: nodes store a small index into a
//! table of distinct areas or zone names, which the bundle codecs collapse to
//! a constant or near-constant column. Edges inherit the values of their tail
//! node; an edge whose tail and head lie in different countries is a border
//! crossing.

use serde::{Deserialize, Serialize};

/// Node has no administrative area (outside every imported boundary).
pub const NO_ADMIN_AREA: u32 = u32::MAX;
/// Node has no timezone (no zone covers it).
pub const NO_TIMEZONE: u32 = u32::MAX;

/// One distinct country/state combination from `admin_level=2` and
/// `admin_level=4` boundaries. (Stored with bincode: every field is always
/// serialized.)
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub struct AdminArea {
    /// ISO 3166-1 alpha-2 country code (`NL`). Empty when the containing
    /// country boundary carries no code and none could be derived from the
    /// state's ISO 3166-2 code.
    pub country_code: String,
    #[serde(default)]
    pub country_name: Option<String>,
    /// ISO 3166-2 subdivision code (`NL-GR`) of the `admin_level=4` area.
    #[serde(default)]
    pub state_code: Option<String>,
    #[serde(default)]
    pub state_name: Option<String>,
}

impl AdminArea {
    pub fn country(&self) -> Option<&str> {
        (!self.country_code.is_empty()).then_some(self.country_code.as_str())
    }

    /// Traffic side of the area's country; `None` without a country code.
    pub fn driving_side(&self) -> Option<DrivingSide> {
        self.country().map(country_driving_side)
    }
}

/// Side of the road vehicles keep to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrivingSide {
    #[default]
    Right,
    Left,
}

/// ISO 3166-1 alpha-2 codes of countries and territories that drive on the
/// left. Every other code drives on the right.
const LEFT_HAND_TRAFFIC: &[&str] = &[
    "AG", "AI", "AU", "BB", "BD", "BM", "BN", "BS", "BT", "BW", "CC", "CK", "CX", "CY", "DM", "FJ",
    "FK", "GB", "GD", "GG", "GY", "HK", "IE", "IM", "IN", "IO", "JE", "JM", "JP", "KE", "KI", "KN",
    "KY", "LC", "LK", "LS", "MO", "MS", "MT", "MU", "MV", "MW", "MY", "MZ", "NA", "NF", "NP", "NR",
    "NU", "NZ", "PG", "PK", "PN", "SB", "SC", "SG", "SH", "SR", "SZ", "TC", "TH", "TK", "TL", "TO",
    "TT", "TV", "TZ", "UG", "VC", "VG", "VI", "WS", "ZA", "ZM", "ZW",
];

/// Driving side of an ISO 3166-1 alpha-2 country code (case-insensitive).
pub fn country_driving_side(country_code: &str) -> DrivingSide {
    let upper = country_code.trim().to_ascii_uppercase();
    if LEFT_HAND_TRAFFIC.binary_search(&upper.as_str()).is_ok() {
        DrivingSide::Left
    } else {
        DrivingSide::Right
    }
}

/// Where the administrative boundaries of a dataset came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminBoundarySource {
    /// `boundary=administrative` relations in the imported OSM extract.
    SourceExtract,
    /// A separate boundaries file (GeoJSON or OSM PBF).
    File { path: String },
}

/// Per-node administrative areas and timezones. Node columns are either
/// empty (layer not imported) or have one entry per topology node.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TopologyRegions {
    #[serde(default)]
    pub admin_source: Option<AdminBoundarySource>,
    pub admin_areas: Vec<AdminArea>,
    /// Index into `admin_areas`, or [`NO_ADMIN_AREA`].
    pub node_admin_area: Vec<u32>,
    /// IANA zone names (`Europe/Amsterdam`).
    pub timezones: Vec<String>,
    /// Index into `timezones`, or [`NO_TIMEZONE`].
    pub node_timezone: Vec<u32>,
}

impl TopologyRegions {
    pub fn node_admin_area(&self, node_index: usize) -> Option<&AdminArea> {
        let index = *self.node_admin_area.get(node_index)?;
        self.admin_areas.get(index as usize)
    }

    pub fn node_country(&self, node_index: usize) -> Option<&str> {
        self.node_admin_area(node_index)
            .and_then(AdminArea::country)
    }

    pub fn node_timezone(&self, node_index: usize) -> Option<&str> {
        let index = *self.node_timezone.get(node_index)?;
        self.timezones.get(index as usize).map(String::as_str)
    }

    pub fn has_admin_areas(&self) -> bool {
        !self.node_admin_area.is_empty()
    }

    pub fn has_timezones(&self) -> bool {
        !self.node_timezone.is_empty()
    }
}

#[cfg(test)]
mod tests;
