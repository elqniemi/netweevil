//! Request and response payloads for the HTTP endpoints. Analysis endpoints
//! share one request envelope (`{profile_id?, engine_mode?, request}`) and one
//! response envelope (`{service, result}`); the aliases below name the
//! concrete payload of each endpoint.

use netweevil_core::{ConnectedComponentsMeta, TopologyBounds};
use netweevil_query::{
    AccessibilityRequest, BetweennessRequest, DirectionsOptions, EngineMode, OdPairsDocument,
    PointSetDocument, RouteRequest, ScenarioBatchRequest, ServiceAreaRequest,
    ServiceAreaSequenceRequest,
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
    /// When `dataset update` last changed the loaded build.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) updated_at: Option<String>,
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
    pub(crate) agency_timezone: String,
    pub(crate) time_origin_unix_s: i64,
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

/// Request-defined profiles are currently supported only by street routes.
#[derive(Debug, Deserialize)]
pub(crate) struct RouteExecutionRequest {
    #[serde(default)]
    pub(crate) profile_id: Option<String>,
    #[serde(default)]
    pub(crate) profile: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) profile_overrides: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) engine_mode: EngineMode,
    pub(crate) request: RouteRequest,
}

/// `/v1/directions`: a route request plus language, units and driving side.
#[derive(Debug, Deserialize)]
pub(crate) struct DirectionsExecutionRequest {
    #[serde(flatten)]
    pub(crate) route: RouteExecutionRequest,
    #[serde(default)]
    pub(crate) directions_options: DirectionsOptions,
}

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

/// `/v1/transit-directions`: a transit route request plus the language of
/// the journey steps and street instructions.
#[derive(Debug, Deserialize)]
pub(crate) struct TransitDirectionsExecutionRequest {
    #[serde(flatten)]
    pub(crate) route: TransitRouteExecutionRequest,
    #[serde(default)]
    pub(crate) directions_options: DirectionsOptions,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TransitServiceAreaExecutionRequest {
    pub(crate) feed_id: String,
    pub(crate) pedestrian_profile_id: Option<String>,
    pub(crate) access_profile_id: Option<String>,
    pub(crate) egress_profile_id: Option<String>,
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
    pub(crate) agency_timezone: String,
    pub(crate) time_origin_unix_s: i64,
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
mod tests;
