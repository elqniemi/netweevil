use serde::{Deserialize, Serialize};

use crate::{
    AdminArea, DrivingSide, FeatureAttributeTable, NO_FEATURE_ROW, TemporalRuleSet,
    TopologyElevation, TopologyRegions, VehicleRestrictionTable,
};

/// Schema version of [`TopologyBundle`]. Readers accept this value only.
pub const TOPOLOGY_BUNDLE_SCHEMA_VERSION: u32 = 17;

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
    /// `highway=busway`: a road reserved for buses.
    Busway,
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
            Self::Service | Self::Busway => RoadClass::Service,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct AccessMask(pub u16);

impl AccessMask {
    pub const CAR: u16 = 1 << 0;
    pub const BICYCLE: u16 = 1 << 1;
    pub const FOOT: u16 = 1 << 2;
    /// Buses (`bus`, and `psv` together with [`Self::TAXI`]).
    pub const BUS: u16 = 1 << 3;
    pub const HGV: u16 = 1 << 4;
    pub const TAXI: u16 = 1 << 5;
    pub const MOTORCYCLE: u16 = 1 << 6;
    /// Mopeds and motor scooters (`moped`).
    pub const MOPED: u16 = 1 << 7;
    /// Every motorized mode (`motor_vehicle`).
    pub const MOTOR_VEHICLES: u16 =
        Self::CAR | Self::BUS | Self::HGV | Self::TAXI | Self::MOTORCYCLE | Self::MOPED;
    /// Every vehicle (`vehicle`): motorized modes and bicycles.
    pub const VEHICLES: u16 = Self::MOTOR_VEHICLES | Self::BICYCLE;
    pub const ALL: u16 = Self::VEHICLES | Self::FOOT;

    pub const fn new(bits: u16) -> Self {
        Self(bits)
    }

    pub const fn contains(self, bit: u16) -> bool {
        self.0 & bit != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
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
    pub flags: u32,
}

/// Sentinel of an absent [`RoutingEdge::temporal_rule_id`].
const NO_TEMPORAL_RULE: u32 = u32::MAX;
/// Sentinel of an absent edge name index.
const NO_NAME: u32 = u32::MAX;

/// The per-edge fields searches read. Fields the searches do not read live
/// in parallel columns of [`TopologyEdgeLayers`]; the edge id is the
/// position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingEdge {
    pub from: NodeId,
    pub to: NodeId,
    /// Three-dimensional segment length in metres, rounded to the nearest
    /// whole metre. When either endpoint has unknown elevation this is the
    /// horizontal great-circle length.
    pub length_m: u32,
    temporal_rule: u32,
    pub flags: u32,
}

impl Default for RoutingEdge {
    fn default() -> Self {
        Self {
            from: NodeId::default(),
            to: NodeId::default(),
            length_m: 0,
            temporal_rule: NO_TEMPORAL_RULE,
            flags: 0,
        }
    }
}

impl RoutingEdge {
    pub fn new(
        from: NodeId,
        to: NodeId,
        length_m: u32,
        temporal_rule_id: Option<u32>,
        flags: u32,
    ) -> Self {
        let mut edge = Self {
            from,
            to,
            length_m,
            temporal_rule: NO_TEMPORAL_RULE,
            flags,
        };
        edge.set_temporal_rule_id(temporal_rule_id);
        edge
    }

    /// Optional edge schedule imported from the mapped source feature.
    pub fn temporal_rule_id(&self) -> Option<u32> {
        (self.temporal_rule != NO_TEMPORAL_RULE).then_some(self.temporal_rule)
    }

    pub fn set_temporal_rule_id(&mut self, temporal_rule_id: Option<u32>) {
        debug_assert_ne!(temporal_rule_id, Some(NO_TEMPORAL_RULE));
        self.temporal_rule = temporal_rule_id.unwrap_or(NO_TEMPORAL_RULE);
    }

