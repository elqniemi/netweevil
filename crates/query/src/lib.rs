//! The NetWeevil routing engine: exact point-to-point routes, alternatives,
//! OD batches, travel-time matrices, service areas (isochrones), and
//! accessibility over an edge-based graph with turn costs and restrictions.
//!
//! Every surface is exact — accelerated paths (customizable contraction
//! hierarchies) return the same costs as the exact engine, with no budgets
//! or heuristics. `PreparedRoutingEngine` is the shared, thread-safe entry
//! point used by the CLI, the HTTP API, and the simulation.

mod accessibility;
mod alternatives;
mod batch;
mod betweenness;
mod diagnostics;
mod documents;
mod engine;
mod geometry;
mod isochrone_polygon;
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

#[cfg(test)]
mod tests;
