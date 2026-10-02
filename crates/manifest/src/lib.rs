//! Persisted manifest types for `.netweevil/` state: dataset imports,
//! compiled profiles, and analysis runs.
//!
//! These types are shared by the state layer (`netweevil-persist`), the
//! import pipeline (`netweevil-ingest`), and the reporting/export crate
//! (`netweevil-report`). They live in their own thin crate so reading or
//! writing a manifest does not pull the report crate's parquet/arrow/sqlite
//! export machinery into the build.

use anyhow::{Context, Result};
use netweevil_core::{
    AccelerationBundleStats, BuildStage, CacheBundleId, DatasetId, SourceFormat,
    TopologyBundleMeta, TravelMode,
};
use netweevil_profile::ProfileDocument;
use netweevil_query::{ConnectivityPolicy, FallbackPolicy};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetManifest {
    pub dataset_id: DatasetId,
    pub label: String,
    pub source_path: String,
    #[serde(default)]
    pub source_paths: Vec<String>,
    pub source_sha256: String,
    pub source_size_bytes: u64,
    #[serde(default)]
    pub source_format: SourceFormat,
    pub imported_at: String,
    pub build_stage: BuildStage,
    #[serde(default)]
    pub topology_bundle: Option<BundleRef>,
    #[serde(default)]
    pub edge_name_bundle: Option<BundleRef>,
    #[serde(default)]
    pub acceleration_bundle: Option<BundleRef>,
    #[serde(default)]
    pub topology_meta: Option<TopologyBundleMeta>,
    #[serde(default)]
    pub acceleration_stats: Option<AccelerationBundleStats>,
    /// Retained OSM source objects that `dataset update` applies change
    /// files to. Only OSM PBF imports have one.
    #[serde(default)]
    pub source_snapshot_bundle: Option<BundleRef>,
    /// The last full nested-dissection ordering of the CCH. Updates reuse
    /// that order and compare their fill against it.
    #[serde(default)]
    pub acceleration_ordering: Option<AccelerationOrdering>,
    /// The most recent `dataset update`, absent until the first one.
    #[serde(default)]
    pub last_update: Option<DatasetUpdateRecord>,
}

/// Shortcut density of the last full CCH ordering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccelerationOrdering {
    pub ordered_at: String,
    pub edge_count: u64,
    pub shortcut_arc_count: u64,
    /// Updates applied since that ordering, each with a patched order.
    pub patched_updates: u64,
}

impl AccelerationOrdering {
    /// Shortcuts per edge state of the full ordering.
    pub fn shortcut_density(&self) -> f64 {
        self.shortcut_arc_count as f64 / self.edge_count.max(1) as f64
    }

    /// `shortcut_arc_count` over the shortcut count a fresh ordering of
    /// `edge_count` states is expected to produce at this ordering's
    /// density: 1.0 means no fill growth.
    pub fn fill_growth(&self, edge_count: u64, shortcut_arc_count: u64) -> f64 {
        let expected = self.shortcut_density() * edge_count as f64;
        if expected <= 0.0 {
            return if shortcut_arc_count == 0 {
                1.0
            } else {
                f64::INFINITY
            };
        }
        shortcut_arc_count as f64 / expected
    }
}

