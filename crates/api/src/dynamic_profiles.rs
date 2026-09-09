//! In-memory, request-defined street profiles. Named-profile requests never
//! enter this cache or acquire its locks. The cache belongs to one immutable
//! dataset runtime, so hashes cannot cross topology/acceleration versions.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use netweevil_profile::{ProfileDocument, compile_profile_bundle_with_acceleration};
use netweevil_query::PreparedRoutingEngine;
use serde_json::Value;
use tokio::sync::{Semaphore, oneshot};
use tracing::info;

use crate::error::ApiError;
use crate::state::{ServiceRuntime, resolve_profile};

pub(crate) struct DynamicProfileCache {
    capacity: usize,
    entries: Mutex<VecDeque<(String, Arc<PreparedRoutingEngine>)>>,
    compile_slot: Arc<Semaphore>,
    compiler: OnceLock<rayon::ThreadPool>,
    pending: Mutex<HashMap<String, Vec<oneshot::Sender<CompileResult>>>>,
}

type CompileResult = Result<(Arc<PreparedRoutingEngine>, bool), ApiError>;

impl DynamicProfileCache {
    pub(crate) fn from_env() -> Result<Self> {
        let capacity = match std::env::var("NETWEEVIL_DYNAMIC_PROFILE_CACHE_ENTRIES") {
            Ok(value) => value.parse::<usize>().context(
                "NETWEEVIL_DYNAMIC_PROFILE_CACHE_ENTRIES must be an integer from 1 to 16",
            )?,
            Err(std::env::VarError::NotPresent) => 2,
            Err(error) => return Err(error.into()),
        };
        if !(1..=16).contains(&capacity) {
            bail!("NETWEEVIL_DYNAMIC_PROFILE_CACHE_ENTRIES must be from 1 to 16");
        }
        Ok(Self::new(capacity))
    }

    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Mutex::new(VecDeque::new()),
            compile_slot: Arc::new(Semaphore::new(1)),
            compiler: OnceLock::new(),
            pending: Mutex::new(HashMap::new()),
        }
    }

    fn get(&self, hash: &str) -> Option<Arc<PreparedRoutingEngine>> {
        let mut entries = self.entries.lock().expect("dynamic profile cache poisoned");
        let index = entries.iter().position(|(key, _)| key == hash)?;
        let entry = entries.remove(index).expect("cache entry exists");
        let engine = Arc::clone(&entry.1);
        entries.push_back(entry);
        Some(engine)
    }

    fn insert(&self, hash: String, engine: Arc<PreparedRoutingEngine>) {
        let evicted = {
            let mut entries = self.entries.lock().expect("dynamic profile cache poisoned");
            let evicted = if entries.len() == self.capacity {
                entries.pop_front()
            } else {
                None
            };
            entries.push_back((hash, engine));
            evicted
        };
        // Free potentially large engines outside the cache lock. Active
        // routes keep their own Arc and remain valid after eviction.
        drop(evicted);
    }
}

pub(crate) struct ResolvedDynamicProfile {
    pub(crate) engine: Arc<PreparedRoutingEngine>,
    pub(crate) cache_status: &'static str,
    /// Includes waiting for the compilation slot on a cold request.
    pub(crate) prepare_ms: f64,
}

