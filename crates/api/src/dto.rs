use netweevil_core::{ConnectedComponentsMeta, TopologyBounds};
use netweevil_query::{
    EngineMode, MatrixResult, OdPairsDocument, OdResult, PointSetDocument, RouteRequest,
    RouteResult, ServiceAreaRequest, ServiceAreaResult,
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
pub struct ServiceAreaExecutionRequest {
    #[serde(default)]
    pub profile_id: Option<String>,
    pub request: ServiceAreaRequest,
}

#[derive(Debug, Deserialize)]
pub struct TransitRouteExecutionRequest {
    pub feed_id: String,
    #[serde(default)]
    pub pedestrian_profile_id: Option<String>,
    pub request: TransitRouteRequest,
}

#[derive(Debug, Deserialize)]
pub struct TransitServiceAreaExecutionRequest {
    pub feed_id: String,
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
pub struct ServiceAreaExecutionResponse {
    pub(crate) service: ExecutionContext,
    pub(crate) result: ServiceAreaResult,
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
    pub(crate) pedestrian_profile_id: Option<String>,
}
