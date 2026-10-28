use std::fs;

use netweevil_query::{OdPairsDocument, OdResult};
use serde_json::json;

use crate::output::write_od_result;
use crate::test_support::TempFile;

#[test]
fn writes_od_components_across_tabular_formats() {
    let request: OdPairsDocument = serde_json::from_value(json!({"pairs": [{
        "pair_id": "pair_1",
        "origin": {"id": "origin", "lon": 6.0, "lat": 53.0},
        "destination": {"id": "destination", "lon": 6.2, "lat": 53.2}
    }]}))
    .unwrap();
    let result: OdResult = serde_json::from_value(json!({
        "pair_count": 1, "succeeded_count": 1, "failed_count": 0,
        "pairs": [{
            "pair_id": "pair_1", "origin_id": "origin", "destination_id": "destination",
            "status": "succeeded", "origin_component_id": 0, "destination_component_id": 0,
            "origin_snap_distance_m": 1.0, "destination_snap_distance_m": 2.0,
            "total_distance_m": 1000, "total_travel_time_s": 120.0, "total_generalized_cost": 120.0,
            "components": {"Slope Cost": 3.5},
            "illegal_movement_penalty_s": 0.0, "illegal_movement_penalty_cost": 0.0,
            "geometry": [[6.0, 53.0, 12.0], [6.1, 53.1, 15.0], [6.2, 53.2, 18.0]]
        }]
    }))
    .unwrap();

    let csv = TempFile::new("od.csv");
    write_od_result(&csv, &request, &result).expect("csv written");
    let raw = fs::read_to_string(&csv).expect("csv readable");
    let header = raw.lines().next().unwrap();
    assert!(header.contains("outcome"));
    assert!(header.contains("diagnostics_json"));
    assert!(header.contains("component_slope_cost"));
    let row = raw.lines().nth(1).unwrap();
    assert!(row.ends_with(",3.5"));
    assert!(row.contains("legal"));
    assert!(row.contains("LINESTRING Z(6 53 12, 6.1 53.1 15, 6.2 53.2 18)"));

    for name in [
        "od-components.gpkg",
        "od-components.parquet",
        "od-components.geoparquet",
    ] {
        write_od_result(TempFile::new(name), &request, &result)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }

    let geojson_path = TempFile::new("od-components.geojson");
    write_od_result(&geojson_path, &request, &result).expect("OD GeoJSON written");
    let geojson = fs::read_to_string(&geojson_path).expect("OD GeoJSON readable");
    assert!(geojson.contains("\"components\""));
    assert!(geojson.contains("\"Slope Cost\": 3.5"));
}
