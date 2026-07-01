use serde::{Deserialize, Serialize};

pub const EDGE_FLAG_ROUNDABOUT: u32 = 1 << 0;
pub const EDGE_FLAG_TARGET_TRAFFIC_SIGNAL: u32 = 1 << 1;
pub const EDGE_FLAG_INFERRED_FOOT_REVERSE_ONEWAY: u32 = 1 << 2;
pub const EDGE_FLAG_INFERRED_BICYCLE_CONTRAFLOW: u32 = 1 << 3;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct NodeId(pub u32);

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
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
pub enum HighwayClass {
    Motorway,
    MotorwayLink,
    Trunk,
    TrunkLink,
    Primary,
    PrimaryLink,
    Secondary,
    SecondaryLink,
    Tertiary,
    TertiaryLink,
    Residential,
    Unclassified,
    LivingStreet,
    Service,
    Track,
    Path,
    Cycleway,
    Footway,
    Pedestrian,
    Steps,
    Bridleway,
    Ferry,
    #[default]
    Unknown,
}

impl HighwayClass {
    pub const fn road_class(self) -> RoadClass {
        match self {
            Self::Motorway | Self::MotorwayLink => RoadClass::Motorway,
            Self::Trunk | Self::TrunkLink => RoadClass::Trunk,
            Self::Primary | Self::PrimaryLink => RoadClass::Primary,
            Self::Secondary | Self::SecondaryLink => RoadClass::Secondary,
            Self::Tertiary | Self::TertiaryLink => RoadClass::Tertiary,
            Self::Residential | Self::Unclassified | Self::LivingStreet => RoadClass::Residential,
            Self::Service => RoadClass::Service,
            Self::Track => RoadClass::Track,
            Self::Path
            | Self::Cycleway
            | Self::Footway
            | Self::Pedestrian
            | Self::Steps
            | Self::Bridleway => RoadClass::Path,
            Self::Ferry => RoadClass::Ferry,
            Self::Unknown => RoadClass::Unknown,
        }
    }

