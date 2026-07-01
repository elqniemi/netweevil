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
mod diagnostics;
mod documents;
mod engine;
mod geometry;
mod isochrone_polygon;
mod requests;
mod results;
mod route;
mod service_area;
mod snapping;

pub use accessibility::*;
pub(crate) use alternatives::*;
pub use batch::*;
pub use diagnostics::*;
pub use documents::*;
pub use engine::*;
pub(crate) use geometry::*;
pub use requests::*;
pub use results::*;
pub use route::*;
pub use service_area::*;
pub(crate) use snapping::*;

#[cfg(test)]
mod tests;
