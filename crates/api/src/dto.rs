//! Request and response payloads for the HTTP endpoints. Analysis endpoints
//! share one request envelope (`{profile_id?, engine_mode?, request}`) and one
//! response envelope (`{service, result}`); the aliases below name the
//! concrete payload of each endpoint.

use netweevil_core::{ConnectedComponentsMeta, TopologyBounds};
use netweevil_query::{
    AccessibilityRequest, BetweennessRequest, EngineMode, OdPairsDocument, PointSetDocument,
    RouteRequest, ScenarioBatchRequest, ServiceAreaRequest, ServiceAreaSequenceRequest,
};
use netweevil_transit::{
    TransitRouteRequest, TransitRouteResult, TransitServiceAreaRequest, TransitServiceAreaResult,
};
use serde::{Deserialize, Serialize};

use crate::state::{EngineDescription, ServiceCapabilities};

#[derive(Debug, Serialize)]
pub(crate) struct HealthResponse {
    pub(crate) status: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct ServiceInfoResponse {
    pub(crate) workspace_root: String,
    pub(crate) dataset: DatasetInfo,
    pub(crate) default_profile_id: String,
    pub(crate) loaded_profiles: Vec<ProfileInfo>,
    pub(crate) loaded_transit_feeds: Vec<TransitFeedInfo>,
    pub(crate) capabilities: ServiceCapabilities,
    pub(crate) engine: EngineDescription,
}

#[derive(Debug, Serialize)]
pub(crate) struct DatasetInfo {
    pub(crate) dataset_id: String,
    pub(crate) label: String,
    pub(crate) source_path: String,
    pub(crate) source_sha256: String,
    pub(crate) imported_at: String,
    pub(crate) topology_bounds: Option<TopologyBounds>,
    pub(crate) node_count: Option<u64>,
    pub(crate) edge_count: Option<u64>,
    pub(crate) turn_count: Option<u64>,
    pub(crate) connected_components: Option<ConnectedComponentsMeta>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ProfileInfo {
    pub(crate) profile_id: String,
    pub(crate) label: String,
    pub(crate) mode: String,
    pub(crate) defaults_pack: String,
    pub(crate) source_path: String,
    pub(crate) profile_hash: String,
    pub(crate) created_at: String,
    pub(crate) edge_count: Option<u64>,
    pub(crate) default_returns: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct TransitFeedInfo {
    pub(crate) feed_id: String,
    pub(crate) source_path: String,
    pub(crate) service_start_date: String,
    pub(crate) service_days: u32,
    pub(crate) stop_count: u64,
    pub(crate) route_count: u64,
    pub(crate) trip_count: u64,
    pub(crate) connection_count: u64,
    pub(crate) bound_stop_count: u64,
    pub(crate) transfer_profile_ids: Vec<String>,
}

/// Envelope for endpoints whose caller may pick the routing engine.
#[derive(Debug, Deserialize)]
pub(crate) struct EngineModeRequest<T> {
    #[serde(default)]
    pub(crate) profile_id: Option<String>,
    #[serde(default)]
    pub(crate) engine_mode: EngineMode,
    pub(crate) request: T,
}

/// Envelope for endpoints whose engine is fixed by the analysis itself.
#[derive(Debug, Deserialize)]
pub(crate) struct ProfiledRequest<T> {
    #[serde(default)]
    pub(crate) profile_id: Option<String>,
    pub(crate) request: T,
}

pub(crate) type RouteExecutionRequest = EngineModeRequest<RouteRequest>;
pub(crate) type OdExecutionRequest = EngineModeRequest<OdPairsDocument>;
pub(crate) type MatrixExecutionRequest = EngineModeRequest<MatrixRequest>;
pub(crate) type AccessibilityExecutionRequest = EngineModeRequest<AccessibilityRequest>;
pub(crate) type ServiceAreaExecutionRequest = ProfiledRequest<ServiceAreaRequest>;
pub(crate) type ServiceAreaSequenceExecutionRequest = ProfiledRequest<ServiceAreaSequenceRequest>;
pub(crate) type BetweennessExecutionRequest = ProfiledRequest<BetweennessRequest>;
pub(crate) type ScenarioBatchExecutionRequest = ProfiledRequest<ScenarioBatchRequest>;

#[derive(Debug, Deserialize)]
pub(crate) struct TransitRouteExecutionRequest {
    pub(crate) feed_id: String,
    #[serde(default)]
    pub(crate) pedestrian_profile_id: Option<String>,
    /// Profile used to draw network geometry for non-walk access legs
    /// (bicycle or car first mile).
    #[serde(default)]
    pub(crate) access_profile_id: Option<String>,
    /// Profile used to draw network geometry for non-walk egress legs
    /// (bicycle or car last mile).
    #[serde(default)]
    pub(crate) egress_profile_id: Option<String>,
    pub(crate) request: TransitRouteRequest,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TransitServiceAreaExecutionRequest {
    pub(crate) feed_id: String,
    pub(crate) request: TransitServiceAreaRequest,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MatrixRequest {
    pub(crate) origins: PointSetDocument,
    pub(crate) destinations: PointSetDocument,
}

/// `{"service": ..., "result": ...}` body returned by every analysis endpoint.
#[derive(Debug, Serialize)]
pub(crate) struct ExecutionResponse<C, T> {
    pub(crate) service: C,
    pub(crate) result: T,
}

pub(crate) type AnalysisResponse<T> = ExecutionResponse<ExecutionContext, T>;
pub(crate) type TransitRouteExecutionResponse =
    ExecutionResponse<TransitExecutionContext, TransitRouteResult>;
pub(crate) type TransitServiceAreaExecutionResponse =
    ExecutionResponse<TransitExecutionContext, TransitServiceAreaResult>;

#[derive(Debug, Deserialize)]
pub(crate) struct ResponseFormatQuery {
    pub(crate) format: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ExecutionContext {
    pub(crate) dataset_id: String,
    pub(crate) profile_id: String,
    pub(crate) profile_hash: String,
    pub(crate) route_engine: String,
    pub(crate) batch_engine: String,
    pub(crate) acceleration: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct TransitExecutionContext {
    pub(crate) feed_id: String,
    pub(crate) service_start_date: String,
    pub(crate) service_days: u32,
    pub(crate) route_engine: String,
    pub(crate) walking_geometry: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) transfer_profile_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pedestrian_profile_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) access_profile_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) egress_profile_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessibility_execution_request_accepts_temporal_origin_options() {
        let payload: AccessibilityExecutionRequest = serde_json::from_value(serde_json::json!({
            "profile_id": "pedestrian_multilayer",
            "request": {
                "origins": {
                    "points": [{"id":"origin","lon":114.15,"lat":22.28,"z":12.0}],
                    "departure_time": "2026-07-10T09:55:00+08:00",
                    "scenario": "central.yml"
                },
                "categories": [{
                    "category_id": "stations",
                    "destinations": {"points":[{"id":"station","lon":114.16,"lat":22.29}]}
                }],
                "thresholds_s": [300.0, 600.0],
                "max_travel_time_s": 600.0
            }
        }))
        .expect("accessibility request parses");
        assert_eq!(payload.profile_id.as_deref(), Some("pedestrian_multilayer"));
        assert_eq!(
            payload.request.origins.temporal.departure_time.as_deref(),
            Some("2026-07-10T09:55:00+08:00")
        );
        assert_eq!(payload.request.categories.len(), 1);
    }

    #[test]
    fn transit_route_request_reads_transfer_profile_from_modes() {
        let payload: TransitRouteExecutionRequest = serde_json::from_value(serde_json::json!({
            "feed_id": "hk_example_mtr",
            "request": {
                "route_id": "central_to_admiralty",
                "origin": {"id": "origin", "lon": 114.158, "lat": 22.281},
                "destination": {"id": "destination", "lon": 114.164, "lat": 22.279},
                "time": {"datetime": "2026-07-10T09:55:00+08:00"},
                "modes": {"transfer_profile_id": "pedestrian_step_free_multilayer"}
            }
        }))
        .expect("transit route request parses");
        assert_eq!(
            payload.request.modes.transfer_profile_id.as_deref(),
            Some("pedestrian_step_free_multilayer")
        );
    }
}
