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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TravelMode {
    #[default]
    Car,
    Bicycle,
    Foot,
    Transit,
    Hgv,
}

impl TravelMode {
    pub const fn access_bit(self) -> u16 {
        match self {
            Self::Car => 1 << 0,
            Self::Bicycle => 1 << 1,
            Self::Foot => 1 << 2,
            Self::Transit => 1 << 3,
            Self::Hgv => 1 << 4,
        }
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
}

impl SourceFormat {
    pub const fn label(self) -> &'static str {
        match self {
            Self::OsmPbf => "osm-pbf",
            Self::OvertureParquet => "overture-parquet",
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
