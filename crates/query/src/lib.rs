//! The NetWeevil routing engine: exact point-to-point routes, alternatives,
//! OD batches, travel-time matrices, service areas (isochrones), and
//! accessibility over an edge-based graph with turn costs and restrictions.
//!
//! Customizable contraction hierarchies minimize the compiled fixed-point
//! metric; route summaries use original floating-point edge costs. Temporal
//! and constrained requests use exact label searches. `PreparedRoutingEngine`
//! is the shared, thread-safe entry point used by the CLI, API, and simulation.

mod accessibility;
mod alternatives;
mod batch;
mod betweenness;
mod diagnostics;
mod documents;
mod edge_spatial_index;
mod engine;
mod geometry;
mod isochrone_polygon;
mod navigation;
mod pareto;
mod provenance;
mod requests;
mod results;
mod route;
mod scenario_batch;
mod service_area;
mod service_area_sequence;
mod snapping;
mod temporal;
mod waypoints;

pub use accessibility::*;
pub(crate) use alternatives::*;
pub use batch::*;
pub(crate) use betweenness::execute_betweenness_with_graph;
pub use betweenness::{
    BetweennessEdgeScore, BetweennessPairResult, BetweennessRequest, BetweennessResult,
    WeightedPoint,
};
pub use diagnostics::*;
pub use documents::*;
pub use engine::*;
pub(crate) use geometry::*;
pub use navigation::*;
pub(crate) use pareto::*;
pub(crate) use provenance::*;
pub use requests::*;
pub use results::*;
pub use route::*;
pub use scenario_batch::*;
pub use service_area::*;
pub use service_area_sequence::*;
pub(crate) use snapping::*;
pub use temporal::{
    HolidayCalendarDocument, ScenarioFeatureOverride, ScenarioOverlay, TemporalContext,
    TemporalOverlaySeries, everyday_mask, load_holiday_calendar, load_scenario_overlay,
    load_temporal_overlay, parse_datetime,
};
pub use waypoints::*;

#[cfg(test)]
mod tests;
