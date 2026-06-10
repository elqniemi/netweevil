use std::error::Error as StdError;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisOutcome {
    #[default]
    Legal,
    Degraded,
    Partial,
    Unreachable,
    NotImplemented,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisDiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisDiagnosticCode {
    SnapNoCandidate,
    DisconnectedComponents,
    LegalRouteUnreachable,
    FallbackUsed,
    AnalysisNotImplemented,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisDiagnostic {
    pub code: AnalysisDiagnosticCode,
    pub severity: AnalysisDiagnosticSeverity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub point_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub component_ids: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_actions: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AnalysisFailure {
    pub message: String,
    pub outcome: AnalysisOutcome,
    pub diagnostics: Vec<AnalysisDiagnostic>,
}

impl AnalysisFailure {
    pub fn new(
        message: impl Into<String>,
        outcome: AnalysisOutcome,
        diagnostics: Vec<AnalysisDiagnostic>,
    ) -> Self {
        Self {
            message: message.into(),
            outcome,
            diagnostics,
        }
    }
}

impl fmt::Display for AnalysisFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for AnalysisFailure {}

pub fn analysis_failure(error: &anyhow::Error) -> Option<&AnalysisFailure> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<AnalysisFailure>())
}

pub(crate) fn route_snap_failure(point: &LabeledPoint, max_distance_m: f64) -> AnalysisFailure {
    AnalysisFailure::new(
        format!(
            "point '{}' has no traversable candidate node or edge within {:.1} m",
            point.id, max_distance_m
        ),
        AnalysisOutcome::Unreachable,
        vec![AnalysisDiagnostic {
            code: AnalysisDiagnosticCode::SnapNoCandidate,
            severity: AnalysisDiagnosticSeverity::Error,
            message: format!(
                "No traversable node or edge was found within {:.1} m for point '{}'.",
                max_distance_m, point.id
            ),
            point_ids: vec![point.id.clone()],
            component_ids: Vec::new(),
            suggested_actions: vec![
                "Increase snap.max_distance_m.".to_string(),
                "Move the point closer to the routable network.".to_string(),
            ],
        }],
    )
}
