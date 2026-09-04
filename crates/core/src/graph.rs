use serde::{Deserialize, Serialize};

use crate::{FeatureAttributeTable, FeatureAttributeValueRef, NO_FEATURE_ROW, TemporalRuleSet};

/// Schema version of [`TopologyBundle`]. Readers accept this value only.
pub const TOPOLOGY_BUNDLE_SCHEMA_VERSION: u32 = 13;

pub const EDGE_FLAG_ROUNDABOUT: u32 = 1 << 0;
pub const EDGE_FLAG_TARGET_TRAFFIC_SIGNAL: u32 = 1 << 1;
pub const EDGE_FLAG_INFERRED_FOOT_REVERSE_ONEWAY: u32 = 1 << 2;
pub const EDGE_FLAG_INFERRED_BICYCLE_CONTRAFLOW: u32 = 1 << 3;
/// The edge exists so a request-owned temporal direction override can enable
/// it, but it is not part of the source feature's static travel direction.
/// Static routing and static CCH customization must keep it gated.
pub const EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION: u32 = 1 << 4;

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
    /// Three-dimensional segment length in metres, rounded to the nearest
    /// whole metre. When either endpoint has unknown elevation this is the
    /// horizontal great-circle length.
    pub length_m: u32,
    /// Positive elevation gain in this travel direction.
    pub ascent_m: f32,
    /// Positive elevation loss in this travel direction.
    pub descent_m: f32,
    /// Row in the source feature attribute table shared by every segment
    /// derived from the same source feature.
    pub feature_row: u32,
    /// `1` follows source geometry, `-1` traverses it backwards, and `0`
    /// means the orientation is unknown.
    pub source_direction: i8,
    /// Optional edge schedule imported from the mapped source feature.
    pub temporal_rule_id: Option<u32>,
    pub duration_s: Option<f64>,
    pub road_class: RoadClass,
    pub surface: SurfaceClass,
    pub smoothness: SmoothnessClass,
    pub access_mask: AccessMask,
    pub is_toll: bool,
    /// Posted speed limit in km/h for this travel direction, when the
    /// source data carries one (OSM `maxspeed`, Overture `speed_limits`).
    pub max_speed_kph: Option<f32>,
    /// Lane count for this travel direction, when the source data carries
    /// one (OSM `lanes`, Overture `lanes` where present).
    pub lanes: Option<u8>,
    pub name_index: Option<u32>,
    pub geometry_offset: u64,
    pub geometry_len: u32,
    pub flags: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RoutingEdge {
    pub edge_id: EdgeId,
    pub from: NodeId,
    pub to: NodeId,
    pub source_way_id: i64,
    /// Three-dimensional segment length in metres, rounded to the nearest
    /// whole metre. When either endpoint has unknown elevation this is the
    /// horizontal great-circle length.
    pub length_m: u32,
    /// Positive elevation gain in this travel direction.
    pub ascent_m: f32,
    /// Positive elevation loss in this travel direction.
    pub descent_m: f32,
    pub feature_row: u32,
    pub source_direction: i8,
    pub temporal_rule_id: Option<u32>,
    pub flags: u32,
}

impl Default for RoutingEdge {
    fn default() -> Self {
        Self {
            edge_id: EdgeId::default(),
            from: NodeId::default(),
            to: NodeId::default(),
            source_way_id: 0,
            length_m: 0,
            ascent_m: 0.0,
            descent_m: 0.0,
            feature_row: NO_FEATURE_ROW,
            source_direction: 0,
            temporal_rule_id: None,
            flags: 0,
        }
    }
}

