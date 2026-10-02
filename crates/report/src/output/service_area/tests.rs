use std::fs::{self, File};

use netweevil_query::{ServiceAreaRequest, ServiceAreaResult};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use rusqlite::Connection;
use serde_json::json;

use crate::output::write_service_area_result;
use crate::test_support::{TempFile, column_names, csv_header};

const COMPONENT_COLUMNS: [&str; 2] = ["start_component_slope_cost", "end_component_slope_cost"];

#[test]
fn writes_service_area_components_and_temporal_metadata_across_formats() {
    let request: ServiceAreaRequest = serde_json::from_value(json!({
        "analysis_id": "sa_demo",
        "origins": [{"id": "origin", "lon": 6.0, "lat": 53.0}],
        "output_mode": "both", "band_mode": "cumulative",
        "boundary_mode": "cut_at_boundary", "multi_origin_mode": "overlap"
    }))
    .unwrap();
    let result: ServiceAreaResult = serde_json::from_value(json!({
        "analysis_id": "sa_demo",
        "departure_time": "2026-07-10T10:00:00Z", "scenario_id": "mall_closure",
        "output_mode": "both", "band_mode": "cumulative",
        "boundary_mode": "cut_at_boundary", "multi_origin_mode": "overlap",
        "origin_count": 1, "processed_origin_count": 1, "threshold_count": 1,
        "features": [{
            "origin_id": "origin", "threshold_id": "t1", "threshold_limit": 300.0,
            "threshold_metric": "distance_m", "geometry_type": "segment",
            "origin_component_id": 0, "reachable_network_length_m": 300.0,
            "reachable_edge_count": 2, "edge_id": 7, "edge_index": 7,
            "start_fraction": 0.0, "end_fraction": 1.0, "start_cost": 10.0, "end_cost": 20.0,
            "segment_distance_m": 300.0, "segment_travel_time_s": 10.0,
            "start_components": {"Slope Cost": 1.5}, "end_components": {"Slope Cost": 3.5},
            "geometry": {"type": "MultiLineString", "coordinates": [[[6.0, 53.0], [6.1, 53.1]]]}
        }]
    }))
    .unwrap();

    let geojson_path = TempFile::new("service_area.geojson");
    write_service_area_result(&geojson_path, &request, &result).expect("geojson written");
    let geojson = fs::read_to_string(&geojson_path).expect("geojson readable");
    for expected in [
        "FeatureCollection",
        "sa_demo",
        "MultiLineString",
        "2026-07-10T10:00:00Z",
        "mall_closure",
        "\"start_components\"",
        "\"end_components\"",
    ] {
        assert!(geojson.contains(expected), "{expected}");
    }

    let csv = TempFile::new("service-area-temporal.csv");
    write_service_area_result(&csv, &request, &result).expect("service area CSV written");
    let header = csv_header(&csv);
    assert!(header.contains("departure_time"));
    for column in COMPONENT_COLUMNS {
        assert!(header.contains(column), "{column}");
    }
    let raw = fs::read_to_string(&csv).unwrap();
    assert!(raw.lines().nth(1).unwrap().contains(",1.5,3.5,"));
    assert!(raw.contains("mall_closure"));

    let gpkg = TempFile::new("service-area-temporal.gpkg");
    write_service_area_result(&gpkg, &request, &result).expect("service area GeoPackage written");
    let columns = column_names(
        &Connection::open(&gpkg).expect("GeoPackage opens"),
        "service_area_segments",
    );
    for column in COMPONENT_COLUMNS {
        assert!(columns.iter().any(|name| name == column), "{column}");
    }

    for name in [
        "service-area-temporal.parquet",
        "service-area-temporal.geoparquet",
    ] {
        let path = TempFile::new(name);
        write_service_area_result(&path, &request, &result)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let schema = ParquetRecordBatchReaderBuilder::try_new(File::open(&path).unwrap())
            .expect("Parquet metadata reads")
            .schema()
            .clone();
        for column in COMPONENT_COLUMNS {
            assert!(schema.field_with_name(column).is_ok(), "{name}: {column}");
        }
    }
}