    pub const fn from_road_class(road_class: RoadClass) -> Self {
        match road_class {
            RoadClass::Motorway => Self::Motorway,
            RoadClass::Trunk => Self::Trunk,
            RoadClass::Primary => Self::Primary,
            RoadClass::Secondary => Self::Secondary,
            RoadClass::Tertiary => Self::Tertiary,
            RoadClass::Residential => Self::Residential,
            RoadClass::Service => Self::Service,
            RoadClass::Track => Self::Track,
            RoadClass::Ferry => Self::Ferry,
            RoadClass::Path => Self::Path,
            RoadClass::Unknown => Self::Unknown,
        }
    }
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
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
    /// Posted speed limit in km/h for this travel direction, when the
    /// source data carries one (OSM `maxspeed`, Overture `speed_limits`).
    #[serde(default)]
    pub max_speed_kph: Option<f32>,
    /// Lane count for this travel direction, when the source data carries
    /// one (OSM `lanes`, Overture `lanes` where present).
    #[serde(default)]
    pub lanes: Option<u8>,
    pub name_index: Option<u32>,
    pub geometry_offset: u64,
    pub geometry_len: u32,
    pub flags: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct RoutingEdge {
    pub edge_id: EdgeId,
    pub from: NodeId,
    pub to: NodeId,
    pub source_way_id: i64,
    pub length_m: u32,
    pub flags: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct EdgeProfileAttributes {
    #[serde(default)]
    pub duration_s: Option<f64>,
    pub road_class: RoadClass,
    #[serde(default)]
    pub highway: HighwayClass,
    pub surface: SurfaceClass,
    #[serde(default)]
    pub smoothness: SmoothnessClass,
    pub access_mask: AccessMask,
    #[serde(default)]
    pub is_toll: bool,
    #[serde(default)]
    pub max_speed_kph: Option<f32>,
    #[serde(default)]
    pub lanes: Option<u8>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct EdgePresentation {
    pub name_index: Option<u32>,
    pub geometry_offset: u64,
    pub geometry_len: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TopologyEdgeLayers {
    #[serde(default)]
    pub routing: Vec<RoutingEdge>,
    #[serde(default)]
    pub profile: Vec<EdgeProfileAttributes>,
    #[serde(default)]
    pub presentation: Vec<EdgePresentation>,
}

impl TopologyEdgeLayers {
    pub fn from_directed_edges(edges: &[DirectedEdge]) -> Self {
        let mut routing = Vec::with_capacity(edges.len());
        let mut profile = Vec::with_capacity(edges.len());
        let mut presentation = Vec::with_capacity(edges.len());
        for edge in edges {
            routing.push(RoutingEdge {
                edge_id: edge.edge_id,
                from: edge.from,
                to: edge.to,
                source_way_id: edge.source_way_id,
                length_m: edge.length_m,
                flags: edge.flags,
            });
            profile.push(EdgeProfileAttributes {
                duration_s: edge.duration_s,
                road_class: edge.road_class,
                highway: HighwayClass::from_road_class(edge.road_class),
                surface: edge.surface,
                smoothness: edge.smoothness,
                access_mask: edge.access_mask,
                is_toll: edge.is_toll,
                max_speed_kph: edge.max_speed_kph,
                lanes: edge.lanes,
            });
            presentation.push(EdgePresentation {
                name_index: edge.name_index,
                geometry_offset: edge.geometry_offset,
                geometry_len: edge.geometry_len,
            });
        }
        Self {
            routing,
            profile,
            presentation,
        }
    }
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
    pub connected_components: Option<ConnectedComponentsMeta>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConnectedComponentKind {
    #[default]
    Weak,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConnectedComponentsMeta {
    #[serde(default)]
    pub kind: ConnectedComponentKind,
    #[serde(default)]
    pub component_count: u32,
    #[serde(default)]
    pub largest_component_node_count: u64,
    #[serde(default)]
    pub largest_component_edge_count: u64,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EdgeNameBundle {
    #[serde(default)]
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EdgeBasedTopology {
    #[serde(default)]
    pub node_first_out: Vec<u32>,
    #[serde(default)]
    pub node_edge_order: Vec<u32>,
    #[serde(default)]
    pub edge_transition_first_out: Vec<u32>,
    #[serde(default)]
    pub edge_transition_edges: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyBundle {
    pub schema_version: u32,
    pub source_path: String,
    pub source_sha256: String,
    pub nodes: Vec<TopologyNode>,
    #[serde(default)]
    pub edge_layers: TopologyEdgeLayers,
    #[serde(default)]
    pub edges: Vec<DirectedEdge>,
    #[serde(default)]
    pub turn_restrictions: Vec<TurnRestriction>,
    #[serde(default)]
    pub names: Vec<String>,
    #[serde(default)]
    pub edge_based_topology: EdgeBasedTopology,
    #[serde(default)]
    pub spatial_index: Option<NodeSpatialIndex>,
    #[serde(default)]
    pub node_component_ids: Vec<u32>,
    #[serde(default)]
    pub edge_component_ids: Vec<u32>,
}

impl TopologyBundle {
    pub fn edge_count(&self) -> usize {
        if !self.edge_layers.routing.is_empty() {
            self.edge_layers.routing.len()
        } else {
            self.edges.len()
        }
    }

    pub fn routing_edge(&self, edge_index: usize) -> RoutingEdge {
        if let Some(edge) = self.edge_layers.routing.get(edge_index) {
            *edge
        } else {
            let edge = self.edges[edge_index];
            RoutingEdge {
                edge_id: edge.edge_id,
                from: edge.from,
                to: edge.to,
                source_way_id: edge.source_way_id,
                length_m: edge.length_m,
                flags: edge.flags,
            }
        }
    }

    pub fn edge_profile(&self, edge_index: usize) -> EdgeProfileAttributes {
        if let Some(edge) = self.edge_layers.profile.get(edge_index) {
            *edge
        } else {
            let edge = self.edges[edge_index];
            EdgeProfileAttributes {
                duration_s: edge.duration_s,
                road_class: edge.road_class,
                highway: HighwayClass::from_road_class(edge.road_class),
                surface: edge.surface,
                smoothness: edge.smoothness,
                access_mask: edge.access_mask,
                is_toll: edge.is_toll,
                max_speed_kph: edge.max_speed_kph,
                lanes: edge.lanes,
            }
        }
    }

    pub fn edge_presentation(&self, edge_index: usize) -> EdgePresentation {
        if let Some(edge) = self.edge_layers.presentation.get(edge_index) {
            *edge
        } else {
            let edge = self.edges[edge_index];
            EdgePresentation {
                name_index: edge.name_index,
                geometry_offset: edge.geometry_offset,
                geometry_len: edge.geometry_len,
            }
        }
    }

    pub fn edge(&self, edge_index: usize) -> DirectedEdge {
        if let Some(edge) = self.edges.get(edge_index) {
            *edge
        } else {
            let routing = self.routing_edge(edge_index);
            let profile = self.edge_profile(edge_index);
            let presentation = self.edge_presentation(edge_index);
            DirectedEdge {
                edge_id: routing.edge_id,
                from: routing.from,
                to: routing.to,
                source_way_id: routing.source_way_id,
                length_m: routing.length_m,
                duration_s: profile.duration_s,
                road_class: profile.road_class,
                surface: profile.surface,
                smoothness: profile.smoothness,
                access_mask: profile.access_mask,
                is_toll: profile.is_toll,
                max_speed_kph: profile.max_speed_kph,
                lanes: profile.lanes,
                name_index: presentation.name_index,
                geometry_offset: presentation.geometry_offset,
                geometry_len: presentation.geometry_len,
                flags: routing.flags,
            }
        }
    }

    pub fn push_edge(&mut self, edge: DirectedEdge) {
        if !self.edges.is_empty() || self.edge_layers.routing.is_empty() {
            self.edges.push(edge);
        }
        if !self.edge_layers.routing.is_empty() || self.edges.is_empty() {
            self.edge_layers.routing.push(RoutingEdge {
                edge_id: edge.edge_id,
                from: edge.from,
                to: edge.to,
                source_way_id: edge.source_way_id,
                length_m: edge.length_m,
                flags: edge.flags,
            });
            self.edge_layers.profile.push(EdgeProfileAttributes {
                duration_s: edge.duration_s,
                road_class: edge.road_class,
                highway: HighwayClass::from_road_class(edge.road_class),
                surface: edge.surface,
                smoothness: edge.smoothness,
                access_mask: edge.access_mask,
                is_toll: edge.is_toll,
                max_speed_kph: edge.max_speed_kph,
                lanes: edge.lanes,
            });
            self.edge_layers.presentation.push(EdgePresentation {
                name_index: edge.name_index,
                geometry_offset: edge.geometry_offset,
                geometry_len: edge.geometry_len,
            });
        }
    }

    pub fn set_edge_name_index(&mut self, edge_index: usize, name_index: Option<u32>) {
        if let Some(edge) = self.edges.get_mut(edge_index) {
            edge.name_index = name_index;
        }
        if let Some(edge) = self.edge_layers.presentation.get_mut(edge_index) {
            edge.name_index = name_index;
        }
    }

    pub fn set_edge_flags(&mut self, edge_index: usize, flags: u32) {
        if let Some(edge) = self.edges.get_mut(edge_index) {
            edge.flags = flags;
        }
        if let Some(edge) = self.edge_layers.routing.get_mut(edge_index) {
            edge.flags = flags;
        }
    }

    pub fn node_component_id(&self, node_id: u32) -> Option<u32> {
        self.node_component_ids.get(node_id as usize).copied()
    }

    pub fn edge_component_id(&self, edge_id: u32) -> Option<u32> {
        self.edge_component_ids.get(edge_id as usize).copied()
    }
}