impl RoutingEdge {
    /// Signed elevation change in this travel direction.
    pub fn elevation_delta_m(self) -> f32 {
        self.ascent_m - self.descent_m
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct EdgeProfileAttributes {
    pub duration_s: Option<f64>,
    pub road_class: RoadClass,
    pub highway: HighwayClass,
    pub surface: SurfaceClass,
    pub smoothness: SmoothnessClass,
    pub access_mask: AccessMask,
    pub is_toll: bool,
    pub max_speed_kph: Option<f32>,
    pub lanes: Option<u8>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct EdgePresentation {
    pub name_index: Option<u32>,
    pub geometry_offset: u64,
    pub geometry_len: u32,
}

/// The three parallel per-edge columns of a [`TopologyBundle`]: routing
/// geometry and flags, profile attributes, and presentation metadata. All
/// three are indexed by the same edge index.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TopologyEdgeLayers {
    pub routing: Vec<RoutingEdge>,
    pub profile: Vec<EdgeProfileAttributes>,
    pub presentation: Vec<EdgePresentation>,
}

impl TopologyEdgeLayers {
    /// Splits `edges` into the three parallel layers.
    pub fn from_directed_edges(edges: &[DirectedEdge]) -> Self {
        let mut layers = Self {
            routing: Vec::with_capacity(edges.len()),
            profile: Vec::with_capacity(edges.len()),
            presentation: Vec::with_capacity(edges.len()),
        };
        for edge in edges {
            layers.push_directed_edge(edge);
        }
        layers
    }

    /// Appends one edge to each layer.
    pub fn push_directed_edge(&mut self, edge: &DirectedEdge) {
        self.routing.push(RoutingEdge {
            edge_id: edge.edge_id,
            from: edge.from,
            to: edge.to,
            source_way_id: edge.source_way_id,
            length_m: edge.length_m,
            ascent_m: edge.ascent_m,
            descent_m: edge.descent_m,
            feature_row: edge.feature_row,
            source_direction: edge.source_direction,
            temporal_rule_id: edge.temporal_rule_id,
            flags: edge.flags,
        });
        self.profile.push(EdgeProfileAttributes {
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
        self.presentation.push(EdgePresentation {
            name_index: edge.name_index,
            geometry_offset: edge.geometry_offset,
            geometry_len: edge.geometry_len,
        });
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
    pub mode_mask: AccessMask,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyBundleMeta {
    pub node_count: u64,
    pub edge_count: u64,
    pub geometry_bytes: u64,
    pub turn_count: u64,
    pub connected_components: Option<ConnectedComponentsMeta>,
    pub source_node_count: u64,
    pub source_way_count: u64,
    pub source_relation_count: u64,
    pub routable_way_count: u64,
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
    pub kind: ConnectedComponentKind,
    pub component_count: u32,
    pub largest_component_node_count: u64,
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
    /// Elevation in the source dataset's vertical datum, in metres.
    /// `NaN` means that the elevation is unknown.
    pub z: f64,
}

impl TopologyNode {
    /// Returns the elevation when the source supplied a finite value.
    pub fn elevation_m(&self) -> Option<f64> {
        self.z.is_finite().then_some(self.z)
    }
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
    pub names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EdgeBasedTopology {
    pub node_first_out: Vec<u32>,
    pub node_edge_order: Vec<u32>,
    pub edge_transition_first_out: Vec<u32>,
    pub edge_transition_edges: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyBundle {
    pub schema_version: u32,
    pub source_path: String,
    pub source_sha256: String,
    pub nodes: Vec<TopologyNode>,
    pub edge_layers: TopologyEdgeLayers,
    pub turn_restrictions: Vec<TurnRestriction>,
    pub names: Vec<String>,
    pub edge_based_topology: EdgeBasedTopology,
    pub spatial_index: Option<NodeSpatialIndex>,
    pub node_component_ids: Vec<u32>,
    pub edge_component_ids: Vec<u32>,
    /// Losslessly retained, typed source attributes. Routing edges refer to
    /// rows by `feature_row` so segmented features do not duplicate values.
    pub feature_attributes: FeatureAttributeTable,
    /// Dataset-owned opening/direction/speed schedules referenced by routing
    /// edges. Runtime scenario overlays remain request-owned.
    pub temporal_rule_sets: Vec<TemporalRuleSet>,
}

impl TopologyBundle {
    pub fn edge_count(&self) -> usize {
        self.edge_layers.routing.len()
    }

    /// Returns rise over horizontal run for an edge whose endpoint
    /// elevations are both known. Vertical or zero-length segments have no
    /// finite gradient and return `None`.
    pub fn edge_gradient(&self, edge_index: usize) -> Option<f64> {
        let edge = self.routing_edge(edge_index);
        let from = self.nodes.get(edge.from.0 as usize)?;
        let to = self.nodes.get(edge.to.0 as usize)?;
        let delta_z = to.elevation_m()? - from.elevation_m()?;
        let horizontal_m = crate::geo::haversine_meters(from.lon, from.lat, to.lon, to.lat);
        (horizontal_m > f64::EPSILON).then_some(delta_z / horizontal_m)
    }

    pub fn routing_edge(&self, edge_index: usize) -> RoutingEdge {
        self.edge_layers.routing[edge_index]
    }

    pub fn edge_profile(&self, edge_index: usize) -> EdgeProfileAttributes {
        self.edge_layers.profile[edge_index]
    }

    pub fn edge_presentation(&self, edge_index: usize) -> EdgePresentation {
        self.edge_layers.presentation[edge_index]
    }

    /// Gathers the three edge layers at `edge_index` into one value.
    pub fn edge(&self, edge_index: usize) -> DirectedEdge {
        let routing = self.routing_edge(edge_index);
        let profile = self.edge_profile(edge_index);
        let presentation = self.edge_presentation(edge_index);
        DirectedEdge {
            edge_id: routing.edge_id,
            from: routing.from,
            to: routing.to,
            source_way_id: routing.source_way_id,
            length_m: routing.length_m,
            ascent_m: routing.ascent_m,
            descent_m: routing.descent_m,
            feature_row: routing.feature_row,
            source_direction: routing.source_direction,
            temporal_rule_id: routing.temporal_rule_id,
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

    /// Appends `edge` to each edge layer.
    pub fn push_edge(&mut self, edge: DirectedEdge) {
        self.edge_layers.push_directed_edge(&edge);
    }

    pub fn set_edge_name_index(&mut self, edge_index: usize, name_index: Option<u32>) {
        if let Some(edge) = self.edge_layers.presentation.get_mut(edge_index) {
            edge.name_index = name_index;
        }
    }

    pub fn edge_attribute_value(
        &self,
        edge_index: usize,
        name: &str,
    ) -> Option<FeatureAttributeValueRef<'_>> {
        let row = self.routing_edge(edge_index).feature_row;
        (row != NO_FEATURE_ROW)
            .then(|| self.feature_attributes.value(row, name))
            .flatten()
    }

    pub fn edge_attribute_matches(&self, edge_index: usize, name: &str, expected: &str) -> bool {
        let row = self.routing_edge(edge_index).feature_row;
        row != NO_FEATURE_ROW && self.feature_attributes.value_matches(row, name, expected)
    }

    pub fn set_edge_flags(&mut self, edge_index: usize, flags: u32) {
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
