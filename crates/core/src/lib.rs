//! Core data types shared across the NetWeevil workspace: graph primitives
//! (nodes, directed edges, edge-based topology), persisted topology bundles,
//! compiled profile metrics, and the customizable contraction hierarchy
//! (CCH) acceleration structures.
//!
//! This crate holds plain data and invariants only; import logic lives in
//! `netweevil-ingest` and search algorithms in `netweevil-query`.

pub mod acceleration;
pub mod dataset;
pub mod geo;
pub mod graph;
pub mod metrics;

pub use acceleration::{
    ACCELERATION_BUNDLE_SCHEMA_VERSION, AccelerationBundleStats, CCH_ALGORITHM,
    DatasetAccelerationBundle,
};
pub use dataset::{BuildStage, CacheBundleId, DatasetId, SourceFormat, TravelMode};
pub use graph::{
    AccessMask, ConnectedComponentKind, ConnectedComponentsMeta, DirectedEdge,
    EDGE_FLAG_INFERRED_BICYCLE_CONTRAFLOW, EDGE_FLAG_INFERRED_FOOT_REVERSE_ONEWAY,
    EDGE_FLAG_ROUNDABOUT, EDGE_FLAG_TARGET_TRAFFIC_SIGNAL, EdgeBasedTopology, EdgeId,
    EdgeNameBundle, EdgePresentation, EdgeProfileAttributes, HighwayClass, NodeId,
    NodeSpatialIndex, RoadClass, RoutingEdge, SmoothnessClass, SpatialIndexCell, SurfaceClass,
    TopologyBounds, TopologyBundle, TopologyBundleMeta, TopologyEdgeLayers, TopologyNode,
    TurnRestriction, TurnRestrictionKind,
};
pub use metrics::{
    CompiledAcceleration, CompiledEdgeMetric, CompiledProfileBundle, CompiledTurnCostConfig,
    NO_MIDDLE,
};
