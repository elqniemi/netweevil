use std::sync::{Arc, OnceLock};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use netweevil_core::*;
use netweevil_query::TrafficService;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::*;
use crate::dto::ResponseFormatQuery;
use crate::state::ApiState;
use crate::state::test_support::test_runtime;

/// 0 -> 1 -> 2 along two residential ways (1 and 2) plus a slower 0 -> 2
/// service way (3), with live traffic enabled for the car profile.
fn state() -> ApiState {
    let edge = |id: u32, from: u32, to: u32, way: i64, length_m: u32, road_class| DirectedEdge {
        edge_id: EdgeId(id),
        from: NodeId(from),
        to: NodeId(to),
        source_way_id: way,
        length_m,
        ascent_m: 0.0,
        descent_m: 0.0,
        feature_row: NO_FEATURE_ROW,
        source_direction: 1,
        temporal_rule_id: None,
        duration_s: None,
        road_class,
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
    };
    let edges = [
        edge(0, 0, 1, 1, 100, RoadClass::Residential),
        edge(1, 1, 2, 2, 200, RoadClass::Residential),
        edge(2, 0, 2, 3, 500, RoadClass::Service),
    ];
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
        elevation: Default::default(),
        regions: Default::default(),
    };
    let document = netweevil_profile::load_profile(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/profiles/car_research_v1.yml"
    ))
    .unwrap();
    let mut runtime = test_runtime(topology, document);
    let service = TrafficService::new(
        Arc::clone(&runtime.topology),
        None,
        runtime
            .profiles
            .iter()
            .map(|(id, profile)| (id.clone(), Arc::clone(&profile.engine))),
        2,
    )
    .unwrap();
    runtime.traffic = Some(TrafficRuntime {
        service: Arc::new(service),
        worker: OnceLock::new(),
    });
    ApiState::for_test(Arc::new(runtime))
}

async fn body(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn departure() -> String {
    (OffsetDateTime::now_utc() + time::Duration::minutes(5))
        .format(&Rfc3339)
        .unwrap()
}

async fn route(state: &ApiState, departure: &str) -> Value {
    let payload = serde_json::from_value(json!({"request": {
        "route_id": "traffic",
        "origin": {"id": "o", "lon": 6.0, "lat": 53.0},
        "destination": {"id": "d", "lon": 6.002, "lat": 53.0},
        "snap": {"max_distance_m": 10},
        "returns": {"geometry": "full"},
        "departure_time": departure
    }}))
    .unwrap();
    let response = crate::handlers::route_handler(
        State(state.clone()),
        Query(ResponseFormatQuery { format: None }),
        Json(payload),
    )
    .await
    .unwrap();
    body(response).await
}

async fn live(state: &ApiState, payload: Value) -> Result<Response, crate::error::ApiError> {
    post_live_traffic(
        State(state.clone()),
        Query(LiveTrafficQuery { wait: true }),
        Json(serde_json::from_value(payload).unwrap()),
    )
    .await
}

/// A live closure published through the API reroutes timed requests and
/// shows up in the response's traffic block and in `/v1/traffic`;
/// clearing it restores the original route. Invalid batches are rejected
/// before they reach the worker.
#[tokio::test]
async fn live_closure_reroutes_timed_requests_until_cleared() {
    let state = state();
    let departure = departure();

    let before = route(&state, &departure).await;
    assert_eq!(
        before["service"]["traffic"]["engine"],
        "departure_bucket_cch"
    );
    assert_eq!(before["result"]["edge_path"], json!([0, 1]));

    let report = body(
        live(
            &state,
            json!({"updates": [{"way_id": 2, "closed": true, "ttl_s": 3600}]}),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(report["version"], 1);
    assert_eq!(report["active_entry_count"], 1);

    let closed = route(&state, &departure).await;
    assert_eq!(closed["result"]["edge_path"], json!([2]));
    assert_eq!(closed["service"]["traffic"]["live_entries_applied"], 1);
    assert_eq!(closed["service"]["traffic"]["live_version"], 1);

    let status = traffic_status(State(state.clone())).await.unwrap().0;
    let status = serde_json::to_value(status).unwrap();
    assert_eq!(status["enabled"], true);
    assert_eq!(status["live_entry_count"], 1);

    let invalid = live(
        &state,
        json!({"updates": [{"feature_id": 2, "closed": true, "speed_kph": 10}]}),
    )
    .await
    .unwrap_err();
    assert_eq!(invalid.into_response().status(), StatusCode::BAD_REQUEST);

    delete_live_traffic(State(state.clone()), Query(LiveTrafficQuery { wait: true }))
        .await
        .unwrap();
    let reopened = route(&state, &departure).await;
    assert_eq!(reopened["result"]["edge_path"], json!([0, 1]));
}
