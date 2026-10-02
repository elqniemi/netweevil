//! Fixtures shared by the report and output-writer tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use netweevil_query::RouteResult;
use rusqlite::Connection;
use serde_json::json;

/// Unique path under the system temp dir; the file is removed on drop.
pub(crate) struct TempFile(PathBuf);

impl TempFile {
    pub(crate) fn new(name: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "netweevil-report-{}-{unique}-{name}",
            std::process::id()
        )))
    }
}

impl AsRef<Path> for TempFile {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Legal 1 km route from (6.0, 53.0) to (6.2, 53.2) over nodes 1-2-3 with a
/// 3D geometry. Tests set the summary fields and detail tables they exercise.
pub(crate) fn route_result(route_id: &str) -> RouteResult {
    serde_json::from_value(json!({
        "route_id": route_id,
        "origin": {
            "point_id": "origin", "requested_lon": 6.0, "requested_lat": 53.0,
            "snapped_node_id": 1, "snapped_lon": 6.0, "snapped_lat": 53.0, "snapped_z": 12.0,
            "snap_distance_m": 10.0, "component_id": 0
        },
        "destination": {
            "point_id": "destination", "requested_lon": 6.2, "requested_lat": 53.2,
            "snapped_node_id": 2, "snapped_lon": 6.2, "snapped_lat": 53.2, "snapped_z": 18.0,
            "snap_distance_m": 20.0, "component_id": 0
        },
        "summary": {
            "network_distance_m": 1000, "network_travel_time_s": 120.0,
            "network_generalized_cost": 120.0, "total_distance_m": 1000,
            "total_travel_time_s": 120.0, "total_generalized_cost": 120.0, "segment_count": 2
        },
        "node_path": [1, 2, 3],
        "edge_path": [10, 11],
        "geometry": [[6.0, 53.0, 12.0], [6.1, 53.1, 15.0], [6.2, 53.2, 18.0]]
    }))
    .expect("route fixture deserializes")
}

pub(crate) fn csv_header(path: impl AsRef<Path>) -> String {
    let csv = std::fs::read_to_string(path).expect("CSV readable");
    csv.lines().next().expect("CSV has a header").to_string()
}

pub(crate) fn row_count(connection: &Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap_or_else(|error| panic!("counting {table}: {error}"))
}

pub(crate) fn column_names(connection: &Connection, table: &str) -> Vec<String> {
    let mut statement = connection
        .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
        .expect("schema query prepares");
    statement
        .query_map([], |row| row.get(0))
        .expect("schema query runs")
        .collect::<rusqlite::Result<_>>()
        .expect("column names decode")
}

/// GeoPackage `z` flag for a feature table (1 = Z values present).
pub(crate) fn geometry_z_flag(connection: &Connection, table: &str) -> i64 {
    connection
        .query_row(
            "SELECT z FROM gpkg_geometry_columns WHERE table_name = ?1",
            [table],
            |row| row.get(0),
        )
        .expect("z metadata present")
}

/// ISO WKB geometry type code of the first feature, after the 8-byte GeoPackage header.
pub(crate) fn first_wkb_geometry_type(connection: &Connection, table: &str) -> u32 {
    let geometry: Vec<u8> = connection
        .query_row(&format!("SELECT geom FROM {table}"), [], |row| row.get(0))
        .expect("geometry present");
    u32::from_le_bytes(geometry[9..13].try_into().unwrap())
}
