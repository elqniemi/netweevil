use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EdgeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoadClass {
    Motorway,
    Trunk,
    Primary,
    Secondary,
    Tertiary,
    Residential,
    Service,
    Track,
    Ferry,
    Path,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceClass {
    Asphalt,
    Paved,
    Gravel,
    Cobblestone,
    Ground,
    Dirt,
    Sand,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AccessMask(pub u16);

impl AccessMask {
    pub const CAR: u16 = 1 << 0;
    pub const BICYCLE: u16 = 1 << 1;
    pub const FOOT: u16 = 1 << 2;
    pub const TRANSIT: u16 = 1 << 3;
    pub const HGV: u16 = 1 << 4;

    pub const fn new(bits: u16) -> Self {
        Self(bits)
    }

    pub const fn contains(self, bit: u16) -> bool {
        self.0 & bit != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SmoothnessClass {
    Excellent,
    Good,
    Intermediate,
    Bad,
    VeryBad,
    Horrible,
    VeryHorrible,
    Impassable,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectedEdge {
    pub edge_id: EdgeId,
    pub from: NodeId,
    pub to: NodeId,
    pub source_way_id: i64,
    pub length_m: u32,
    #[serde(default)]
    pub duration_s: Option<f64>,
    pub road_class: RoadClass,
    pub surface: SurfaceClass,
    #[serde(default)]
    pub smoothness: SmoothnessClass,
    pub access_mask: AccessMask,
    #[serde(default)]
    pub is_toll: bool,
    pub name_index: Option<u32>,
    pub geometry_offset: u64,
    pub geometry_len: u32,
    pub flags: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnRestrictionKind {
    NoTurn,
    OnlyTurn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRestriction {
    pub relation_id: i64,
    pub kind: TurnRestrictionKind,
    pub edge_path: Vec<EdgeId>,
    #[serde(default)]
    pub mode_mask: AccessMask,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyBundleMeta {
    pub node_count: u64,
    pub edge_count: u64,
    pub geometry_bytes: u64,
    pub turn_count: u64,
    #[serde(default)]
    pub source_node_count: u64,
    #[serde(default)]
    pub source_way_count: u64,
    #[serde(default)]
    pub source_relation_count: u64,
    #[serde(default)]
    pub routable_way_count: u64,
    #[serde(default)]
    pub skipped_way_count: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct TopologyBounds {
    pub min_lon: f64,
    pub min_lat: f64,
    pub max_lon: f64,
    pub max_lat: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyNode {
    pub node_id: NodeId,
    pub osm_node_id: i64,
    pub lon: f64,
    pub lat: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct SpatialIndexCell {
    pub node_start: u32,
    pub node_len: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeSpatialIndex {
    pub bounds: TopologyBounds,
    pub columns: u32,
    pub rows: u32,
    pub cell_width_deg: f64,
    pub cell_height_deg: f64,
    pub cells: Vec<SpatialIndexCell>,
    pub node_ids: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyBundle {
    pub schema_version: u32,
    pub source_path: String,
    pub source_sha256: String,
    pub nodes: Vec<TopologyNode>,
    pub edges: Vec<DirectedEdge>,
    #[serde(default)]
    pub turn_restrictions: Vec<TurnRestriction>,
    #[serde(default)]
    pub names: Vec<String>,
    #[serde(default)]
    pub spatial_index: Option<NodeSpatialIndex>,
}
