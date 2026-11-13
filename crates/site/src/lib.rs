//! Indoor sites placed on the globe, and routes that use them.
//!
//! A site is one `netweevil-indoor` engine input in local metres plus a
//! [`LocalFrame`](netweevil_core::LocalFrame) that maps its coordinates to
//! WGS84. Its entrances join it to the street network.
//!
//! - [`store`]: compile an input, find its entrances and write the site
//!   under `.netweevil/` (manifest types live in `netweevil-persist`).
//! - [`runtime`]: load sites lazily into a shared cache ([`SiteCatalog`]).
//! - [`route`]: indoor routes by geographic coordinates, POI or space.
//! - [`geojson`]: site elements and routes as GeoJSON.
//!
//! The crate sits above `netweevil-query` (street routing) and
//! `netweevil-persist` (state layout) so that the API and the CLI share one
//! implementation; neither of those crates depends on the indoor engine.

pub use netweevil_indoor;

pub mod geojson;
pub mod route;
pub mod runtime;
pub mod store;
#[cfg(test)]
mod tests;

pub use route::{
    IndoorPoint, IndoorProfileSpec, IndoorRouteRequest, IndoorRouteResult, IndoorRouteSegment,
    IndoorRouteSummary, LevelRef, route_indoor,
};
pub use runtime::{IndoorSite, SiteCatalog};
pub use store::{BuiltSite, build_site};
