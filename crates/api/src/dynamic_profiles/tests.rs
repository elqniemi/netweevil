use super::*;
use crate::{
    dto::ResponseFormatQuery,
    state::{ApiState, test_support::test_service},
};
use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use netweevil_core::*;
use serde_json::json;

/// Three-node car network (0-1-2 residential plus a 0-2 service shortcut) with a
/// CCH attached, so dynamic overrides exercise the accelerated compile path. The
/// endpoint tests below (trace matching, directions, waypoints, locate) live here
/// because building it needs this module's private `compile_engine`.
fn fixture() -> Arc<ServiceRuntime> {
    let edges = [
        (0, 1, 100, RoadClass::Residential),
        (1, 2, 200, RoadClass::Residential),
        (0, 2, 500, RoadClass::Service),
    ]
    .into_iter()
    .enumerate()
    .map(|(id, (from, to, length_m, road_class))| DirectedEdge {
        edge_id: EdgeId(id as u32),
        from: NodeId(from),
        to: NodeId(to),
        source_way_id: id as i64,
        length_m,
        road_class,
        ascent_m: 0.0,
        descent_m: 0.0,
        feature_row: NO_FEATURE_ROW,
        source_direction: 1,
        temporal_rule_id: None,
        duration_s: None,
        surface: SurfaceClass::Asphalt,
        smoothness: Default::default(),
        access_mask: AccessMask::new(AccessMask::CAR),
        is_toll: false,
        max_speed_kph: None,
        lanes: None,
        name_index: None,
        geometry_offset: 0,
        geometry_len: 0,
        flags: 0,
    })
    .collect::<Vec<_>>();
    let topology = TopologyBundle {
        schema_version: 1,
        source_path: "fixture".into(),
        source_sha256: "test".into(),
        nodes: (0..3)
            .map(|id| TopologyNode {
                node_id: NodeId(id),
                lon: 6.0 + f64::from(id) * 0.001,
                lat: 53.0,
                z: 0.0,
            })
            .collect(),
        edge_layers: TopologyEdgeLayers::from_directed_edges(&edges),
        edge_based_topology: EdgeBasedTopology {
            node_first_out: vec![0, 2, 3, 3],
            node_edge_order: vec![0, 2, 1],
            edge_transition_first_out: vec![0, 1, 1, 1],
            edge_transition_edges: vec![1],
        },
        turn_restrictions: vec![],
        names: vec![],
        spatial_index: None,
        node_component_ids: vec![0; 3],
        edge_component_ids: vec![0; 3],
        feature_attributes: Default::default(),
        temporal_rule_sets: vec![],
    };
    let document = netweevil_profile::load_profile(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/profiles/car_research_v1.yml"
    ))
    .unwrap();
    let mut service = test_service(topology, document);
    let state = Arc::get_mut(&mut service).unwrap();
    state.dynamic_profiles = DynamicProfileCache::new(1);
    let acceleration = Arc::new(DatasetAccelerationBundle {
        schema_version: ACCELERATION_BUNDLE_SCHEMA_VERSION,
        source_topology_bundle_id: CacheBundleId::new("test-topology"),
        algorithm: CCH_ALGORITHM.into(),
        stats: Default::default(),
        edge_order: vec![0, 1, 2],
        edge_rank: vec![0, 1, 2],
        upward_first_out: vec![0, 1, 1, 1],
        upward_head: vec![1],
        downward_first_out: vec![0, 0, 0, 0],
        downward_head: vec![],
    });
    state.dataset_manifest.acceleration_bundle = Some(netweevil_manifest::BundleRef {
        bundle_id: CacheBundleId::new("test-cch"),
        path: "unused".into(),
    });
    state
        .profiles
        .get_mut(&state.default_profile_id)
        .unwrap()
        .dataset_acceleration = Some(acceleration);
    let document = state.profiles[&state.default_profile_id].document.clone();
    let prepared = Arc::new(compile_engine(state, &document).unwrap());
    state
        .profiles
        .get_mut(&state.default_profile_id)
        .unwrap()
        .engine = prepared;
    service
}

