use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DatasetId(pub String);

impl DatasetId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CacheBundleId(pub String);

impl CacheBundleId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TravelMode {
    #[default]
    Car,
    Bicycle,
    Foot,
    /// Buses and other scheduled road public transport (`bus`, `psv`).
    Bus,
    /// Heavy goods vehicles (`hgv`).
    Hgv,
    Taxi,
    Motorcycle,
    /// Mopeds and motor scooters (`moped`).
    Moped,
}

impl TravelMode {
    pub const fn access_bit(self) -> u16 {
        match self {
            Self::Car => 1 << 0,
            Self::Bicycle => 1 << 1,
            Self::Foot => 1 << 2,
            Self::Bus => 1 << 3,
            Self::Hgv => 1 << 4,
            Self::Taxi => 1 << 5,
            Self::Motorcycle => 1 << 6,
            Self::Moped => 1 << 7,
        }
    }

    /// Modes that drive on the road network under motor power and obey
    /// posted speed limits.
    pub const fn is_motorized(self) -> bool {
        !matches!(self, Self::Bicycle | Self::Foot)
    }
}

/// File format of an imported dataset source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SourceFormat {
    /// OpenStreetMap protobuf extract (`.osm.pbf`).
    #[default]
    OsmPbf,
    /// Overture Maps transportation theme GeoParquet (a `.parquet` /
    /// `.geoparquet` file or a directory of them).
    OvertureParquet,
    /// One or more OGC GeoPackage files read with an explicit ingest mapping.
    GeoPackage,
}

impl SourceFormat {
    pub const fn label(self) -> &'static str {
        match self {
            Self::OsmPbf => "osm-pbf",
            Self::OvertureParquet => "overture-parquet",
            Self::GeoPackage => "geopackage",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildStage {
    Registered,
    TopologyPending,
    TopologyReady,
    MetricsPending,
    MetricsReady,
}