pub(crate) async fn resolve_dynamic_profile(
    service: Arc<ServiceRuntime>,
    profile_id: Option<&str>,
    inline: Option<Value>,
    overrides: Option<Value>,
) -> Result<ResolvedDynamicProfile, ApiError> {
    let started = Instant::now();
    if inline.is_some() && (profile_id.is_some() || overrides.is_some()) {
        return Err(ApiError::bad_request(
            "profile cannot be combined with profile_id or profile_overrides",
        ));
    }
    let raw = if let Some(inline) = inline {
        inline
    } else {
        let base = resolve_profile(&service, profile_id)?;
        let mut raw = serde_json::to_value(&base.document)
            .map_err(|error| ApiError::internal(error.to_string()))?;
        let overrides =
            overrides.ok_or_else(|| ApiError::bad_request("missing profile_overrides"))?;
        if !overrides.is_object() {
            return Err(ApiError::bad_request("profile_overrides must be an object"));
        }
        merge_overrides(&mut raw, overrides);
        raw
    };
    let document = parse_document(raw).map_err(ApiError::from_execution_error)?;
    let hash = document
        .fingerprint()
        .map_err(ApiError::from_execution_error)?;

    // An empty or equivalent override can reuse an already prepared profile.
    if let Some(profile) = service
        .profiles
        .values()
        .find(|p| p.manifest.profile_hash == hash)
    {
        return Ok(ResolvedDynamicProfile {
            engine: Arc::clone(&profile.engine),
            cache_status: "preloaded",
            prepare_ms: started.elapsed().as_secs_f64() * 1000.0,
        });
    }
    if let Some(engine) = service.dynamic_profiles.get(&hash) {
        return Ok(ResolvedDynamicProfile {
            engine,
            cache_status: "hit",
            prepare_ms: started.elapsed().as_secs_f64() * 1000.0,
        });
    }

    let (sender, receiver) = oneshot::channel();
    let leader = {
        let mut pending = service
            .dynamic_profiles
            .pending
            .lock()
            .expect("pending profiles poisoned");
        let waiters = pending.entry(hash.clone()).or_default();
        let leader = waiters.is_empty();
        waiters.push(sender);
        leader
    };
    if leader {
        // The task owns compilation independently of any one HTTP waiter.
        // Deliver the same Arc to every waiter even if another profile has
        // evicted this one before a waiter gets scheduled again.
        tokio::spawn(async move {
            let result = compile_queued_profile(service.clone(), document, hash.clone()).await;
            let waiters = service
                .dynamic_profiles
                .pending
                .lock()
                .expect("pending profiles poisoned")
                .remove(&hash)
                .unwrap_or_default();
            for waiter in waiters {
                let _ = waiter.send(result.clone());
            }
        });
    }
    let (engine, compiled) = receiver
        .await
        .map_err(|error| ApiError::internal(format!("profile compiler stopped: {error}")))??;
    Ok(ResolvedDynamicProfile {
        engine,
        cache_status: if leader && compiled { "miss" } else { "hit" },
        prepare_ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}

async fn compile_queued_profile(
    service: Arc<ServiceRuntime>,
    document: ProfileDocument,
    hash: String,
) -> CompileResult {
    let permit = service
        .dynamic_profiles
        .compile_slot
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    tokio::task::spawn_blocking(move || {
        // Keep the permit and cache insertion inside the job: cancelling an
        // HTTP waiter must not release the slot while compilation still runs.
        let _permit = permit;
        let cache = &service.dynamic_profiles;
        if let Some(engine) = cache.get(&hash) {
            return Ok((engine, false));
        }
        if cache.compiler.get().is_none() {
            let compiler = rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .thread_name(|_| "profile-compiler".to_string())
                .build()
                .map_err(|error| ApiError::internal(error.to_string()))?;
            let _ = cache.compiler.set(compiler);
        }
        let compile_started = Instant::now();
        let engine = cache
            .compiler
            .get()
            .expect("compiler initialized")
            .install(|| compile_engine(&service, &document))
            .map_err(ApiError::from_execution_error)?;
        let engine = Arc::new(engine);
        cache.insert(hash.clone(), Arc::clone(&engine));
        info!(profile_hash = %hash,
            compile_ms = compile_started.elapsed().as_secs_f64() * 1000.0,
            cache_capacity = cache.capacity, "request profile prepared");
        Ok((engine, true))
    })
    .await
    .map_err(|error| ApiError::internal(format!("profile compiler failed: {error}")))?
}

fn compile_engine(
    service: &ServiceRuntime,
    document: &ProfileDocument,
) -> Result<PreparedRoutingEngine> {
    let topology_ref = service
        .dataset_manifest
        .topology_bundle
        .as_ref()
        .context("dataset is missing its topology bundle")?;
    let acceleration = service.profiles[&service.default_profile_id]
        .dataset_acceleration
        .clone();
    let metrics = compile_profile_bundle_with_acceleration(
        document,
        &service.topology,
        topology_ref.bundle_id.clone(),
        service
            .dataset_manifest
            .acceleration_bundle
            .as_ref()
            .zip(acceleration.as_ref())
            .map(|(reference, bundle)| (bundle.as_ref(), reference.bundle_id.clone())),
    )?;
    // No manifests or bundles are written for request-defined profiles.
    PreparedRoutingEngine::new(
        Arc::clone(&service.topology),
        Arc::new(metrics),
        acceleration,
    )
}

/// Objects merge recursively; arrays and scalar values replace the old value.
/// Null is a literal value, useful for clearing an optional slope model.
fn merge_overrides(base: &mut Value, overrides: Value) {
    match (base, overrides) {
        (Value::Object(base), Value::Object(overrides)) => {
            for (key, value) in overrides {
                merge_overrides(base.entry(key).or_insert(Value::Null), value);
            }
        }
        (base, value) => *base = value,
    }
}

fn parse_document(raw: Value) -> Result<ProfileDocument> {
    let document: ProfileDocument =
        serde_json::from_value(raw.clone()).context("invalid profile")?;
    // Reject misspelled override fields before compiling the profile.
    reject_unknown_fields(&raw, &serde_json::to_value(&document)?, "profile")?;
    document.validate()?;
    for value in [
        document.turns.left_penalty_s,
        document.turns.right_penalty_s,
        document.turns.uturn_penalty_s,
        document.turns.traffic_signal_penalty_s,
        document.turns.roundabout_entry_penalty_s,
        document.preferences.service_penalty_s,
        document.ferry.boarding_cost_s,
    ] {
        if !value.is_finite() || value < 0.0 {
            bail!("profile penalties must be finite and non-negative");
        }
    }
    Ok(document)
}

fn reject_unknown_fields(raw: &Value, normalized: &Value, path: &str) -> Result<()> {
    match (raw, normalized) {
        (Value::Object(raw), Value::Object(normalized)) => {
            for (key, value) in raw {
                let path = format!("{path}.{key}");
                let expected = normalized
                    .get(key)
                    .with_context(|| format!("unknown field {path}"))?;
                reject_unknown_fields(value, expected, &path)?;
            }
        }
        (Value::Array(raw), Value::Array(normalized)) => {
            for (index, (value, expected)) in raw.iter().zip(normalized).enumerate() {
                reject_unknown_fields(value, expected, &format!("{path}[{index}]"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        dto::ResponseFormatQuery,
        state::{ApiState, test_service},
    };
    use axum::{
        Json,
        extract::{Query, State},
        response::IntoResponse,
    };
    use netweevil_core::*;
    use serde_json::json;

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
        let state = ApiState {
            service: fixture(),
            simulations: Arc::new(crate::simulation::SimulationRegistry::default()),
        };
        let base = json!({"request": {
            "trace_id":"test-trace", "snap":{"max_distance_m":10},
            "observations": [
                {"point":{"id":"p0","lon":6.0002,"lat":53.0},"timestamp_s":0},
                {"point":{"id":"p1","lon":6.0009,"lat":53.0},"timestamp_s":10},
                {"point":{"id":"p2","lon":6.0018,"lat":53.0},"timestamp_s":20}
            ]
        }});
        for format in [None, Some("geojson")] {
            let response = crate::trace_matching::trace_match_handler(
                State(state.clone()),
                Query(ResponseFormatQuery {
                    format: format.map(str::to_owned),
                }),
                Json(serde_json::from_value(base.clone()).unwrap()),
            )
            .await
            .unwrap();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            let result = if format.is_some() {
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
            if format.is_some() {
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
            Query(ResponseFormatQuery { format: None }),
            Json(serde_json::from_value(invalid).unwrap()),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.into_response().status(),
            axum::http::StatusCode::BAD_REQUEST
        );
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
        let state = ApiState {
            service: service.clone(),
            simulations: Arc::new(crate::simulation::SimulationRegistry::default()),
        };
        for format in [None, Some("geojson")] {
            let response = crate::handlers::directions_handler(
                State(state.clone()),
                Query(ResponseFormatQuery {
                    format: format.map(str::to_string),
                }),
                Json(serde_json::from_value(json!({"request":request()})).unwrap()),
            )
            .await
            .unwrap();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            let result = if format.is_some() {
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
            if format.is_none() {
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
        let state = ApiState {
            service: fixture(),
            simulations: Arc::new(crate::simulation::SimulationRegistry::default()),
        };
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
                Query(ResponseFormatQuery { format: None }),
                Json(serde_json::from_value(payload).unwrap()),
            )
            .await
            .unwrap();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let response: Value = serde_json::from_slice(&bytes).unwrap();
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
            Query(ResponseFormatQuery {
                format: Some("geojson".into()),
            }),
            Json(serde_json::from_value(base.clone()).unwrap()),
        )
        .await
        .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let response: Value = serde_json::from_slice(&bytes).unwrap();
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
        let error = crate::waypoints::waypoints_handler(
            State(state),
            Query(ResponseFormatQuery { format: None }),
            Json(serde_json::from_value(temporal).unwrap()),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.into_response().status(),
            axum::http::StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn locate_candidates_match_route_snapping_and_reject_invalid_requests() {
        let service = fixture();
        let state = ApiState {
            service: service.clone(),
            simulations: Arc::new(crate::simulation::SimulationRegistry::default()),
        };
        for direction in ["origin", "destination"] {
            let payload = json!({"request": {
                "points": [{"id":"first","lon":6.0005,"lat":53.0},
                           {"id":"second","lon":6.0005,"lat":53.0}],
                "snap":{"max_distance_m":100}, "direction":direction
            }});
            let Json(response) = crate::locate::locate_handler(
                State(state.clone()),
                Json(serde_json::from_value(payload).unwrap()),
            )
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
        for request in [
            json!({"points":[]}),
            json!({"points":[{"id":"bad","lon":181,"lat":53}]}),
            json!({"points":[{"id":"bad","lon":6,"lat":91}]}),
            json!({"points":[{"id":"far","lon":0,"lat":0}]}),
            json!({"points":[{"id":"bad","lon":6,"lat":53}], "snap":{"max_distance_m":-1}}),
            json!({"points":[{"id":"bad","lon":6,"lat":53,"z":50}], "snap":{"z_window_m":1}}),
            json!({"points":[{"id":"bad","lon":6.0005,"lat":53}], "snap":{"point_constraints":{"bad":{"bearing":{"degrees":361,"tolerance_degrees":10}}}}}),
            json!({"points":[{"id":"bad","lon":6.0005,"lat":53}], "snap":{"point_constraints":{"bad":{"bearing":{"degrees":270,"tolerance_degrees":10}}}}}),
        ] {
            let error = crate::locate::locate_handler(
                State(state.clone()),
                Json(serde_json::from_value(json!({"request":request})).unwrap()),
            )
            .await
            .unwrap_err();
            assert_eq!(
                error.into_response().status(),
                axum::http::StatusCode::BAD_REQUEST
            );
        }
        let error = crate::locate::locate_handler(
            State(state),
            Json(
                serde_json::from_value(json!({"profile_id":"missing","request": {
                    "points":[{"id":"point","lon":6,"lat":53}]
                }}))
                .unwrap(),
            ),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.into_response().status(),
            axum::http::StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn locate_applies_bearing_and_curb_constraints_to_directed_candidates() {
        let state = ApiState {
            service: fixture(),
            simulations: Arc::new(crate::simulation::SimulationRegistry::default()),
        };
        for direction in ["origin", "destination"] {
            let payload = json!({"request": {
                "points": [{"id":"curb","lon":6.0005,"lat":52.9999}],
                "direction": direction,
                "snap": {"max_distance_m":20, "point_constraints": {
                    "curb": {"bearing":{"degrees":90,"tolerance_degrees":5},
                             "approach":"curb", "driving_side":"right"}
                }}
            }});
            let Json(response) = crate::locate::locate_handler(
                State(state.clone()),
                Json(serde_json::from_value(payload).unwrap()),
            )
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
        let exact =
            PreparedRoutingEngine::new(service.topology.clone(), Arc::new(exact_metrics), None)
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
            State(ApiState {
                service: service.clone(),
                simulations: Arc::new(crate::simulation::SimulationRegistry::default()),
            }),
            Query(ResponseFormatQuery { format: None }),
            Json(serde_json::from_value(json!({"request":request()})).unwrap()),
        )
        .await
        .unwrap();
        assert!(!response.headers().contains_key("x-netweevil-profile-cache"));
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
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
        let state = ApiState {
            service: service.clone(),
            simulations: Arc::new(crate::simulation::SimulationRegistry::default()),
        };
        let payload = json!({"profile_overrides":changed_speeds(), "request":request()});
        let response = crate::handlers::route_handler(
            State(state.clone()),
            Query(ResponseFormatQuery { format: None }),
            Json(serde_json::from_value(payload.clone()).unwrap()),
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
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_ne!(
            body["service"]["profile_hash"],
            service.profiles[&service.default_profile_id]
                .manifest
                .profile_hash
        );
        let response = crate::handlers::route_handler(
            State(state),
            Query(ResponseFormatQuery {
                format: Some("geojson".into()),
            }),
            Json(serde_json::from_value(payload).unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(response.headers()["x-netweevil-profile-cache"], "hit");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let geojson: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(geojson["type"], "FeatureCollection");
        assert!(
            String::from_utf8(bytes.to_vec())
                .unwrap()
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
            assert_eq!(
                error.into_response().status(),
                axum::http::StatusCode::BAD_REQUEST
            );
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
}