async fn json_body(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn format(format: Option<&str>) -> Query<ResponseFormatQuery> {
    Query(ResponseFormatQuery {
        format: format.map(str::to_owned),
    })
}

fn json_payload<T: serde::de::DeserializeOwned>(value: Value) -> Json<T> {
    Json(serde_json::from_value(value).unwrap())
}

fn status(error: impl IntoResponse) -> StatusCode {
    error.into_response().status()
}

fn changed_speeds() -> Value {
    json!({"speed_rules": [{"match": {"highway": "residential"}, "speed_kph": 1.0}]})
}

fn request() -> netweevil_query::RouteRequest {
    serde_json::from_value(json!({"route_id":"test",
        "origin":{"id":"o","lon":6.0,"lat":53.0},
        "destination":{"id":"d","lon":6.002,"lat":53.0},
        "snap":{"max_distance_m":10}, "returns":{"geometry":"full"}
    }))
    .unwrap()
}

#[tokio::test]
async fn trace_matching_endpoint_exports_sequence_and_rejects_partial_timestamps() {
    let state = ApiState::for_test(fixture());
    let base = json!({"request": {
        "trace_id":"test-trace", "snap":{"max_distance_m":10},
        "observations": [
            {"point":{"id":"p0","lon":6.0002,"lat":53.0},"timestamp_s":0},
            {"point":{"id":"p1","lon":6.0009,"lat":53.0},"timestamp_s":10},
            {"point":{"id":"p2","lon":6.0018,"lat":53.0},"timestamp_s":20}
        ]
    }});
    for fmt in [None, Some("geojson")] {
        let response = crate::trace_matching::trace_match_handler(
            State(state.clone()),
            format(fmt),
            json_payload(base.clone()),
        )
        .await
        .unwrap();
        let body = json_body(response).await;
        let result = if fmt.is_some() {
            &body
        } else {
            &body["result"]
        };
        assert_eq!(result["trace_id"], "test-trace");
        assert!(
            result["tracepoints"]
                .as_array()
                .unwrap()
                .iter()
                .all(|point| !point.is_null())
        );
        assert_eq!(result["gaps"].as_array().unwrap().len(), 0);
        assert_eq!(
            result["confidence_method"],
            "exp_negative_mean_model_penalty_not_probability"
        );
        if fmt.is_some() {
            assert_eq!(body["type"], "FeatureCollection");
            assert_eq!(body["features"][0]["geometry"]["type"], "LineString");
            assert_eq!(
                body["features"][0]["properties"]["observation_indices"],
                json!([0, 1, 2])
            );
        } else {
            assert_eq!(result["matchings"].as_array().unwrap().len(), 1);
            assert_eq!(result["matchings"][0]["edge_path"], json!([0, 1]));
        }
    }
    let mut invalid = base;
    invalid["request"]["observations"][1]
        .as_object_mut()
        .unwrap()
        .remove("timestamp_s");
    let error = crate::trace_matching::trace_match_handler(
        State(state),
        format(None),
        json_payload(invalid),
    )
    .await
    .unwrap_err();
    assert_eq!(status(error), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn directions_endpoint_preserves_profile_context_in_json_and_geojson() {
    let service = fixture();
    service
        .edge_names
        .set(Arc::from([
            "First Road".into(),
            "Second Road".into(),
            "Service Road".into(),
        ]))
        .unwrap();
    let state = ApiState::for_test(service.clone());
    for fmt in [None, Some("geojson")] {
        let response = crate::handlers::directions_handler(
            State(state.clone()),
            format(fmt),
            json_payload(json!({"request":request()})),
        )
        .await
        .unwrap();
        let body = json_body(response).await;
        let result = if fmt.is_some() {
            &body
        } else {
            &body["result"]
        };
        assert_eq!(result["language"], "en");
        let maneuvers = result["maneuvers"].as_array().unwrap();
        assert_eq!(maneuvers.first().unwrap()["kind"], "depart");
        assert_eq!(maneuvers.last().unwrap()["kind"], "arrive");
        assert!(
            maneuvers
                .iter()
                .all(|m| !m["instruction"].as_str().unwrap().is_empty())
        );
        if fmt.is_none() {
            assert_eq!(body["service"]["profile_id"], service.default_profile_id);
            assert_eq!(result["route"]["route_id"], "test");
            assert!(result["route"]["geometry"].as_array().unwrap().len() >= 2);
        } else {
            assert_eq!(body["type"], "FeatureCollection");
            assert_eq!(
                body["features"][0]["properties"]["profile_id"],
                service.default_profile_id
            );
        }
    }
}

#[tokio::test]
async fn waypoint_endpoint_returns_legs_order_and_geojson_and_rejects_temporal_requests() {
    let state = ApiState::for_test(fixture());
    let base = json!({"request": {
        "route_id":"waypoints-test", "snap":{"max_distance_m":10},
        "waypoints":[
            {"point":{"id":"start","lon":6.0,"lat":53.0}},
            {"point":{"id":"visit","lon":6.001,"lat":53.0},"kind":"break"},
            {"point":{"id":"end","lon":6.002,"lat":53.0}}
        ]
    }});
    for (middle_kind, optimized, leg_count) in [
        ("break", false, 2),
        ("through", false, 1),
        ("break", true, 2),
    ] {
        let mut payload = base.clone();
        payload["request"]["waypoints"][1]["kind"] = json!(middle_kind);
        payload["request"]["optimize_order"] = json!(optimized);
        let response = crate::waypoints::waypoints_handler(
            State(state.clone()),
            format(None),
            json_payload(payload),
        )
        .await
        .unwrap();
        let response = json_body(response).await;
        assert_eq!(response["result"]["waypoint_order"], json!([0, 1, 2]));
        assert_eq!(
            response["result"]["legs"].as_array().unwrap().len(),
            leg_count
        );
        assert_eq!(
            response["result"]["optimization_method"],
            if optimized {
                "held_karp_exact"
            } else {
                "input_order"
            }
        );
        assert_eq!(
            response["service"]["route_engine"],
            "waypoint_dijkstra_with_turn_history"
        );
        assert!(response["result"]["total_distance_m"].as_u64().unwrap() > 0);
    }
    let response = crate::waypoints::waypoints_handler(
        State(state.clone()),
        format(Some("geojson")),
        json_payload(base.clone()),
    )
    .await
    .unwrap();
    let response = json_body(response).await;
    assert_eq!(response["type"], "FeatureCollection");
    assert_eq!(response["waypoint_order"], json!([0, 1, 2]));
    let features = response["features"].as_array().unwrap();
    assert_eq!(features.len(), 2);
    assert_eq!(features[1]["properties"]["leg_index"], 1);
    assert!(
        features[0]["geometry"]["coordinates"]
            .as_array()
            .unwrap()
            .len()
            >= 2
    );
    let mut temporal = base;
    temporal["request"]["departure_time"] = json!("2026-09-09T08:00:00Z");
    let error =
        crate::waypoints::waypoints_handler(State(state), format(None), json_payload(temporal))
            .await
            .unwrap_err();
    assert_eq!(status(error), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn trip_endpoint_orders_stops_along_one_way_streets_and_reports_failures() {
    let state = ApiState::for_test(fixture());
    // The fixture only drives east (0 -> 1 -> 2), so an open trip must visit
    // the middle stop second whatever the input order.
    let payload = json!({"request": {
        "trip_id": "trip-test", "mode": "open", "snap": {"max_distance_m": 10},
        "stops": [
            {"point": {"id": "middle", "lon": 6.001, "lat": 53.0}, "service_s": 60},
            {"point": {"id": "west", "lon": 6.0, "lat": 53.0}},
            {"point": {"id": "east", "lon": 6.002, "lat": 53.0},
             "time_window": {"earliest_s": 0, "latest_s": 3600}}
        ]
    }});
    let response = crate::trip::trip_handler(
        State(state.clone()),
        format(None),
        json_payload(payload.clone()),
    )
    .await
    .unwrap();
    let response = json_body(response).await;
    let result = &response["result"];
    assert_eq!(result["stop_order"], json!([1, 0, 2]));
    assert_eq!(result["legs"].as_array().unwrap().len(), 2);
    assert_eq!(result["visits"][1]["point_id"], "middle");
    assert_eq!(result["total_service_s"], 60.0);
    assert_eq!(result["time_windows_met"], true);
    assert_eq!(
        result["optimization"]["method"],
        "held_karp_time_windows_exact"
    );
    assert_eq!(
        response["service"]["batch_engine"],
        "held_karp_time_windows_exact"
    );

    let response = crate::trip::trip_handler(
        State(state.clone()),
        format(Some("geojson")),
        json_payload(payload.clone()),
    )
    .await
    .unwrap();
    let response = json_body(response).await;
    assert_eq!(response["type"], "FeatureCollection");
    let features = response["features"].as_array().unwrap();
    let stops = features
        .iter()
        .filter(|feature| feature["properties"]["kind"] == "trip_stop")
        .collect::<Vec<_>>();
    assert_eq!(stops.len(), 3);
    assert_eq!(stops[0]["properties"]["point_id"], "west");
    assert!(
        features
            .iter()
            .any(|feature| feature["properties"]["leg_index"] == 1)
    );

    // Nothing drives west, so no roundtrip exists.
    let mut roundtrip = payload;
    roundtrip["request"]["mode"] = json!("roundtrip");
    let error = crate::trip::trip_handler(State(state), format(None), json_payload(roundtrip))
        .await
        .unwrap_err();
    assert_eq!(status(error), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn locate_candidates_match_route_snapping_and_reject_invalid_requests() {
    let service = fixture();
    let state = ApiState::for_test(service.clone());
    for direction in ["origin", "destination"] {
        let payload = json!({"request": {
            "points": [{"id":"first","lon":6.0005,"lat":53.0},
                       {"id":"second","lon":6.0005,"lat":53.0}],
            "snap":{"max_distance_m":100}, "direction":direction
        }});
        let Json(response) =
            crate::locate::locate_handler(State(state.clone()), json_payload(payload))
                .await
                .unwrap();
        let response = serde_json::to_value(response).unwrap();
        assert_eq!(
            response["service"]["profile_id"],
            service.default_profile_id
        );
        for (index, id) in ["first", "second"].into_iter().enumerate() {
            let expected = service.profiles[&service.default_profile_id]
                .engine
                .snap_route_candidates(
                    &netweevil_query::LabeledPoint {
                        id: id.into(),
                        lon: 6.0005,
                        lat: 53.0,
                        z: None,
                    },
                    100.0,
                    direction == "origin",
                )
                .unwrap();
            assert_eq!(response["result"][index]["point_id"], id);
            assert_eq!(
                response["result"][index]["candidates"],
                serde_json::to_value(expected).unwrap()
            );
            assert!(
                response["result"][index]["candidates"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|candidate| candidate["snapped_edge_fraction"]
                        .as_f64()
                        .is_some_and(|fraction| fraction > 0.0 && fraction < 1.0))
            );
        }
    }
    // Points that match nothing report empty lists instead of failing the
    // whole request.
    for request in [
        json!({"points":[{"id":"far","lon":0,"lat":0}]}),
        json!({"points":[{"id":"high","lon":6,"lat":53,"z":50}], "snap":{"z_window_m":1}}),
        json!({"points":[{"id":"west","lon":6.0005,"lat":53}], "snap":{"point_constraints":{"west":{"bearing":{"degrees":270,"tolerance_degrees":10}}}}}),
    ] {
        let Json(response) = crate::locate::locate_handler(
            State(state.clone()),
            json_payload(json!({"request":request})),
        )
        .await
        .unwrap();
        let response = serde_json::to_value(response).unwrap();
        assert_eq!(response["result"][0]["candidates"], json!([]));
        assert_eq!(response["result"][0]["nearest"], json!([]));
    }
    for request in [
        json!({"points":[]}),
        json!({"points":[{"id":"bad","lon":181,"lat":53}]}),
        json!({"points":[{"id":"bad","lon":6,"lat":91}]}),
        json!({"points":[{"id":"bad","lon":6,"lat":53}], "count":0}),
        json!({"points":[{"id":"bad","lon":6,"lat":53}], "snap":{"max_distance_m":-1}}),
        json!({"points":[{"id":"bad","lon":6.0005,"lat":53}], "snap":{"point_constraints":{"bad":{"bearing":{"degrees":361,"tolerance_degrees":10}}}}}),
    ] {
        let error = crate::locate::locate_handler(
            State(state.clone()),
            json_payload(json!({"request":request})),
        )
        .await
        .unwrap_err();
        assert_eq!(status(error), StatusCode::BAD_REQUEST);
    }
    let error = crate::locate::locate_handler(
        State(state),
        json_payload(json!({"profile_id":"missing","request": {
            "points":[{"id":"point","lon":6,"lat":53}]
        }})),
    )
    .await
    .unwrap_err();
    assert_eq!(status(error), StatusCode::NOT_FOUND);
}

/// `count` nearest segments, nearest first, with edge attributes and the
/// profile's view of each direction.
#[tokio::test]
async fn locate_reports_k_nearest_segments_with_attributes() {
    let state = ApiState::for_test(fixture());
    let payload = json!({"request": {
        "points": [{"id":"p","lon":6.0015,"lat":53.0001}],
        "snap": {"max_distance_m": 100}, "count": 2
    }});
    let Json(response) = crate::locate::locate_handler(State(state), json_payload(payload))
        .await
        .unwrap();
    let response = serde_json::to_value(response).unwrap();
    let nearest = response["result"][0]["nearest"].as_array().unwrap();
    assert_eq!(nearest.len(), 2);
    // Edge 1 (1 -> 2) and the 0 -> 2 service road overlap at the point.
    let edges = nearest
        .iter()
        .map(|segment| segment["edges"][0]["edge_index"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(edges, vec![1, 2]);
    assert!(
        nearest[0]["distance_m"].as_f64().unwrap() <= nearest[1]["distance_m"].as_f64().unwrap()
    );
    let first = &nearest[0]["edges"][0];
    assert_eq!(first["road_class"], "residential");
    assert_eq!(first["oneway"], true);
    assert_eq!(first["access_car"], true);
    assert!((first["fraction"].as_f64().unwrap() - 0.5).abs() < 0.01);
    assert!(first["travel_time_s"].as_f64().unwrap() > 0.0);
    assert!(first["speed_kph"].as_f64().unwrap() > 0.0);
}

/// Feature ids per layer name of an encoded vector tile.
fn mvt_feature_ids(bytes: &[u8]) -> std::collections::BTreeMap<String, Vec<u64>> {
    fn varint(bytes: &[u8], at: &mut usize) -> u64 {
        let mut value = 0_u64;
        for shift in (0..64).step_by(7) {
            let byte = bytes[*at];
            *at += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                break;
            }
        }
        value
    }
    /// (field, varint value or length-delimited payload) pairs.
    fn fields(bytes: &[u8]) -> Vec<(u64, Result<u64, &[u8]>)> {
        let mut at = 0;
        let mut fields = Vec::new();
        while at < bytes.len() {
            let key = varint(bytes, &mut at);
            match key & 7 {
                0 => fields.push((key >> 3, Ok(varint(bytes, &mut at)))),
                2 => {
                    let length = varint(bytes, &mut at) as usize;
                    fields.push((key >> 3, Err(&bytes[at..at + length])));
                    at += length;
                }
                wire => panic!("unexpected wire type {wire}"),
            }
        }
        fields
    }
    let mut layers = std::collections::BTreeMap::new();
    for (_, layer) in fields(bytes).into_iter().filter(|(field, _)| *field == 3) {
        let layer = fields(layer.unwrap_err());
        let name = layer
            .iter()
            .find(|(field, _)| *field == 1)
            .map(|(_, name)| String::from_utf8(name.unwrap_err().to_vec()).unwrap())
            .unwrap();
        let ids = layer
            .iter()
            .filter(|(field, _)| *field == 2)
            .map(|(_, feature)| {
                fields(feature.unwrap_err())
                    .into_iter()
                    .find(|(field, _)| *field == 1)
                    .map(|(_, id)| id.unwrap())
                    .unwrap()
            })
            .collect();
        layers.insert(name, ids);
    }
    layers
}

#[tokio::test]
async fn network_tiles_serve_routable_edges_by_zoom() {
    let state = ApiState::for_test(fixture());
    let tile_of = |z: u8| {
        let scale = f64::from(1_u32 << z);
        let lat = 53.0_f64.to_radians();
        let x = ((6.001 + 180.0) / 360.0 * scale).floor() as u32;
        let y = ((1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0 * scale)
            .floor() as u32;
        (z, x, format!("{y}.mvt"))
    };
    let fetch = |z: u8| {
        let state = state.clone();
        async move {
            let response = crate::tiles::network_tile_handler(
                State(state),
                axum::extract::Path(tile_of(z)),
                Query(crate::tiles::TileQuery { profile_id: None }),
            )
            .await
            .unwrap();
            assert_eq!(
                response.headers()["content-type"],
                "application/vnd.mapbox-vector-tile"
            );
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            mvt_feature_ids(&bytes)
        }
    };
    let mut detailed = fetch(16).await.remove("network").unwrap();
    detailed.sort_unstable();
    assert_eq!(detailed, vec![0, 1, 2]);
    // Residential and service roads are hidden at zoom 11.
    assert_eq!(fetch(11).await["network"], Vec::<u64>::new());

    let error = crate::tiles::network_tile_handler(
        State(state),
        axum::extract::Path((3, 9, "0.mvt".to_string())),
        Query(crate::tiles::TileQuery { profile_id: None }),
    )
    .await
    .unwrap_err();
    assert_eq!(status(error), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn locate_applies_bearing_and_curb_constraints_to_directed_candidates() {
    let state = ApiState::for_test(fixture());
    for direction in ["origin", "destination"] {
        let payload = json!({"request": {
            "points": [{"id":"curb","lon":6.0005,"lat":52.9999}],
            "direction": direction,
            "snap": {"max_distance_m":20, "point_constraints": {
                "curb": {"bearing":{"degrees":90,"tolerance_degrees":5},
                         "approach":"curb", "driving_side":"right"}
            }}
        }});
        let Json(response) =
            crate::locate::locate_handler(State(state.clone()), json_payload(payload))
                .await
                .unwrap();
        let response = serde_json::to_value(response).unwrap();
        let candidates = response["result"][0]["candidates"].as_array().unwrap();
        assert!(!candidates.is_empty());
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate["snapped_edge_id"].is_u64())
        );
    }
}

#[tokio::test]
async fn concurrent_requests_compile_once_and_match_independent_exact_routing() {
    let service = fixture();
    let base = &service.profiles[&service.default_profile_id];
    let before = serde_json::to_value(base.engine.execute_route(&request()).unwrap()).unwrap();
    let (first, second) = tokio::join!(
        resolve_dynamic_profile(service.clone(), None, None, Some(changed_speeds())),
        resolve_dynamic_profile(service.clone(), None, None, Some(changed_speeds())),
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert!(Arc::ptr_eq(&first.engine, &second.engine));
    assert_eq!(
        [first.cache_status, second.cache_status]
            .iter()
            .filter(|s| **s == "miss")
            .count(),
        1
    );
    assert!(first.engine.metrics().acceleration.is_some());
    let mut raw = serde_json::to_value(&base.document).unwrap();
    merge_overrides(&mut raw, changed_speeds());
    let effective = parse_document(raw).unwrap();
    let exact_metrics = netweevil_profile::compile_profile_bundle(
        &effective,
        &service.topology,
        CacheBundleId::new("test-topology"),
    )
    .unwrap();
    let exact = PreparedRoutingEngine::new(service.topology.clone(), Arc::new(exact_metrics), None)
        .unwrap();
    let actual = first.engine.execute_route(&request()).unwrap();
    let expected = exact.execute_route(&request()).unwrap();
    assert_eq!(
        serde_json::to_value(&actual).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert_ne!(
        before["edge_path"],
        serde_json::to_value(actual).unwrap()["edge_path"]
    );
    assert_eq!(
        before,
        serde_json::to_value(base.engine.execute_route(&request()).unwrap()).unwrap()
    );
}

#[tokio::test]
async fn interleaved_profiles_share_inflight_results_even_with_one_cache_entry() {
    let service = fixture();
    let (first, other, duplicate) = tokio::join!(
        resolve_dynamic_profile(service.clone(), None, None, Some(changed_speeds())),
        resolve_dynamic_profile(
            service.clone(),
            None,
            None,
            Some(json!({"turns":{"left_penalty_s":33}}))
        ),
        resolve_dynamic_profile(service.clone(), None, None, Some(changed_speeds())),
    );
    let first = first.unwrap();
    let other = other.unwrap();
    let duplicate = duplicate.unwrap();
    assert_eq!(first.cache_status, "miss");
    assert_eq!(other.cache_status, "miss");
    assert_eq!(duplicate.cache_status, "hit");
    assert!(Arc::ptr_eq(&first.engine, &duplicate.engine));
    assert!(service.dynamic_profiles.pending.lock().unwrap().is_empty());
}

#[tokio::test]
async fn eviction_keeps_active_engines_valid_and_dataset_caches_are_separate() {
    let service = fixture();
    let first = resolve_dynamic_profile(service.clone(), None, None, Some(changed_speeds()))
        .await
        .unwrap();
    let hash = first.engine.metrics().profile_hash.clone();
    let before = serde_json::to_value(first.engine.execute_route(&request()).unwrap()).unwrap();
    let second = resolve_dynamic_profile(
        service.clone(),
        None,
        None,
        Some(json!({"turns":{"left_penalty_s":33}})),
    )
    .await
    .unwrap();
    assert_eq!(second.cache_status, "miss");
    assert!(service.dynamic_profiles.get(&hash).is_none());
    assert_eq!(service.dynamic_profiles.entries.lock().unwrap().len(), 1);
    assert_eq!(
        before,
        serde_json::to_value(first.engine.execute_route(&request()).unwrap()).unwrap()
    );
    let other = resolve_dynamic_profile(fixture(), None, None, Some(changed_speeds()))
        .await
        .unwrap();
    assert_eq!(other.cache_status, "miss");
    assert!(!Arc::ptr_eq(&first.engine, &other.engine));
}

#[tokio::test]
#[allow(
    clippy::await_holding_lock,
    reason = "Holding the cache lock proves named routes do not acquire it"
)]
async fn named_routes_bypass_compilation_and_cache_locks_and_keep_response_shape() {
    let service = fixture();
    let _slot = service
        .dynamic_profiles
        .compile_slot
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    let _cache = service.dynamic_profiles.entries.lock().unwrap();
    let response = crate::handlers::route_handler(
        State(ApiState::for_test(service.clone())),
        format(None),
        json_payload(json!({"request":request()})),
    )
    .await
    .unwrap();
    assert!(!response.headers().contains_key("x-netweevil-profile-cache"));
    let body = json_body(response).await;
    assert_eq!(
        body["result"],
        serde_json::to_value(
            service.profiles[&service.default_profile_id]
                .engine
                .execute_route(&request())
                .unwrap()
        )
        .unwrap()
    );
    assert_eq!(
        body["service"]["profile_hash"],
        service.profiles[&service.default_profile_id]
            .manifest
            .profile_hash
    );
}

#[tokio::test]
async fn dynamic_json_and_geojson_report_effective_profile_and_cache_status() {
    let service = fixture();
    let state = ApiState::for_test(service.clone());
    let payload = json!({"profile_overrides":changed_speeds(), "request":request()});
    let response = crate::handlers::route_handler(
        State(state.clone()),
        format(None),
        json_payload(payload.clone()),
    )
    .await
    .unwrap();
    assert_eq!(response.headers()["x-netweevil-profile-cache"], "miss");
    assert!(
        response.headers()["x-netweevil-profile-prepare-ms"]
            .to_str()
            .unwrap()
            .parse::<f64>()
            .unwrap()
            >= 0.0
    );
    let body = json_body(response).await;
    assert_ne!(
        body["service"]["profile_hash"],
        service.profiles[&service.default_profile_id]
            .manifest
            .profile_hash
    );
    let response = crate::handlers::route_handler(
        State(state),
        format(Some("geojson")),
        json_payload(payload),
    )
    .await
    .unwrap();
    assert_eq!(response.headers()["x-netweevil-profile-cache"], "hit");
    let geojson = json_body(response).await;
    assert_eq!(geojson["type"], "FeatureCollection");
    assert!(
        geojson
            .to_string()
            .contains(body["service"]["profile_hash"].as_str().unwrap())
    );
}

#[tokio::test]
async fn cancelled_waiter_does_not_release_compilation_slot_or_lose_result() {
    let service = fixture();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    pool.spawn(move || {
        ready_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    ready_rx.recv().unwrap();
    service.dynamic_profiles.compiler.set(pool).ok().unwrap();
    let task_service = service.clone();
    let task = tokio::spawn(async move {
        resolve_dynamic_profile(task_service, None, None, Some(changed_speeds())).await
    });
    while service.dynamic_profiles.compile_slot.available_permits() != 0 {
        tokio::task::yield_now().await;
    }
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    assert_eq!(service.dynamic_profiles.compile_slot.available_permits(), 0);
    release_tx.send(()).unwrap();
    let resolved = resolve_dynamic_profile(service, None, None, Some(changed_speeds()))
        .await
        .unwrap();
    assert_eq!(resolved.cache_status, "hit");
}

#[tokio::test]
async fn invalid_profiles_do_not_poison_cache_and_inline_profiles_share_overrides() {
    let service = fixture();
    for patch in [
        json!({"turns":{"left_penalty_s":-1}}),
        json!({"turns":{"left_penatly_s":10}}),
        json!({"speed_rules":[{"match":{"highway":"residential"},"speed_kph":0}]}),
        json!({"exclude_rules":[{"match":{"not_a_retained_attribute":"yes"}}]}),
    ] {
        let error = resolve_dynamic_profile(service.clone(), None, None, Some(patch))
            .await
            .err()
            .unwrap();
        assert_eq!(status(error), StatusCode::BAD_REQUEST);
    }
    assert!(service.dynamic_profiles.entries.lock().unwrap().is_empty());
    let preloaded = resolve_dynamic_profile(service.clone(), None, None, Some(json!({})))
        .await
        .unwrap();
    assert_eq!(preloaded.cache_status, "preloaded");
    let first = resolve_dynamic_profile(service.clone(), None, None, Some(changed_speeds()))
        .await
        .unwrap();
    let mut inline =
        serde_json::to_value(&service.profiles[&service.default_profile_id].document).unwrap();
    merge_overrides(&mut inline, changed_speeds());
    let second = resolve_dynamic_profile(service.clone(), None, Some(inline.clone()), None)
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&first.engine, &second.engine));
    assert!(
        resolve_dynamic_profile(service.clone(), Some("missing"), Some(inline), None)
            .await
            .is_err()
    );
    assert!(
        resolve_dynamic_profile(service, Some("missing"), None, Some(json!({})))
            .await
            .is_err()
    );
}
