//! GTFS transit support: feed import into a compact binary bundle plus
//! scheduled transit routing and transit service areas with walk, bicycle,
//! and car street access. Routes and service areas run either depart-after
//! (earliest arrival) or arrive-by (latest departure). Street access is priced
//! by straight-line distance by default; hosts can inject a
//! [`StreetTimeEstimator`] for real network times.

mod backward;
mod fusion;
mod gtfs;
mod legs;
mod model;
mod router;
mod runtime;
mod service_area;
mod timetable_time;

pub use fusion::{
    apply_transit_stop_bindings, build_transit_transfer_table, load_transit_stop_bindings,
    network_transfer, read_transit_transfer_table, write_transit_transfer_table,
};
pub use gtfs::{import_gtfs, read_transit_bundle, transit_import_summary, write_transit_bundle};
pub use model::{
    AccessMode, TRANSIT_BUNDLE_SCHEMA_VERSION, TRANSIT_STOP_BINDING_SCHEMA_VERSION,
    TRANSIT_TRANSFER_TABLE_SCHEMA_VERSION, TransitAlternativeOptions, TransitBundle,
    TransitConnection, TransitFeedManifest, TransitImportOptions, TransitImportSummary, TransitLeg,
    TransitLegType, TransitMode, TransitModeOptions, TransitNetworkTransfer, TransitOutcome,
    TransitPoint, TransitQueryTime, TransitReturnOptions, TransitRoute, TransitRouteAlternative,
    TransitRouteRequest, TransitRouteResult, TransitRouteStop, TransitRouteStopSegment,
    TransitRouteSummary, TransitServiceAreaRequest, TransitServiceAreaResult,
    TransitServiceAreaReturnOptions, TransitServiceAreaSegment, TransitServiceAreaStop,
    TransitShape, TransitStop, TransitStopBinding, TransitStopBindingSummary,
    TransitStopBindingTable, TransitStopBindingTarget, TransitStreetAccessModel, TransitStreetPath,
    TransitTimeContext, TransitTransferBuildOptions, TransitTransferTable,
    TransitTransferTableManifest, TransitTrip, TransitWalkingGeometry, load_transit_request,
};
pub use router::{PreparedTransitRouter, execute_transit_route};
pub use runtime::StreetTimeEstimator;
pub use service_area::execute_transit_service_area;

#[cfg(test)]
mod tests;
