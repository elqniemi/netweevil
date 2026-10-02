//! Dataset import pipeline: scans `.osm.pbf` extracts or Overture Maps
//! transportation GeoParquet, builds the routable topology (edge-based
//! graph, turn restrictions, connected components, road classification),
//! and preprocesses the CCH acceleration bundle used by the query engine.
//! Imports also assign each node its IANA timezone and, optionally, its
//! administrative area and DEM height.

mod acceleration;
mod access;
mod admin;
mod audit;
mod classify;
mod conditional;
pub mod dem;
mod elevation;
mod geopackage;
mod guidance;
mod import;
mod mapping;
mod opening_hours;
mod osm_change;
mod overture;
mod restrictions;
mod scan;
mod source;
mod timezones;
mod topology;
mod update;
mod vehicle_tags;
mod wkb;

pub use audit::{
    AttributeDrift, AttributeSchemaDrift, DatasetAuditReport, EdgeAttributeLinkDrift,
    GeometryDrift, GeometrySegment, audit_geopackage_dataset,
};
pub use dem::ElevationSampler;
pub use import::{
    DatasetImportOptions, DatasetImportProgress, DatasetImportStage, DemImportOptions,
    detect_source_format, import_dataset,
};
pub use mapping::{
    CoordinateMapping, CoordinateQuantization, DirectionValue, FieldMapping,
    GeoPackageLayerMapping, GeoPackageMapping, LayerDefaults, RetainMode, load_gpkg_mapping,
};
pub use opening_hours::parse_access_schedule;
pub use update::{
    ChangedObjects, DEFAULT_MAX_FILL_GROWTH, DatasetUpdateInput, DatasetUpdateOptions,
    DatasetUpdateReport, ReorderPolicy, update_dataset,
};

#[cfg(test)]
mod test_util;
