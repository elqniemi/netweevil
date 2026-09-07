//! Core data types shared across the NetWeevil workspace: graph primitives
//! (nodes, directed edges, edge-based topology), persisted topology bundles,
//! compiled profile metrics, and the customizable contraction hierarchy
//! (CCH) acceleration structures.
//!
//! This crate holds plain data and invariants only; import logic lives in
//! `netweevil-ingest` and search algorithms in `netweevil-query`.

pub mod acceleration;
pub mod attributes;
pub mod dataset;
pub mod geo;
pub mod graph;
pub mod metrics;
pub mod temporal;

pub use acceleration::{
    ACCELERATION_BUNDLE_SCHEMA_VERSION, AccelerationBundleStats, CCH_ALGORITHM,
    DatasetAccelerationBundle,
};
pub use attributes::{
    FeatureAttributeColumn, FeatureAttributeColumnData, FeatureAttributeDefinition,
    FeatureAttributeTable, FeatureAttributeType, FeatureAttributeValueRef, NO_FEATURE_ROW,
    normalize_attribute_name,
};
pub use dataset::{BuildStage, CacheBundleId, DatasetId, SourceFormat, TravelMode};
pub use graph::{
    AccessMask, ConnectedComponentKind, ConnectedComponentsMeta, DirectedEdge,
    EDGE_FLAG_INFERRED_BICYCLE_CONTRAFLOW, EDGE_FLAG_INFERRED_FOOT_REVERSE_ONEWAY,
    EDGE_FLAG_ROUNDABOUT, EDGE_FLAG_TARGET_TRAFFIC_SIGNAL,
    EDGE_FLAG_TEMPORAL_MATERIALIZED_DIRECTION, EdgeBasedTopology, EdgeId, EdgeNameBundle,
    EdgePresentation, EdgeProfileAttributes, HighwayClass, NodeId, NodeSpatialIndex, RoadClass,
    RoutingEdge, SmoothnessClass, SpatialIndexCell, SurfaceClass, TOPOLOGY_BUNDLE_SCHEMA_VERSION,
    TopologyBounds, TopologyBundle, TopologyBundleMeta, TopologyEdgeLayers, TopologyNode,
    TurnRestriction, TurnRestrictionKind,
};
pub use metrics::{
    CCH_WEIGHT_INFINITY, CCH_WEIGHT_OVERFLOW, CCH_WEIGHT_SCALE,
    COMPILED_ACCELERATION_SCHEMA_VERSION, COMPILED_PROFILE_BUNDLE_SCHEMA_VERSION,
    CompiledAcceleration, CompiledCostComponent, CompiledEdgeMetric, CompiledProfileBundle,
    CompiledTemporalProfile, CompiledTurnCostConfig, add_cch_weights, decode_cch_weight,
    encode_cch_weight,
};
pub use temporal::{
    EVERY_DAY, FRIDAY, MONDAY, MinuteInterval, PUBLIC_HOLIDAY, SATURDAY, SUNDAY, THURSDAY, TUESDAY,
    TemporalEffect, TemporalRule, TemporalRuleSet, WEDNESDAY,
};