    /// The raw column value: the rule id, or `u32::MAX` when absent.
    pub fn temporal_rule_raw(&self) -> u32 {
        self.temporal_rule
    }

    pub fn set_temporal_rule_raw(&mut self, raw: u32) {
        self.temporal_rule = raw;
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct EdgeProfileAttributes {
    pub duration_s: Option<f64>,
    pub road_class: RoadClass,
    pub highway: HighwayClass,
    pub surface: SurfaceClass,
    pub smoothness: SmoothnessClass,
    /// Modes allowed by the unconditional source tags.
    pub access_mask: AccessMask,
    /// Subset of `access_mask` whose access is limited to traffic with a
    /// destination on the edge (`destination`, `delivery`, `customers`).
    /// Profiles charge entering such an edge from one that is not.
    pub destination_only: AccessMask,
    /// Modes the unconditional tags deny but a time condition allows
    /// (`motor_vehicle=no` + `motor_vehicle:conditional=yes @ (...)`).
    /// Static routing excludes them; time-dependent routing evaluates the
    /// edge's temporal rules.
    pub conditional_access: AccessMask,
    pub is_toll: bool,
    pub max_speed_kph: Option<f32>,
    pub lanes: Option<u8>,
}

/// Bitwise identity of an [`EdgeProfileAttributes`] row, for interning.
type ProfileRowKey = (
    Option<u64>,
    [u8; 4],
    [u16; 3],
    bool,
    Option<u32>,
    Option<u8>,
);

impl EdgeProfileAttributes {
    fn row_key(&self) -> ProfileRowKey {
        (
            self.duration_s.map(f64::to_bits),
            [
                self.road_class as u8,
                self.highway as u8,
                self.surface as u8,
                self.smoothness as u8,
            ],
            [
                self.access_mask.0,
                self.destination_only.0,
                self.conditional_access.0,
            ],
            self.is_toll,
            self.max_speed_kph.map(f32::to_bits),
            self.lanes,
        )
    }
}

/// The per-edge columns of a [`TopologyBundle`], all indexed by edge
/// index: the hot [`RoutingEdge`] records, cold source and elevation
/// columns, interned profile attribute rows, and name indexes.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TopologyEdgeLayers {
    pub routing: Vec<RoutingEdge>,
    pub source_way_ids: Vec<i64>,
    /// Positive elevation gain in each edge's travel direction.
    pub ascent_m: Vec<f32>,
    /// Positive elevation loss in each edge's travel direction.
    pub descent_m: Vec<f32>,
    /// Row in the source feature attribute table shared by every segment
    /// derived from the same source feature.
    pub feature_rows: Vec<u32>,
    /// `1` follows source geometry, `-1` traverses it backwards, and `0`
    /// means the orientation is unknown.
    pub source_directions: Vec<i8>,
    /// Profile attribute rows, distinct unless rows were rewritten in
    /// place.
    profile_rows: Vec<EdgeProfileAttributes>,
    /// Per edge, its row in `profile_rows`.
    profile_row_index: Vec<u32>,
    /// Per edge, its index into [`TopologyBundle::names`], or `u32::MAX`.
    pub name_indexes: Vec<u32>,
    /// Sparse physical restrictions (height, weight, hazmat, ...) indexed
    /// by edge.
    #[serde(default)]
    pub vehicle_restrictions: VehicleRestrictionTable,
    /// Row of each interned key, covering `profile_rows[..profile_lookup_len]`.
    #[serde(skip)]
    profile_lookup: std::collections::HashMap<ProfileRowKey, u32>,
    #[serde(skip)]
    profile_lookup_len: usize,
}

impl TopologyEdgeLayers {
    /// Splits `edges` into the per-edge columns.
    pub fn from_directed_edges(edges: &[DirectedEdge]) -> Self {
        let mut layers = Self::with_capacity(edges.len());
        for edge in edges {
            layers.push_directed_edge(edge);
        }
        layers
    }

