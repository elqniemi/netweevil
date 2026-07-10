use netweevil_core::{ConnectedComponentsMeta, TopologyBounds};
use netweevil_query::{
    AccessibilityRequest, AccessibilityResult, BetweennessRequest, BetweennessResult, EngineMode,
    MatrixResult, OdPairsDocument, OdResult, PointSetDocument, RouteRequest, RouteResult,
    ScenarioBatchRequest, ScenarioBatchResult, ServiceAreaRequest, ServiceAreaResult,
    ServiceAreaSequenceRequest, ServiceAreaSequenceResult,
};
use netweevil_transit::{
    TransitRouteRequest, TransitRouteResult, TransitServiceAreaRequest, TransitServiceAreaResult,
};
use serde::{Deserialize, Serialize};

use crate::state::{EngineDescription, ServiceCapabilities};

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub(crate) status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ServiceInfoResponse {
    pub(crate) workspace_root: String,
    pub(crate) dataset: DatasetInfo,
    pub(crate) default_profile_id: String,
    pub(crate) loaded_profiles: Vec<ProfileInfo>,
    pub(crate) loaded_transit_feeds: Vec<TransitFeedInfo>,
    pub(crate) capabilities: ServiceCapabilities,
    pub(crate) engine: EngineDescription,
}

#[derive(Debug, Serialize)]
pub struct DatasetInfo {
    pub(crate) dataset_id: String,
    pub(crate) label: String,
    pub(crate) source_path: String,
    pub(crate) source_sha256: String,
    pub(crate) imported_at: String,
    #[serde(default)]
    pub(crate) topology_bounds: Option<TopologyBounds>,
    #[serde(default)]
    pub(crate) node_count: Option<u64>,
    #[serde(default)]
    pub(crate) edge_count: Option<u64>,
    #[serde(default)]
    pub(crate) turn_count: Option<u64>,
    #[serde(default)]
    pub(crate) connected_components: Option<ConnectedComponentsMeta>,
}

#[derive(Debug, Serialize)]
pub struct ProfileInfo {
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
pub struct TransitFeedInfo {
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

#[derive(Debug, Deserialize)]
pub struct RouteExecutionRequest {
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub engine_mode: EngineMode,
    pub request: RouteRequest,
}

#[derive(Debug, Deserialize)]
pub struct OdExecutionRequest {
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub engine_mode: EngineMode,
    pub request: OdPairsDocument,
}

#[derive(Debug, Deserialize)]
pub struct MatrixExecutionRequest {
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub engine_mode: EngineMode,
    pub request: MatrixRequest,
}

#[derive(Debug, Deserialize)]
pub struct AccessibilityExecutionRequest {
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub engine_mode: EngineMode,
    pub request: AccessibilityRequest,
}

#[derive(Debug, Deserialize)]
pub struct ServiceAreaExecutionRequest {
    #[serde(default)]
    pub profile_id: Option<String>,
    pub request: ServiceAreaRequest,
}

#[derive(Debug, Deserialize)]
pub struct ServiceAreaSequenceExecutionRequest {
    #[serde(default)]
    pub profile_id: Option<String>,
    pub request: ServiceAreaSequenceRequest,
}

#[derive(Debug, Deserialize)]
pub struct BetweennessExecutionRequest {
    #[serde(default)]
    pub profile_id: Option<String>,
    pub request: BetweennessRequest,
}

#[derive(Debug, Deserialize)]
pub struct ScenarioBatchExecutionRequest {
    #[serde(default)]
    pub profile_id: Option<String>,
    pub request: ScenarioBatchRequest,
}

#[derive(Debug, Deserialize)]
pub struct TransitRouteExecutionRequest {
    pub feed_id: String,
    /// Optional shorthand for `request.modes.transfer_profile_id`.
    #[serde(default)]
    pub transfer_profile_id: Option<String>,
    #[serde(default)]
    pub pedestrian_profile_id: Option<String>,
    /// Profile used to draw network geometry for non-walk access legs
    /// (bicycle or car first mile).
    #[serde(default)]
    pub access_profile_id: Option<String>,
    /// Profile used to draw network geometry for non-walk egress legs
    /// (bicycle or car last mile).
    #[serde(default)]
    pub egress_profile_id: Option<String>,
    pub request: TransitRouteRequest,
}

#[derive(Debug, Deserialize)]
pub struct TransitServiceAreaExecutionRequest {
    pub feed_id: String,
    /// Optional shorthand for `request.modes.transfer_profile_id`.
    #[serde(default)]
    pub transfer_profile_id: Option<String>,
    pub request: TransitServiceAreaRequest,
}

#[derive(Debug, Deserialize)]
pub struct MatrixRequest {
    pub origins: PointSetDocument,
    pub destinations: PointSetDocument,
}

#[derive(Debug, Serialize)]
pub struct RouteExecutionResponse {
    pub(crate) service: ExecutionContext,
    pub(crate) result: RouteResult,
}

#[derive(Debug, Serialize)]
pub struct OdExecutionResponse {
    pub(crate) service: ExecutionContext,
    pub(crate) result: OdResult,
}

#[derive(Debug, Serialize)]
pub struct MatrixExecutionResponse {
    pub(crate) service: ExecutionContext,
    pub(crate) result: MatrixResult,
}

#[derive(Debug, Serialize)]
pub struct AccessibilityExecutionResponse {
    pub(crate) service: ExecutionContext,
    pub(crate) result: AccessibilityResult,
}

#[derive(Debug, Serialize)]
pub struct ServiceAreaExecutionResponse {
    pub(crate) service: ExecutionContext,
    pub(crate) result: ServiceAreaResult,
}

#[derive(Debug, Serialize)]
pub struct ServiceAreaSequenceExecutionResponse {
    pub(crate) service: ExecutionContext,
    pub(crate) result: ServiceAreaSequenceResult,
}

#[derive(Debug, Serialize)]
pub struct BetweennessExecutionResponse {
    pub(crate) service: ExecutionContext,
    pub(crate) result: BetweennessResult,
}

#[derive(Debug, Serialize)]
pub struct ScenarioBatchExecutionResponse {
    pub(crate) service: ExecutionContext,
    pub(crate) result: ScenarioBatchResult,
}

#[derive(Debug, Serialize)]
pub struct TransitRouteExecutionResponse {
    pub(crate) service: TransitExecutionContext,
    pub(crate) result: TransitRouteResult,
}

#[derive(Debug, Serialize)]
pub struct TransitServiceAreaExecutionResponse {
    pub(crate) service: TransitExecutionContext,
    pub(crate) result: TransitServiceAreaResult,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct ResponseFormatQuery {
    pub(crate) format: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExecutionContext {
    pub(crate) dataset_id: String,
    pub(crate) profile_id: String,
    pub(crate) profile_hash: String,
    pub(crate) route_engine: String,
    pub(crate) batch_engine: String,
    pub(crate) acceleration: String,
}

#[derive(Debug, Serialize)]
pub struct TransitExecutionContext {
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
}
