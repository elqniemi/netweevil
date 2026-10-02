use std::collections::BTreeMap;

use netweevil_query::RouteRequest;
use rusqlite::Connection;
use serde_json::json;

use crate::output::write_route_result;
use crate::test_support::{
    TempFile, csv_header, first_wkb_geometry_type, geometry_z_flag, route_result, row_count,
};

#[test]
fn writes_route_components_and_temporal_metadata_across_tabular_formats() {
    let request: RouteRequest = serde_json::from_value(json!({
        "route_id": "route_1",
        "origin": {"id": "origin", "lon": 6.0, "lat": 53.0},
        "destination": {"id": "destination", "lon": 6.2, "lat": 53.2}
    }))
    .unwrap();
    let mut result = route_result("route_1");
    let summary = &mut result.summary;
    summary.components = BTreeMap::from([("Sun Exposure".to_string(), 14.5)]);
    summary.waiting_time_s = 30.0;
    summary.departure_time = Some("2026-07-10T10:00:00Z".to_string());
    summary.arrival_time = Some("2026-07-10T10:02:30Z".to_string());
    summary.scenario_id = Some("summer_weekday".to_string());

    let gpkg = TempFile::new("route.gpkg");
    write_route_result(&gpkg, &request, &result).expect("gpkg written");
    let connection = Connection::open(&gpkg).expect("gpkg opens");
    assert_eq!(row_count(&connection, "route_result"), 1);
    assert_eq!(geometry_z_flag(&connection, "route_result"), 1);
    assert_eq!(first_wkb_geometry_type(&connection, "route_result"), 1002);
    let row: (f64, f64, String, String, String) = connection
        .query_row(
            "SELECT component_sun_exposure, waiting_time_s, departure_time, arrival_time, scenario_id FROM route_result",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .expect("component and temporal columns present");
    assert_eq!(
        row,
        (
            14.5,
            30.0,
            "2026-07-10T10:00:00Z".to_string(),
            "2026-07-10T10:02:30Z".to_string(),
            "summer_weekday".to_string()
        )
    );

    let csv = TempFile::new("route-components.csv");
    write_route_result(&csv, &request, &result).expect("route CSV written");
    let header = csv_header(&csv);
    assert!(header.contains("component_sun_exposure"));
    assert!(header.contains("departure_time"));

    for name in ["route-components.parquet", "route-components.geoparquet"] {
        write_route_result(TempFile::new(name), &request, &result)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}
