use super::*;
use crate::state::test_support::test_service;
use netweevil_core::{
    DemFormat, DemSourceInfo, ELEVATION_RASTER_SCHEMA_VERSION, ElevationRasterHeader, RasterGrid,
};
use serde_json::{Value, json};

fn request(value: Value) -> HeightRequest {
    serde_json::from_value(value).unwrap()
}

#[test]
fn polylines_round_trip_at_both_precisions() {
    // Google's reference example at precision 5.
    let points = decode_polyline("_p~iF~ps|U_ulLnnqC_mqNvxq`@", 5).unwrap();
    assert_eq!(
        points,
        vec![[-120.2, 38.5], [-120.95, 40.7], [-126.453, 43.252]]
    );
    assert_eq!(encode_polyline(&points, 5), "_p~iF~ps|U_ulLnnqC_mqNvxq`@");
    let precise = vec![[6.566_512, 53.219_383], [6.571_604, 53.214_801]];
    assert_eq!(
        decode_polyline(&encode_polyline(&precise, 6), 6).unwrap(),
        precise
    );
    assert!(decode_polyline("_p~iF", 5).is_err());
    assert!(decode_polyline("_p~i\u{1}", 5).is_err());
}

#[test]
fn shapes_resample_at_even_intervals_and_keep_the_end() {
    // About 111.2 m along the equator.
    let shape = [[0.0, 0.0], [0.0005, 0.0], [0.001, 0.0]];
    let resampled = resample(&shape, 50.0, 100).unwrap();
    assert_eq!(resampled.len(), 4);
    assert_eq!(resampled[0], [0.0, 0.0]);
    assert!(
        (resampled[1][0] - 0.000_449_7).abs() < 1e-6,
        "{resampled:?}"
    );
    assert_eq!(resampled[3], [0.001, 0.0]);
    assert!(resample(&shape, 1.0, 50).is_err());

    let result = height_result(
        &request(json!({"points": shape, "range": true, "resample_distance_m": 50})),
        resampled,
        vec![Some(1.0), None, Some(2.0), Some(3.0)],
        false,
    );
    let range = result.range_heights.unwrap();
    assert_eq!(range[1], (50.0, None));
    assert_eq!(range[3].0, 111.2);
    assert_eq!(result.shape.unwrap().len(), 4);
    assert!(result.heights.is_none() && result.encoded_polyline.is_none());
}

#[test]
fn requests_need_exactly_one_valid_shape() {
    for (body, message) in [
        (json!({}), "requires points"),
        (
            json!({"points": [[0, 0]], "encoded_polyline": "??"}),
            "not both",
        ),
        (json!({"points": [[200, 0]]}), "longitude"),
        (
            json!({"points": [[0, 0]], "resample_distance_m": 0.5}),
            "at least",
        ),
        (
            json!({"encoded_polyline": "??", "polyline_precision": 7}),
            "5 or 6",
        ),
    ] {
        let error = request_shape(&request(body)).unwrap_err();
        assert!(error.contains(message), "{error}");
    }
    let (shape, encoded) = request_shape(&request(
        json!({"encoded_polyline": "_p~iF~ps|U", "polyline_precision": 5}),
    ))
    .unwrap();
    assert_eq!((shape, encoded), (vec![[-120.2, 38.5]], true));
}

#[tokio::test]
async fn the_endpoint_samples_the_dataset_raster() {
    let topology = netweevil_core::TopologyBundle {
        schema_version: netweevil_core::TOPOLOGY_BUNDLE_SCHEMA_VERSION,
        source_path: "empty".into(),
        nodes: vec![netweevil_core::TopologyNode {
            node_id: netweevil_core::NodeId(0),
            lon: 6.0,
            lat: 53.0,
            z: 0.0,
        }],
        edge_layers: Default::default(),
        turn_restrictions: Vec::new(),
        names: Vec::new(),
        edge_based_topology: netweevil_core::EdgeBasedTopology::build(1, 0, |_| (0, 0)),
        spatial_index: None,
        node_component_ids: vec![0],
        edge_component_ids: Vec::new(),
        feature_attributes: Default::default(),
        temporal_rule_sets: Vec::new(),
        regions: Default::default(),
        elevation: Default::default(),
    };
    let document = netweevil_profile::load_profile(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/profiles/car_research_v1.yml"
    ))
    .unwrap();
    let mut service = test_service(topology, document);
    let state = ApiState::for_test(Arc::clone(&service));
    let error = height_handler(
        State(state),
        Json(request(json!({"points": [[6.0, 53.0]]}))),
    )
    .await
    .expect_err("no raster");
    let response = axum::response::IntoResponse::into_response(error);
    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);

    // A 2x2 geographic raster of 0.01 degree cells holding 10, 20, 30, 40.
    let grid = RasterGrid {
        columns: 2,
        rows: 2,
        min_x: 6.0,
        max_y: 53.02,
        cell_width: 0.01,
        cell_height: 0.01,
    };
    let header = ElevationRasterHeader {
        schema_version: ELEVATION_RASTER_SCHEMA_VERSION,
        source: DemSourceInfo {
            path: "dem.asc".into(),
            format: DemFormat::AsciiGrid,
            crs: RasterCrs::Geographic,
            vertical_datum: Some("NAP".into()),
            grid,
        },
        grid,
        tile_size: 256,
    };
    let path = std::env::temp_dir().join(format!("netweevil-height-{}.bin", std::process::id()));
    netweevil_persist::write_elevation_raster(&path, &header, &[vec![10.0, 20.0, 30.0, 40.0]])
        .unwrap();
    let runtime = Arc::get_mut(&mut service).unwrap();
    runtime.dataset_manifest.elevation_bundle = Some(netweevil_persist::manifest::BundleRef {
        bundle_id: netweevil_core::CacheBundleId::new("elevation-test"),
        path: path.display().to_string(),
    });
    // The first request cached the absence of a raster.
    runtime.elevation = std::sync::OnceLock::new();
    let state = ApiState::for_test(service);
    let Json(response) = height_handler(
        State(state),
        Json(request(
            json!({"points": [[6.005, 53.015], [6.01, 53.01], [7.0, 53.0]]}),
        )),
    )
    .await
    .unwrap();
    assert_eq!(response.service.vertical_datum.as_deref(), Some("NAP"));
    assert_eq!(response.service.crs, "EPSG:4326");
    assert_eq!(
        response.result.heights,
        Some(vec![Some(10.0), Some(25.0), None])
    );
    std::fs::remove_file(path).unwrap();
}
