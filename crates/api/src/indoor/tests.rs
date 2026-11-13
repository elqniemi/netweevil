use std::sync::Arc;

use axum::body::to_bytes;
use axum::http::StatusCode;
use netweevil_core::LocalFrame;
use netweevil_persist::WorkspacePaths;
use netweevil_persist::indoor::IndoorSiteSource;
use netweevil_site::build_site;
use serde_json::{Value, json};

use super::*;
use crate::simulation::SimulationRegistry;
use crate::state::Workspace;

fn state_with_demo_site() -> ApiState {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let paths = WorkspacePaths::discover(
        std::env::temp_dir().join(format!("netweevil-indoor-api-{unique}")),
    )
    .unwrap();
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/indoor/demo_site.json"
    ))
    .unwrap();
    let input = netweevil_site::netweevil_indoor::parse_input(&text).unwrap();
    let source = IndoorSiteSource {
        path: "demo_site.json".into(),
        format: "input_json".into(),
        crs: None,
        bbox: None,
    };
    build_site(
        &paths,
        "demo",
        None,
        input,
        LocalFrame::new(6.5665, 53.2194),
        source,
    )
    .unwrap();
    ApiState {
        workspace: Arc::new(Workspace::new(paths, None)),
        simulations: Arc::new(SimulationRegistry::default()),
    }
}

async fn body(response: Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
}

#[tokio::test]
async fn indoor_endpoints_work_without_a_dataset() {
    let state = state_with_demo_site();
    let Json(sites) = list_sites(State(state.clone())).await.unwrap();
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].entrance_count, 2);

    let Json(manifest) = get_site(State(state.clone()), AxumPath("demo".into()))
        .await
        .unwrap();
    assert_eq!(manifest.levels.len(), 2);
    let missing = get_site(State(state.clone()), AxumPath("nope".into())).await;
    assert_eq!(
        missing.unwrap_err().into_response().status(),
        StatusCode::NOT_FOUND
    );

    let geojson = site_geojson_handler(
        State(state.clone()),
        AxumPath("demo".into()),
        Query(SiteGeojsonQuery {
            level: Some("1".into()),
        }),
    )
    .await
    .unwrap();
    assert_eq!(geojson.headers()["content-type"], "application/geo+json");
    let geojson = body(geojson).await;
    assert!(!geojson["features"].as_array().unwrap().is_empty());

    let request: IndoorRouteRequest = serde_json::from_value(json!({
        "site_id": "demo",
        "origin": {"poi": "Workshop"},
        "destination": {"poi": "1.14"},
        "profile": "wheelchair"
    }))
    .unwrap();
    let response = indoor_route_handler(
        State(state.clone()),
        Query(ResponseFormatQuery { format: None }),
        Json(request.clone()),
    )
    .await
    .unwrap();
    let route = body(response).await;
    assert!(route["summary"]["duration_s"].as_f64().unwrap() > 0.0);
    assert!(
        route["segments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["kind"] == "lift")
    );
    let response = indoor_route_handler(
        State(state.clone()),
        Query(ResponseFormatQuery {
            format: Some("geojson".into()),
        }),
        Json(request),
    )
    .await
    .unwrap();
    assert_eq!(body(response).await["type"], "FeatureCollection");

    let bad: IndoorRouteRequest = serde_json::from_value(json!({
        "site_id": "demo",
        "origin": {"poi": "Nowhere"},
        "destination": {"poi": "1.14"}
    }))
    .unwrap();
    let err = indoor_route_handler(
        State(state),
        Query(ResponseFormatQuery { format: None }),
        Json(bad),
    )
    .await
    .unwrap_err();
    assert_eq!(err.into_response().status(), StatusCode::BAD_REQUEST);
}
