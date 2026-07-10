use std::collections::BTreeMap;

use netweevil_core::{RoadClass, SurfaceClass};
use serde::{Deserialize, Serialize};

use crate::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAreaGeometryType {
    Network,
    Polygon,
    Segment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAreaFeature {
    #[serde(default)]
    pub origin_id: Option<String>,
    #[serde(default)]
    pub band_start_limit: Option<f64>,
    #[serde(default)]
    pub threshold_id: Option<String>,
    pub threshold_limit: f64,
    pub threshold_metric: ServiceAreaThresholdMetric,
    pub geometry_type: ServiceAreaGeometryType,
    #[serde(default)]
    pub fallback_used: bool,
    #[serde(default)]
    pub origin_component_id: Option<u32>,
    #[serde(default)]
    pub origin_hop_distance_m: Option<f64>,
    #[serde(default)]
    pub reachable_network_length_m: Option<f64>,
    #[serde(default)]
    pub reachable_edge_count: Option<u64>,
    #[serde(default)]
    pub edge_id: Option<u32>,
    #[serde(default)]
    pub edge_index: Option<u32>,
    #[serde(default)]
    pub source_way_id: Option<i64>,
    #[serde(default)]
    pub from_node_id: Option<u32>,
    #[serde(default)]
    pub to_node_id: Option<u32>,
    #[serde(default)]
    pub start_fraction: Option<f64>,
    #[serde(default)]
    pub end_fraction: Option<f64>,
    #[serde(default)]
    pub start_cost: Option<f64>,
    #[serde(default)]
    pub end_cost: Option<f64>,
    #[serde(default)]
    pub segment_distance_m: Option<f64>,
    #[serde(default)]
    pub segment_travel_time_s: Option<f64>,
    /// Named additive profile-component totals at the start of a segment.
    ///
    /// These are populated only for segment features. A merged network or
    /// polygon represents many shortest paths and therefore has no single
    /// well-defined cumulative component vector.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub start_components: BTreeMap<String, f64>,
    /// Named additive profile-component totals at the end of a segment.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub end_components: BTreeMap<String, f64>,
    #[serde(default)]
    pub geometry: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAreaSegment {
    #[serde(default)]
    pub origin_id: Option<String>,
    #[serde(default)]
    pub band_start_limit: Option<f64>,
    #[serde(default)]
    pub threshold_id: Option<String>,
    pub threshold_limit: f64,
    pub threshold_metric: ServiceAreaThresholdMetric,
    pub edge_id: u32,
    pub edge_index: u32,
    pub source_way_id: i64,
    pub from_node_id: u32,
    pub to_node_id: u32,
    pub start_fraction: f64,
    pub end_fraction: f64,
    pub start_cost: f64,
    pub end_cost: f64,
    pub segment_distance_m: f64,
    #[serde(default)]
    pub segment_travel_time_s: Option<f64>,
    #[serde(default)]
    pub fallback_used: bool,
    #[serde(default)]
    pub origin_component_id: Option<u32>,
    #[serde(default)]
    pub origin_hop_distance_m: Option<f64>,
    /// Cumulative named profile-component totals on the winning shortest
    /// path at `start_fraction`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub start_components: BTreeMap<String, f64>,
    /// Cumulative named profile-component totals on the same path at
    /// `end_fraction`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub end_components: BTreeMap<String, f64>,
    #[serde(default)]
    pub geometry: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAreaThresholdSummary {
    #[serde(default)]
    pub origin_id: Option<String>,
    #[serde(default)]
    pub band_start_limit: Option<f64>,
    #[serde(default)]
    pub threshold_id: Option<String>,
    pub threshold_limit: f64,
    pub threshold_metric: ServiceAreaThresholdMetric,
    #[serde(default)]
    pub fallback_used: bool,
    #[serde(default)]
    pub origin_component_id: Option<u32>,
    #[serde(default)]
    pub origin_hop_distance_m: Option<f64>,
    #[serde(default)]
    pub reachable_network_length_m: Option<f64>,
    #[serde(default)]
    pub reachable_edge_count: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAreaResult {
    pub analysis_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub departure_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    #[serde(default)]
    pub outcome: AnalysisOutcome,
    #[serde(default)]
    pub output_mode: ServiceAreaOutputMode,
    #[serde(default)]
    pub band_mode: ServiceAreaBandMode,
    #[serde(default)]
    pub boundary_mode: ServiceAreaBoundaryMode,
    #[serde(default)]
    pub multi_origin_mode: ServiceAreaMultiOriginMode,
    pub origin_count: usize,
    #[serde(default)]
    pub processed_origin_count: usize,
    #[serde(default)]
    pub skipped_origin_count: usize,
    #[serde(default)]
    pub fallback_origin_count: usize,
    pub threshold_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<ServiceAreaFeature>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summaries: Vec<ServiceAreaThresholdSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub segments: Vec<ServiceAreaSegment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AnalysisDiagnostic>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnappedPoint {
    pub point_id: String,
    pub requested_lon: f64,
    pub requested_lat: f64,
    pub snapped_node_id: u32,
    pub snapped_lon: f64,
    pub snapped_lat: f64,
    /// Elevation of the snapped network position in metres. Sources without
    /// elevation data use zero.
    #[serde(default)]
    pub snapped_z: f64,
    pub snap_distance_m: f64,
    #[serde(default)]
    pub snapped_edge_id: Option<u32>,
    #[serde(default)]
    pub snapped_edge_fraction: Option<f64>,
    #[serde(default)]
    pub snapped_from_node_id: Option<u32>,
    #[serde(default)]
    pub snapped_to_node_id: Option<u32>,
    #[serde(default)]
    pub component_id: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteSummary {
    #[serde(default)]
    pub network_distance_m: u64,
    #[serde(default)]
    pub network_travel_time_s: f64,
    #[serde(default)]
    pub network_generalized_cost: f64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub components: BTreeMap<String, f64>,
    #[serde(default)]
    pub waiting_time_s: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub departure_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrival_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    #[serde(default)]
    pub illegal_movement_penalty_s: f64,
    #[serde(default)]
    pub illegal_movement_penalty_cost: f64,
    #[serde(default)]
    pub violation_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violation_types: Vec<RouteViolationType>,
    pub total_distance_m: u64,
    pub total_travel_time_s: f64,
    pub total_generalized_cost: f64,
    pub segment_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteViolationType {
    ReverseOneway,
    IllegalTurn,
    IgnoredTurnRestriction,
    ForbiddenUturn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteViolation {
    pub violation_type: RouteViolationType,
    #[serde(default)]
    pub edge_id: Option<u32>,
    #[serde(default)]
    pub from_edge_id: Option<u32>,
    #[serde(default)]
    pub to_edge_id: Option<u32>,
    #[serde(default)]
    pub distance_m: Option<f64>,
    #[serde(default)]
    pub penalty_s: f64,
    #[serde(default)]
    pub penalty_generalized_cost: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HopEndpoint {
    Origin,
    Destination,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteHopSegment {
    pub endpoint: HopEndpoint,
    pub distance_m: f64,
    pub geometry: Vec<[f64; 3]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteSegment {
    pub edge_id: u32,
    pub from_node_id: u32,
    pub to_node_id: u32,
    pub source_way_id: i64,
    pub length_m: u32,
    pub travel_time_s: f64,
    pub generalized_cost: f64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub components: BTreeMap<String, f64>,
    #[serde(default)]
    pub waiting_time_s: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_time: Option<String>,
    pub road_class: RoadClass,
    pub surface: SurfaceClass,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub violation_type: Option<RouteViolationType>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MetricBreakdown {
    #[serde(default)]
    pub distance_m: Option<u64>,
    #[serde(default)]
    pub time_s: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RouteBreakdowns {
    #[serde(default)]
    pub road_class: BTreeMap<String, MetricBreakdown>,
    #[serde(default)]
    pub surface: BTreeMap<String, MetricBreakdown>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteResult {
    pub route_id: String,
    pub origin: SnappedPoint,
    pub destination: SnappedPoint,
    #[serde(default)]
    pub outcome: AnalysisOutcome,
    #[serde(default)]
    pub fallback_used: bool,
    #[serde(default)]
    pub origin_hop_distance_m: Option<f64>,
    #[serde(default)]
    pub destination_hop_distance_m: Option<f64>,
    pub summary: RouteSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub node_path: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edge_path: Vec<u32>,
    #[serde(default)]
    pub geometry: Option<Vec<[f64; 3]>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hop_segments: Vec<RouteHopSegment>,
    #[serde(default)]
    pub segments: Option<Vec<RouteSegment>>,
    #[serde(default)]
    pub breakdowns: Option<RouteBreakdowns>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<RouteViolation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AnalysisDiagnostic>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<RouteAlternative>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteAlternative {
    pub alternative_index: u32,
    pub rank: u32,
    pub summary: RouteSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub node_path: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edge_path: Vec<u32>,
    #[serde(default)]
    pub geometry: Option<Vec<[f64; 3]>>,
    #[serde(default)]
    pub segments: Option<Vec<RouteSegment>>,
    #[serde(default)]
    pub breakdowns: Option<RouteBreakdowns>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<RouteViolation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AnalysisDiagnostic>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteBatchItemResult {
    pub route_id: String,
    pub origin_id: String,
    pub destination_id: String,
    pub status: BatchItemStatus,
    #[serde(default)]
    pub route: Option<RouteResult>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteBatchResult {
    pub route_count: usize,
    pub succeeded_count: usize,
    pub failed_count: usize,
    pub items: Vec<RouteBatchItemResult>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchItemStatus {
    Succeeded,
    Ignored,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OdPairResult {
    pub pair_id: String,
    pub origin_id: String,
    pub destination_id: String,
    pub status: BatchItemStatus,
    #[serde(default)]
    pub outcome: AnalysisOutcome,
    #[serde(default)]
    pub fallback_used: bool,
    #[serde(default)]
    pub origin_component_id: Option<u32>,
    #[serde(default)]
    pub destination_component_id: Option<u32>,
    #[serde(default)]
    pub origin_hop_distance_m: Option<f64>,
    #[serde(default)]
    pub destination_hop_distance_m: Option<f64>,
    #[serde(default)]
    pub origin_snap_distance_m: Option<f64>,
    #[serde(default)]
    pub destination_snap_distance_m: Option<f64>,
    #[serde(default)]
    pub total_distance_m: Option<u64>,
    #[serde(default)]
    pub total_travel_time_s: Option<f64>,
    #[serde(default)]
    pub total_generalized_cost: Option<f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub components: BTreeMap<String, f64>,
    #[serde(default)]
    pub illegal_movement_penalty_s: Option<f64>,
    #[serde(default)]
    pub illegal_movement_penalty_cost: Option<f64>,
    #[serde(default)]
    pub violation_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violation_types: Vec<RouteViolationType>,
    #[serde(default)]
    pub geometry: Option<Vec<[f64; 3]>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AnalysisDiagnostic>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<BatchAlternativeResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OdResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub departure_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrive_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    pub pair_count: usize,
    pub succeeded_count: usize,
    pub failed_count: usize,
    #[serde(default)]
    pub ignored_count: usize,
    pub pairs: Vec<OdPairResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AnalysisDiagnostic>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixCellResult {
    pub origin_id: String,
    pub destination_id: String,
    pub status: BatchItemStatus,
    #[serde(default)]
    pub outcome: AnalysisOutcome,
    #[serde(default)]
    pub fallback_used: bool,
    #[serde(default)]
    pub origin_component_id: Option<u32>,
    #[serde(default)]
    pub destination_component_id: Option<u32>,
    #[serde(default)]
    pub origin_hop_distance_m: Option<f64>,
    #[serde(default)]
    pub destination_hop_distance_m: Option<f64>,
    #[serde(default)]
    pub origin_snap_distance_m: Option<f64>,
    #[serde(default)]
    pub destination_snap_distance_m: Option<f64>,
    #[serde(default)]
    pub total_distance_m: Option<u64>,
    #[serde(default)]
    pub total_travel_time_s: Option<f64>,
    #[serde(default)]
    pub total_generalized_cost: Option<f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub components: BTreeMap<String, f64>,
    #[serde(default)]
    pub illegal_movement_penalty_s: Option<f64>,
    #[serde(default)]
    pub illegal_movement_penalty_cost: Option<f64>,
    #[serde(default)]
    pub violation_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violation_types: Vec<RouteViolationType>,
    #[serde(default)]
    pub geometry: Option<Vec<[f64; 3]>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AnalysisDiagnostic>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<BatchAlternativeResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchAlternativeResult {
    pub alternative_index: u32,
    pub rank: u32,
    #[serde(default)]
    pub total_distance_m: Option<u64>,
    #[serde(default)]
    pub total_travel_time_s: Option<f64>,
    #[serde(default)]
    pub total_generalized_cost: Option<f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub components: BTreeMap<String, f64>,
    #[serde(default)]
    pub geometry: Option<Vec<[f64; 3]>>,
    #[serde(default)]
    pub violation_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violation_types: Vec<RouteViolationType>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub departure_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrive_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    pub origin_count: usize,
    pub destination_count: usize,
    pub cell_count: usize,
    pub succeeded_count: usize,
    pub failed_count: usize,
    #[serde(default)]
    pub ignored_count: usize,
    pub cells: Vec<MatrixCellResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AnalysisDiagnostic>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessibilityCategoryResult {
    pub origin_id: String,
    pub category_id: String,
    pub status: BatchItemStatus,
    #[serde(default)]
    pub outcome: AnalysisOutcome,
    #[serde(default)]
    pub fallback_used: bool,
    #[serde(default)]
    pub origin_component_id: Option<u32>,
    #[serde(default)]
    pub origin_hop_distance_m: Option<f64>,
    #[serde(default)]
    pub origin_snap_distance_m: Option<f64>,
    pub destination_count: usize,
    pub snapped_destination_count: usize,
    #[serde(default)]
    pub nearest_destination_id: Option<String>,
    #[serde(default)]
    pub nearest_travel_time_s: Option<f64>,
    #[serde(default)]
    pub nearest_destination_snap_distance_m: Option<f64>,
    #[serde(default)]
    pub counts_within_threshold_s: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AnalysisDiagnostic>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessibilityResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub departure_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrive_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    pub origin_count: usize,
    pub category_count: usize,
    pub destination_count: usize,
    pub max_travel_time_s: f64,
    pub thresholds_s: Vec<f64>,
    pub row_count: usize,
    pub succeeded_count: usize,
    pub failed_count: usize,
    #[serde(default)]
    pub skipped_origin_count: usize,
    pub rows: Vec<AccessibilityCategoryResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AnalysisDiagnostic>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod provenance_compatibility_tests {
    use super::*;

    #[test]
    fn legacy_batch_results_default_temporal_provenance() {
        let od: OdResult = serde_json::from_value(serde_json::json!({
            "pair_count": 0,
            "succeeded_count": 0,
            "failed_count": 0,
            "pairs": []
        }))
        .expect("legacy OD result parses");
        let matrix: MatrixResult = serde_json::from_value(serde_json::json!({
            "origin_count": 0,
            "destination_count": 0,
            "cell_count": 0,
            "succeeded_count": 0,
            "failed_count": 0,
            "cells": []
        }))
        .expect("legacy matrix result parses");
        let accessibility: AccessibilityResult = serde_json::from_value(serde_json::json!({
            "origin_count": 0,
            "category_count": 0,
            "destination_count": 0,
            "max_travel_time_s": 600.0,
            "thresholds_s": [300.0],
            "row_count": 0,
            "succeeded_count": 0,
            "failed_count": 0,
            "rows": []
        }))
        .expect("legacy accessibility result parses");

        for value in [
            od.departure_time,
            od.arrive_by,
            od.scenario_id,
            matrix.departure_time,
            matrix.arrive_by,
            matrix.scenario_id,
            accessibility.departure_time,
            accessibility.arrive_by,
            accessibility.scenario_id,
        ] {
            assert!(value.is_none());
        }
    }
}
