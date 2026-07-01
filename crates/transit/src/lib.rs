//! GTFS transit support: feed import into a compact binary bundle plus
//! scheduled transit routing and transit service areas with walk, bicycle,
//! and car street access. Street access is priced by straight-line distance
//! by default; hosts can inject a [`StreetTimeEstimator`] for real network
//! times.

mod gtfs;
mod legs;
mod model;
mod router;
mod runtime;
mod service_area;

pub use gtfs::{import_gtfs, read_transit_bundle, transit_import_summary, write_transit_bundle};
pub use model::{
    AccessMode, OPENOV_GTFS_URL, TransitAlternativeOptions, TransitBundle, TransitConnection,
    TransitFeedManifest, TransitImportOptions, TransitImportSummary, TransitLeg, TransitLegType,
    TransitMode, TransitModeOptions, TransitOutcome, TransitPoint, TransitQueryTime,
    TransitReturnOptions, TransitRoute, TransitRouteAlternative, TransitRouteRequest,
    TransitRouteResult, TransitRouteStop, TransitRouteStopSegment, TransitRouteSummary,
    TransitServiceAreaRequest, TransitServiceAreaResult, TransitServiceAreaReturnOptions,
    TransitServiceAreaSegment, TransitServiceAreaStop, TransitShape, TransitStop,
    TransitStreetAccessModel, TransitTrip, TransitWalkingGeometry, load_transit_request,
};
pub use router::{PreparedTransitRouter, execute_transit_route};
pub use runtime::StreetTimeEstimator;
pub use service_area::execute_transit_service_area;

#[cfg(test)]
mod tests;