/// What one `dataset update` applied and how its hierarchy compares with
/// a fresh ordering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetUpdateRecord {
    pub updated_at: String,
    /// `osm_change` for change files or `source` for a replacement extract.
    pub kind: String,
    pub inputs: Vec<String>,
    pub previous_source_sha256: String,
    /// Directed edge states whose CCH rank was carried over.
    pub reused_edge_states: u64,
    /// Directed edge states placed next to their neighbours' ranks.
    pub inserted_edge_states: u64,
    /// Whether this update ran a full nested-dissection ordering.
    pub reordered: bool,
    /// Shortcut count relative to the last full ordering's density.
    pub fill_growth: f64,
    pub elapsed_s: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledProfileManifest {
    pub compile_id: String,
    pub dataset_id: DatasetId,
    pub profile_id: String,
    pub profile_hash: String,
    pub defaults_pack: String,
    pub mode: TravelMode,
    pub created_at: String,
    #[serde(default)]
    pub topology_bundle_id: Option<CacheBundleId>,
    #[serde(default)]
    pub edge_count: Option<u64>,
    pub bundle: BundleRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleRef {
    pub bundle_id: CacheBundleId,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunManifest {
    pub run_id: String,
    pub run_kind: RunKind,
    pub status: RunStatus,
    pub created_at: String,
    pub dataset_id: String,
    pub profile_id: String,
    #[serde(default)]
    pub compiled_profile_bundle_id: Option<String>,
    pub request_source: String,
    /// Fully resolved request executed by the CLI after command-line
    /// overrides. Omitted from the manifest when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_request: Option<serde_json::Value>,
    #[serde(default)]
    pub result_path: Option<String>,
    #[serde(default)]
    pub report_path: Option<String>,
    pub software: SoftwareInfo,
    pub algorithm: AlgorithmInfo,
    pub methods_summary: MethodsSummary,
    #[serde(default)]
    pub connectivity_policy: Option<ConnectivityPolicy>,
    #[serde(default)]
    pub fallback_policy: Option<FallbackPolicy>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Route,
    RouteBatch,
    Od,
    Matrix,
    Accessibility,
    ServiceArea,
    ServiceAreaSequence,
    Betweenness,
    ScenarioBatch,
    Experiment,
    Simulation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Planned,
    Succeeded,
    Failed,
    NotImplemented,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoftwareInfo {
    pub executable: String,
    pub version: String,
    pub git_commit: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlgorithmInfo {
    pub engine: String,
    pub graph_model: String,
    pub acceleration: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodsSummary {
    pub plain_language: String,
    #[serde(default)]
    pub locked_profile_hash: Option<String>,
    #[serde(default)]
    pub defaults_pack: Option<String>,
}

pub fn now_rfc3339() -> Result<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("formatting timestamp")
}

/// Builds a run manifest. `algorithm` and `methods_summary` describe the engine
/// that actually produced the run, so callers must supply the finished values
/// rather than patching them onto a freshly constructed manifest.
pub fn new_run_manifest(
    run_kind: RunKind,
    dataset_id: impl Into<String>,
    profile: &ProfileDocument,
    request_source: impl Into<String>,
    status: RunStatus,
    message: impl Into<String>,
    software: SoftwareInfo,
    compiled_profile_bundle_id: Option<String>,
    algorithm: AlgorithmInfo,
    methods_summary: impl Into<String>,
) -> Result<RunManifest> {
    Ok(RunManifest {
        run_id: Uuid::new_v4().to_string(),
        run_kind,
        status,
        created_at: now_rfc3339()?,
        dataset_id: dataset_id.into(),
        profile_id: profile.profile.id.clone(),
        compiled_profile_bundle_id,
        request_source: request_source.into(),
        effective_request: None,
        result_path: None,
        report_path: None,
        software,
        algorithm,
        methods_summary: MethodsSummary {
            plain_language: methods_summary.into(),
            locked_profile_hash: Some(profile.fingerprint()?),
            defaults_pack: Some(profile.profile.defaults_pack.clone()),
        },
        connectivity_policy: None,
        fallback_policy: None,
        message: message.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_request_is_omitted_when_absent_and_round_trips_when_present() {
        let mut manifest: RunManifest = serde_json::from_value(serde_json::json!({
            "run_id": "run",
            "run_kind": "matrix",
            "status": "succeeded",
            "created_at": "2026-07-10T00:00:00Z",
            "dataset_id": "dataset",
            "profile_id": "profile",
            "request_source": "origins.json | destinations.json",
            "software": {"executable": "netweevil", "version": "0.1.0", "git_commit": null},
            "algorithm": {"engine": "exact", "graph_model": "directed_edge_graph", "acceleration": "none"},
            "methods_summary": {"plain_language": "test"},
            "message": "ok"
        }))
        .expect("manifest without effective_request parses");
        assert!(manifest.effective_request.is_none());
        let json = serde_json::to_string(&manifest).expect("manifest serializes");
        assert!(!json.contains("effective_request"));

        manifest.effective_request = Some(serde_json::json!({
            "origins": {"departure_time": "2026-07-10T09:00:00+08:00"},
            "destinations": {"points": []}
        }));
        let json = serde_json::to_string(&manifest).expect("manifest serializes");
        let decoded: RunManifest = serde_json::from_str(&json).expect("manifest re-parses");
        assert_eq!(decoded.effective_request, manifest.effective_request);
    }
}