    /// Columns for `routing` with default cold values: no source way,
    /// elevation change, feature row, orientation or name, and default
    /// profile attributes.
    pub fn from_routing(routing: Vec<RoutingEdge>) -> Self {
        let edge_count = routing.len();
        let mut layers = Self::with_capacity(0);
        let row = layers.intern_profile(EdgeProfileAttributes::default());
        layers.routing = routing;
        layers.source_way_ids = vec![0; edge_count];
        layers.ascent_m = vec![0.0; edge_count];
        layers.descent_m = vec![0.0; edge_count];
        layers.feature_rows = vec![NO_FEATURE_ROW; edge_count];
        layers.source_directions = vec![0; edge_count];
        layers.profile_row_index = vec![row; edge_count];
        layers.name_indexes = vec![NO_NAME; edge_count];
        layers
    }

    pub fn with_capacity(edge_count: usize) -> Self {
        Self {
            routing: Vec::with_capacity(edge_count),
            source_way_ids: Vec::with_capacity(edge_count),
            ascent_m: Vec::with_capacity(edge_count),
            descent_m: Vec::with_capacity(edge_count),
            feature_rows: Vec::with_capacity(edge_count),
            source_directions: Vec::with_capacity(edge_count),
            profile_rows: Vec::new(),
            profile_row_index: Vec::with_capacity(edge_count),
            name_indexes: Vec::with_capacity(edge_count),
            vehicle_restrictions: VehicleRestrictionTable::default(),
            profile_lookup: std::collections::HashMap::new(),
            profile_lookup_len: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.routing.len()
    }

    pub fn is_empty(&self) -> bool {
        self.routing.is_empty()
    }

    /// Appends one edge to each column.
    pub fn push_directed_edge(&mut self, edge: &DirectedEdge) {
        self.routing.push(RoutingEdge::new(
            edge.from,
            edge.to,
            edge.length_m,
            edge.temporal_rule_id,
            edge.flags,
        ));
        self.source_way_ids.push(edge.source_way_id);
        self.ascent_m.push(edge.ascent_m);
        self.descent_m.push(edge.descent_m);
        self.feature_rows.push(edge.feature_row);
        self.source_directions.push(edge.source_direction);
        let row = self.intern_profile(EdgeProfileAttributes {
            duration_s: edge.duration_s,
            road_class: edge.road_class,
            highway: HighwayClass::from_road_class(edge.road_class),
            surface: edge.surface,
            smoothness: edge.smoothness,
            access_mask: edge.access_mask,
            destination_only: AccessMask::default(),
            conditional_access: AccessMask::default(),
            is_toll: edge.is_toll,
            max_speed_kph: edge.max_speed_kph,
            lanes: edge.lanes,
        });
        self.profile_row_index.push(row);
        self.name_indexes.push(edge.name_index.unwrap_or(NO_NAME));
    }

    /// The row of `profile` in the profile row table, appending it when
    /// new. Edges refer to rows through [`Self::profile_row_index`].
    pub fn intern_profile(&mut self, profile: EdgeProfileAttributes) -> u32 {
        for (row, attributes) in self
            .profile_rows
            .iter()
            .enumerate()
            .skip(self.profile_lookup_len)
        {
            self.profile_lookup
                .entry(attributes.row_key())
                .or_insert(row as u32);
        }
        let next = u32::try_from(self.profile_rows.len()).expect("profile rows fit in u32");
        let row = *self.profile_lookup.entry(profile.row_key()).or_insert(next);
        if row == next {
            self.profile_rows.push(profile);
        }
        self.profile_lookup_len = self.profile_rows.len();
        row
    }

    /// The profile attribute rows edges refer to.
    pub fn profile_rows(&self) -> &[EdgeProfileAttributes] {
        &self.profile_rows
    }

    /// Per edge, its row in [`Self::profile_rows`].
    pub fn profile_row_index(&self) -> &[u32] {
        &self.profile_row_index
    }

    /// Replaces the profile row table and the per-edge row column.
    pub fn set_profile_columns(&mut self, rows: Vec<EdgeProfileAttributes>, row_index: Vec<u32>) {
        self.profile_rows = rows;
        self.profile_row_index = row_index;
        self.profile_lookup.clear();
        self.profile_lookup_len = 0;
    }

    /// Applies `update` to every profile row, and so to every edge.
    pub fn update_profile_rows(&mut self, update: impl FnMut(&mut EdgeProfileAttributes)) {
        self.profile_rows.iter_mut().for_each(update);
        self.profile_lookup.clear();
        self.profile_lookup_len = 0;
    }

    /// Appends the profile row column entry of a new edge.
    pub fn push_profile_row_index(&mut self, row: u32) {
        self.profile_row_index.push(row);
    }

    /// Profile attributes of the edge at `edge_index`.
    pub fn profile(&self, edge_index: usize) -> EdgeProfileAttributes {
        self.profile_rows[self.profile_row_index[edge_index] as usize]
    }

    /// Replaces the profile attributes of the edge at `edge_index`.
    pub fn set_profile(&mut self, edge_index: usize, profile: EdgeProfileAttributes) {
        let row = self.intern_profile(profile);
        self.profile_row_index[edge_index] = row;
    }

    /// Applies `update` to the profile attributes of the edge at
    /// `edge_index`.
    pub fn update_profile(
        &mut self,
        edge_index: usize,
        update: impl FnOnce(&mut EdgeProfileAttributes),
    ) {
        let mut profile = self.profile(edge_index);
        update(&mut profile);
        self.set_profile(edge_index, profile);
    }

    pub fn name_index(&self, edge_index: usize) -> Option<u32> {
        let raw = self.name_indexes[edge_index];
        (raw != NO_NAME).then_some(raw)
    }

    /// Checks that every column has one entry per routing edge and that
    /// profile rows exist.
    pub fn validate(&self) -> Result<(), String> {
        let edge_count = self.routing.len();
        for (name, len) in [
            ("source way ids", self.source_way_ids.len()),
            ("ascents", self.ascent_m.len()),
            ("descents", self.descent_m.len()),
            ("feature rows", self.feature_rows.len()),
            ("source directions", self.source_directions.len()),
            ("profile row indexes", self.profile_row_index.len()),
            ("name indexes", self.name_indexes.len()),
        ] {
            if len != edge_count {
                return Err(format!(
                    "edge column {name} has {len} entries, expected {edge_count}"
                ));
            }
        }
        if self
            .profile_row_index
            .iter()
            .any(|row| *row as usize >= self.profile_rows.len())
        {
            return Err("edge profile row index out of range".to_string());
        }
        Ok(())
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
    /// Time condition (`restriction:conditional`). `None` means the
    /// restriction always applies.
    pub condition: Option<TurnRestrictionCondition>,
}

/// A time-dependent turn restriction. The restriction is in force while
/// its rule set evaluates *unavailable* (a `closed` rule is active, or no
/// `open_only` rule is), mirroring edge schedules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TurnRestrictionCondition {
    /// Index into [`TopologyBundle::temporal_rule_sets`].
    pub rule_set_id: u32,
    /// Whether static (time-independent) routing applies the restriction:
    /// true when the unconditional `restriction` tag carries it and the
    /// condition only lifts it, false for a `restriction:conditional` alone.
    pub applies_statically: bool,
}

impl TurnRestriction {
    pub fn applies_statically(&self) -> bool {
        self.condition
            .is_none_or(|condition| condition.applies_statically)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyBundleMeta {
    pub node_count: u64,
    pub edge_count: u64,
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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct TopologyBounds {
    pub min_lon: f64,
    pub min_lat: f64,
    pub max_lon: f64,
    pub max_lat: f64,
}

/// One node; its id is its position. Coordinates keep full `f64`
/// precision: GeoPackage sources resolve vertices finer than OSM's 1e-7
/// degrees, and sub-metre edges take their cost from node coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TopologyNode {
    lon: f64,
    lat: f64,
    /// Elevation in the source dataset's vertical datum, in metres.
    /// `NaN` means that the elevation is unknown.
    z: f64,
}

impl TopologyNode {
    /// A node at `lon`/`lat` degrees with elevation `z` metres (`NaN` when
    /// unknown).
    pub const fn new(lon: f64, lat: f64, z: f64) -> Self {
        Self { lon, lat, z }
    }

    pub const fn lon(&self) -> f64 {
        self.lon
    }

    pub const fn lat(&self) -> f64 {
        self.lat
    }

    /// Elevation in metres; `NaN` when unknown.
    pub const fn z(&self) -> f64 {
        self.z
    }

    /// Returns the elevation when the source supplied a finite value.
    pub fn elevation_m(&self) -> Option<f64> {
        self.z.is_finite().then_some(self.z)
    }

    pub fn set_lon(&mut self, lon: f64) {
        self.lon = lon;
    }

    pub fn set_lat(&mut self, lat: f64) {
        self.lat = lat;
    }

    pub fn set_z(&mut self, z: f64) {
        self.z = z;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SpatialIndexCell {
    pub node_start: u32,
    pub node_len: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeSpatialIndex {
    pub bounds: TopologyBounds,
    pub columns: u32,
    pub rows: u32,
    pub cell_width_deg: f64,
    pub cell_height_deg: f64,
    pub cells: Vec<SpatialIndexCell>,
    pub node_ids: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EdgeBasedTopology {
    pub node_first_out: Vec<u32>,
    pub node_edge_order: Vec<u32>,
    pub edge_transition_first_out: Vec<u32>,
    pub edge_transition_edges: Vec<u32>,
}

impl EdgeBasedTopology {
    /// Builds the node-outgoing CSR (edges in index order per node) and
    /// the complete edge-to-edge transition CSR: every edge continues into
    /// every outgoing edge of its head node, u-turns included.
    /// `endpoints(edge_index)` returns `(from, to)` node indexes.
    pub fn build(
        node_count: usize,
        edge_count: usize,
        endpoints: impl Fn(usize) -> (u32, u32),
    ) -> Self {
        let mut node_first_out = vec![0_u32; node_count + 1];
        let mut head = vec![0_u32; edge_count];
        for (edge_index, head_slot) in head.iter_mut().enumerate() {
            let (from, to) = endpoints(edge_index);
            node_first_out[from as usize + 1] += 1;
            *head_slot = to;
        }
        for node_index in 0..node_count {
            node_first_out[node_index + 1] += node_first_out[node_index];
        }

        let mut node_edge_order = vec![0_u32; edge_count];
        let mut write_positions = node_first_out[..node_count].to_vec();
        for edge_index in 0..edge_count {
            let (from, _) = endpoints(edge_index);
            let write_index = &mut write_positions[from as usize];
            node_edge_order[*write_index as usize] = edge_index as u32;
            *write_index += 1;
        }

        let mut edge_transition_first_out = vec![0_u32; edge_count + 1];
        for edge_index in 0..edge_count {
            let head_node = head[edge_index] as usize;
            edge_transition_first_out[edge_index + 1] = edge_transition_first_out[edge_index]
                + (node_first_out[head_node + 1] - node_first_out[head_node]);
        }

        let mut edge_transition_edges =
            Vec::with_capacity(edge_transition_first_out[edge_count] as usize);
        for &head_node in &head {
            let head_node = head_node as usize;
            edge_transition_edges.extend_from_slice(
                &node_edge_order
                    [node_first_out[head_node] as usize..node_first_out[head_node + 1] as usize],
            );
        }

        Self {
            node_first_out,
            node_edge_order,
            edge_transition_first_out,
            edge_transition_edges,
        }
    }
}

impl NodeSpatialIndex {
    /// Uniform grid of roughly 750 m cells over the node bounds; each cell
    /// lists its node ids in node order.
    pub fn build(nodes: &[TopologyNode]) -> Option<Self> {
        const TARGET_CELL_SPAN_M: f64 = 750.0;
        const METERS_PER_DEGREE_LAT: f64 = 111_320.0;

        let first = nodes.first()?;
        let mut bounds = TopologyBounds {
            min_lon: first.lon(),
            min_lat: first.lat(),
            max_lon: first.lon(),
            max_lat: first.lat(),
        };
        for node in nodes.iter().skip(1) {
            bounds.min_lon = bounds.min_lon.min(node.lon());
            bounds.min_lat = bounds.min_lat.min(node.lat());
            bounds.max_lon = bounds.max_lon.max(node.lon());
            bounds.max_lat = bounds.max_lat.max(node.lat());
        }

        let mid_lat = ((bounds.min_lat + bounds.max_lat) / 2.0).to_radians();
        let cos_lat = mid_lat.cos().abs().max(0.2);
        let cell_height_deg = (TARGET_CELL_SPAN_M / METERS_PER_DEGREE_LAT).max(f64::EPSILON);
        let cell_width_deg =
            (TARGET_CELL_SPAN_M / (METERS_PER_DEGREE_LAT * cos_lat)).max(f64::EPSILON);
        let columns =
            (((bounds.max_lon - bounds.min_lon) / cell_width_deg).floor() as u32).saturating_add(1);
        let rows = (((bounds.max_lat - bounds.min_lat) / cell_height_deg).floor() as u32)
            .saturating_add(1);
        let cell_count = columns as usize * rows as usize;
        let cell_of = |node: &TopologyNode| {
            let column = (((node.lon() - bounds.min_lon) / cell_width_deg).floor() as i64)
                .clamp(0, columns.saturating_sub(1) as i64) as usize;
            let row = (((node.lat() - bounds.min_lat) / cell_height_deg).floor() as i64)
                .clamp(0, rows.saturating_sub(1) as i64) as usize;
            row * columns as usize + column
        };

        let mut counts = vec![0_u32; cell_count];
        for node in nodes {
            counts[cell_of(node)] += 1;
        }
        let mut cells = vec![SpatialIndexCell::default(); cell_count];
        let mut next_offset = 0_u32;
        for (cell, count) in cells.iter_mut().zip(&counts) {
            cell.node_start = next_offset;
            cell.node_len = *count;
            next_offset += *count;
        }
        let mut write_positions = cells
            .iter()
            .map(|cell| cell.node_start as usize)
            .collect::<Vec<_>>();
        let mut node_ids = vec![0_u32; nodes.len()];
        for (node_index, node) in nodes.iter().enumerate() {
            let cell_index = cell_of(node);
            node_ids[write_positions[cell_index]] = node_index as u32;
            write_positions[cell_index] += 1;
        }

        Some(Self {
            bounds,
            columns,
            rows,
            cell_width_deg,
            cell_height_deg,
            cells,
            node_ids,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyBundle {
    pub schema_version: u32,
    pub source_path: String,
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
    /// Per-node administrative areas and timezones.
    pub regions: TopologyRegions,
    /// DEM provenance and per-edge grades; empty without a DEM.
    pub elevation: TopologyElevation,
}

impl TopologyBundle {
    pub fn edge_count(&self) -> usize {
        self.edge_layers.routing.len()
    }

    pub fn routing_edge(&self, edge_index: usize) -> RoutingEdge {
        self.edge_layers.routing[edge_index]
    }

    pub fn edge_profile(&self, edge_index: usize) -> EdgeProfileAttributes {
        self.edge_layers.profile(edge_index)
    }

    pub fn edge_name_index(&self, edge_index: usize) -> Option<u32> {
        self.edge_layers.name_index(edge_index)
    }

    pub fn edge_source_way_id(&self, edge_index: usize) -> i64 {
        self.edge_layers.source_way_ids[edge_index]
    }

    /// Positive elevation gain in the edge's travel direction.
    pub fn edge_ascent_m(&self, edge_index: usize) -> f32 {
        self.edge_layers.ascent_m[edge_index]
    }

    /// Positive elevation loss in the edge's travel direction.
    pub fn edge_descent_m(&self, edge_index: usize) -> f32 {
        self.edge_layers.descent_m[edge_index]
    }

    pub fn edge_feature_row(&self, edge_index: usize) -> u32 {
        self.edge_layers.feature_rows[edge_index]
    }

    pub fn edge_source_direction(&self, edge_index: usize) -> i8 {
        self.edge_layers.source_directions[edge_index]
    }

    /// Gathers every edge column at `edge_index` into one value.
    pub fn edge(&self, edge_index: usize) -> DirectedEdge {
        let routing = self.routing_edge(edge_index);
        let profile = self.edge_profile(edge_index);
        DirectedEdge {
            edge_id: EdgeId(edge_index as u32),
            from: routing.from,
            to: routing.to,
            source_way_id: self.edge_source_way_id(edge_index),
            length_m: routing.length_m,
            ascent_m: self.edge_ascent_m(edge_index),
            descent_m: self.edge_descent_m(edge_index),
            feature_row: self.edge_feature_row(edge_index),
            source_direction: self.edge_source_direction(edge_index),
            temporal_rule_id: routing.temporal_rule_id(),
            duration_s: profile.duration_s,
            road_class: profile.road_class,
            surface: profile.surface,
            smoothness: profile.smoothness,
            access_mask: profile.access_mask,
            is_toll: profile.is_toll,
            max_speed_kph: profile.max_speed_kph,
            lanes: profile.lanes,
            name_index: self.edge_name_index(edge_index),
            flags: routing.flags,
        }
    }

    /// Appends `edge` to each edge column.
    pub fn push_edge(&mut self, edge: DirectedEdge) {
        self.edge_layers.push_directed_edge(&edge);
    }

    pub fn set_edge_name_index(&mut self, edge_index: usize, name_index: Option<u32>) {
        if let Some(slot) = self.edge_layers.name_indexes.get_mut(edge_index) {
            *slot = name_index.unwrap_or(NO_NAME);
        }
    }

    pub fn edge_attribute_matches(&self, edge_index: usize, name: &str, expected: &str) -> bool {
        let row = self.edge_feature_row(edge_index);
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

    /// Administrative area of an edge: that of its tail node.
    pub fn edge_admin_area(&self, edge_index: usize) -> Option<&AdminArea> {
        let from = self.edge_layers.routing.get(edge_index)?.from.0 as usize;
        self.regions.node_admin_area(from)
    }

    /// Whether an edge runs between two different known countries.
    pub fn edge_crosses_border(&self, edge_index: usize) -> bool {
        let Some(edge) = self.edge_layers.routing.get(edge_index) else {
            return false;
        };
        match (
            self.regions.node_country(edge.from.0 as usize),
            self.regions.node_country(edge.to.0 as usize),
        ) {
            (Some(from), Some(to)) => from != to,
            _ => false,
        }
    }

    /// IANA timezone of an edge: that of its tail node.
    pub fn edge_timezone(&self, edge_index: usize) -> Option<&str> {
        let from = self.edge_layers.routing.get(edge_index)?.from.0 as usize;
        self.regions.node_timezone(from)
    }

    /// Traffic side of the country an edge lies in, when known.
    pub fn edge_driving_side(&self, edge_index: usize) -> Option<DrivingSide> {
        self.edge_admin_area(edge_index)
            .and_then(AdminArea::driving_side)
    }
}

#[cfg(test)]
mod tests;
