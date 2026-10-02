//! Dataset import pipeline: scans `.osm.pbf` extracts or Overture Maps
//! transportation GeoParquet, builds the routable topology (edge-based
//! graph, turn restrictions, connected components, road classification),
//! and preprocesses the CCH acceleration bundle used by the query engine.

mod acceleration;
mod audit;
mod classify;
mod geopackage;
mod guidance;
mod import;
mod mapping;
mod opening_hours;
mod overture;
mod restrictions;
mod scan;
mod topology;
mod wkb;

pub use audit::{
    AttributeDrift, AttributeSchemaDrift, DatasetAuditReport, EdgeAttributeLinkDrift,
    GeometryDrift, GeometrySegment, audit_geopackage_dataset,
};
pub use import::{
    DatasetImportOptions, DatasetImportProgress, DatasetImportStage, detect_source_format,
    import_dataset,
};
pub use mapping::{
    CoordinateMapping, CoordinateQuantization, DirectionValue, FieldMapping,
    GeoPackageLayerMapping, GeoPackageMapping, LayerDefaults, RetainMode, load_gpkg_mapping,
};
pub use opening_hours::parse_access_schedule;

#[cfg(test)]
mod test_util;
