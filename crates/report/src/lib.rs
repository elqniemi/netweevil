mod output;

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use netweevil_core::{
    AccelerationBuildSettings, AccelerationBundleStats, BuildStage, CacheBundleId, DatasetId,
    TopologyBundleMeta, TravelMode,
};
use netweevil_profile::ProfileDocument;
use netweevil_query::{
    AnalysisOutcome, ConnectivityPolicy, FallbackPolicy, MatrixResult, OdResult, RouteBatchResult,
    RouteResult, ServiceAreaBandMode, ServiceAreaMultiOriginMode, ServiceAreaOutputMode,
    ServiceAreaResult,
};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

pub use output::{
    write_matrix_result, write_od_result, write_route_batch_result, write_route_result,
    write_service_area_result,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetManifest {
    pub dataset_id: DatasetId,
    pub label: String,
    pub source_path: String,
    pub source_sha256: String,
    pub source_size_bytes: u64,
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
    pub acceleration_settings: Option<AccelerationBuildSettings>,
    #[serde(default)]
    pub acceleration_stats: Option<AccelerationBundleStats>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    ServiceArea,
    Experiment,
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

pub fn new_run_manifest(
    run_kind: RunKind,
    dataset_id: impl Into<String>,
    profile: &ProfileDocument,
    request_source: impl Into<String>,
    status: RunStatus,
    message: impl Into<String>,
    software: SoftwareInfo,
    compiled_profile_bundle_id: Option<String>,
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
        result_path: None,
        report_path: None,
        software,
        algorithm: AlgorithmInfo {
            engine: "edge_based_exact_placeholder".to_string(),
            graph_model: "directed_edge_graph".to_string(),
            acceleration: "none".to_string(),
        },
        methods_summary: MethodsSummary {
            plain_language: "Scaffold manifest only. The routing kernel, snapping, turn restrictions, and path computation are not implemented yet.".to_string(),
            locked_profile_hash: Some(profile.fingerprint()?),
            defaults_pack: Some(profile.profile.defaults_pack.clone()),
        },
        connectivity_policy: None,
        fallback_policy: None,
        message: message.into(),
    })
}

#[derive(Debug, Clone)]
pub enum RunResultSummary {
    Route(RouteSummary),
    Batch(BatchSummary),
    ServiceArea(ServiceAreaSummary),
}

#[derive(Debug, Clone)]
pub struct RouteSummary {
    pub route_id: String,
    pub outcome: AnalysisOutcome,
    pub total_distance_m: u64,
    pub total_travel_time_s: f64,
    pub total_generalized_cost: f64,
    pub illegal_movement_penalty_s: f64,
    pub illegal_movement_penalty_cost: f64,
    pub violation_count: usize,
    pub segment_count: usize,
    pub diagnostics_count: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BatchSummary {
    pub label: &'static str,
    pub item_count: usize,
    pub succeeded_count: usize,
    pub failed_count: usize,
    pub ignored_count: usize,
    pub legal_count: usize,
    pub degraded_count: usize,
    pub partial_count: usize,
    pub unreachable_count: usize,
    pub diagnostics_count: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ServiceAreaSummary {
    pub analysis_id: String,
    pub outcome: AnalysisOutcome,
    pub output_mode: ServiceAreaOutputMode,
    pub band_mode: ServiceAreaBandMode,
    pub multi_origin_mode: ServiceAreaMultiOriginMode,
    pub origin_count: usize,
    pub processed_origin_count: usize,
    pub skipped_origin_count: usize,
    pub fallback_origin_count: usize,
    pub threshold_count: usize,
    pub feature_count: usize,
    pub diagnostics_count: usize,
    pub warnings: Vec<String>,
}

pub fn load_run_result_summary(manifest: &RunManifest) -> Result<Option<RunResultSummary>> {
    let Some(result_path) = manifest.result_path.as_deref() else {
        return Ok(None);
    };
    let extension = Path::new(result_path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());
    if extension.as_deref() != Some("json") {
        return Ok(None);
    }
    let raw = fs::read_to_string(result_path)
        .with_context(|| format!("reading run result {}", result_path))?;
    let summary = match manifest.run_kind {
        RunKind::Route => {
            let route: RouteResult =
                serde_json::from_str(&raw).context("parsing route result JSON")?;
            RunResultSummary::Route(RouteSummary {
                route_id: route.route_id,
                outcome: route.outcome,
                total_distance_m: route.summary.total_distance_m,
                total_travel_time_s: route.summary.total_travel_time_s,
                total_generalized_cost: route.summary.total_generalized_cost,
                illegal_movement_penalty_s: route.summary.illegal_movement_penalty_s,
                illegal_movement_penalty_cost: route.summary.illegal_movement_penalty_cost,
                violation_count: route.summary.violation_count,
                segment_count: route.summary.segment_count,
                diagnostics_count: route.diagnostics.len(),
                warnings: route.warnings,
            })
        }
        RunKind::RouteBatch => {
            let batch: RouteBatchResult =
                serde_json::from_str(&raw).context("parsing route-batch result JSON")?;
            let tally = tally_outcomes(batch.items.iter().filter_map(|item| {
                item.route
                    .as_ref()
                    .map(|route| (route.outcome, route.diagnostics.len()))
            }));
            RunResultSummary::Batch(BatchSummary {
                label: "routes",
                item_count: batch.route_count,
                succeeded_count: batch.succeeded_count,
                failed_count: batch.failed_count,
                ignored_count: 0,
                legal_count: tally.legal_count,
                degraded_count: tally.degraded_count,
                partial_count: tally.partial_count,
                unreachable_count: tally.unreachable_count,
                diagnostics_count: tally.diagnostics_count,
                warnings: batch.warnings,
            })
        }
        RunKind::Od => {
            let od: OdResult = serde_json::from_str(&raw).context("parsing OD result JSON")?;
            let tally = tally_outcomes(
                od.pairs
                    .iter()
                    .map(|pair| (pair.outcome, pair.diagnostics.len())),
            );
            RunResultSummary::Batch(BatchSummary {
                label: "OD pairs",
                item_count: od.pair_count,
                succeeded_count: od.succeeded_count,
                failed_count: od.failed_count,
                ignored_count: od.ignored_count,
                legal_count: tally.legal_count,
                degraded_count: tally.degraded_count,
                partial_count: tally.partial_count,
                unreachable_count: tally.unreachable_count,
                diagnostics_count: tally.diagnostics_count,
                warnings: od.warnings,
            })
        }
        RunKind::Matrix => {
            let matrix: MatrixResult =
                serde_json::from_str(&raw).context("parsing matrix result JSON")?;
            let tally = tally_outcomes(
                matrix
                    .cells
                    .iter()
                    .map(|cell| (cell.outcome, cell.diagnostics.len())),
            );
            RunResultSummary::Batch(BatchSummary {
                label: "matrix cells",
                item_count: matrix.cell_count,
                succeeded_count: matrix.succeeded_count,
                failed_count: matrix.failed_count,
                ignored_count: matrix.ignored_count,
                legal_count: tally.legal_count,
                degraded_count: tally.degraded_count,
                partial_count: tally.partial_count,
                unreachable_count: tally.unreachable_count,
                diagnostics_count: tally.diagnostics_count,
                warnings: matrix.warnings,
            })
        }
        RunKind::ServiceArea => {
            let service_area: ServiceAreaResult =
                serde_json::from_str(&raw).context("parsing service-area result JSON")?;
            RunResultSummary::ServiceArea(ServiceAreaSummary {
                analysis_id: service_area.analysis_id,
                outcome: service_area.outcome,
                output_mode: service_area.output_mode,
                band_mode: service_area.band_mode,
                multi_origin_mode: service_area.multi_origin_mode,
                origin_count: service_area.origin_count,
                processed_origin_count: service_area.processed_origin_count,
                skipped_origin_count: service_area.skipped_origin_count,
                fallback_origin_count: service_area.fallback_origin_count,
                threshold_count: service_area.threshold_count,
                feature_count: service_area.features.len(),
                diagnostics_count: service_area.diagnostics.len(),
                warnings: service_area.warnings,
            })
        }
        RunKind::Experiment => return Ok(None),
    };
    Ok(Some(summary))
}

pub fn render_run_markdown(
    manifest: &RunManifest,
    result_summary: Option<&RunResultSummary>,
) -> String {
    let mut markdown = String::new();
    markdown.push_str("# netweevil run report\n\n");
    markdown.push_str(&format!(
        "- Run ID: `{}`\n\
         - Status: `{:?}`\n\
         - Kind: `{:?}`\n\
         - Created: `{}`\n\
         - Dataset: `{}`\n\
         - Profile: `{}`\n\
         - Compiled bundle: `{}`\n\
         - Request source: `{}`\n\
         - Result path: `{}`\n\
         - Software: `{}` `{}`\n\
         - Git commit: `{}`\n\
         - Algorithm engine: `{}`\n\
         - Graph model: `{}`\n\
         - Acceleration: `{}`\n\
         - Connectivity policy: `{}`\n\
         - Fallback policy: `{}`\n\n",
        manifest.run_id,
        manifest.status,
        manifest.run_kind,
        manifest.created_at,
        manifest.dataset_id,
        manifest.profile_id,
        manifest
            .compiled_profile_bundle_id
            .as_deref()
            .unwrap_or("not_recorded"),
        manifest.request_source,
        manifest.result_path.as_deref().unwrap_or("not_written"),
        manifest.software.executable,
        manifest.software.version,
        manifest.software.git_commit.as_deref().unwrap_or("unknown"),
        manifest.algorithm.engine,
        manifest.algorithm.graph_model,
        manifest.algorithm.acceleration,
        compact_json_or_not_recorded(manifest.connectivity_policy.as_ref()),
        compact_json_or_not_recorded(manifest.fallback_policy.as_ref()),
    ));

    if let Some(summary) = result_summary {
        markdown.push_str("## Results\n\n");
        push_summary_markdown(&mut markdown, summary);
        markdown.push('\n');
    }

    markdown.push_str("## Methods\n\n");
    markdown.push_str(&manifest.methods_summary.plain_language);
    markdown.push_str("\n\n");
    markdown.push_str(&format!(
        "- Locked profile hash: `{}`\n\
         - Defaults pack: `{}`\n\n",
        manifest
            .methods_summary
            .locked_profile_hash
            .as_deref()
            .unwrap_or("not_recorded"),
        manifest
            .methods_summary
            .defaults_pack
            .as_deref()
            .unwrap_or("not_recorded"),
    ));

    markdown.push_str("## Message\n\n");
    markdown.push_str(&manifest.message);
    markdown.push('\n');
    markdown
}

pub fn render_run_html(
    manifest: &RunManifest,
    result_summary: Option<&RunResultSummary>,
) -> String {
    let mut html = String::new();
    html.push_str(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>netweevil run report</title><style>\
         :root{color-scheme:light;font-family:Georgia,\"Iowan Old Style\",serif;}\
         body{margin:0;background:#f5f1e8;color:#1f1c18;}\
         main{max-width:960px;margin:0 auto;padding:40px 24px 64px;}\
         h1,h2{font-family:\"Palatino Linotype\",\"Book Antiqua\",serif;letter-spacing:0.02em;}\
         section{background:rgba(255,255,255,0.72);border:1px solid #d8cdbb;border-radius:18px;padding:24px;margin:20px 0;box-shadow:0 10px 30px rgba(67,52,31,0.08);}\
         dl{display:grid;grid-template-columns:max-content 1fr;gap:10px 16px;margin:0;}\
         dt{font-weight:700;}dd{margin:0;}\
         ul{margin:0;padding-left:20px;}\
         code{font-family:\"SFMono-Regular\",\"Consolas\",monospace;background:#efe6d6;padding:0.1rem 0.3rem;border-radius:6px;}\
         p{line-height:1.6;}\
         </style></head><body><main>",
    );
    html.push_str("<h1>netweevil run report</h1>");
    html.push_str("<section><h2>Run</h2><dl>");
    push_definition(&mut html, "Run ID", &code_html(&manifest.run_id));
    push_definition(
        &mut html,
        "Status",
        &code_html(&format!("{:?}", manifest.status)),
    );
    push_definition(
        &mut html,
        "Kind",
        &code_html(&format!("{:?}", manifest.run_kind)),
    );
    push_definition(&mut html, "Created", &code_html(&manifest.created_at));
    push_definition(&mut html, "Dataset", &code_html(&manifest.dataset_id));
    push_definition(&mut html, "Profile", &code_html(&manifest.profile_id));
    push_definition(
        &mut html,
        "Compiled bundle",
        &code_html(
            manifest
                .compiled_profile_bundle_id
                .as_deref()
                .unwrap_or("not_recorded"),
        ),
    );
    push_definition(
        &mut html,
        "Request source",
        &code_html(&manifest.request_source),
    );
    push_definition(
        &mut html,
        "Result path",
        &code_html(manifest.result_path.as_deref().unwrap_or("not_written")),
    );
    push_definition(
        &mut html,
        "Software",
        &escape_html(&format!(
            "{} {}",
            manifest.software.executable, manifest.software.version
        )),
    );
    push_definition(
        &mut html,
        "Git commit",
        &code_html(manifest.software.git_commit.as_deref().unwrap_or("unknown")),
    );
    push_definition(
        &mut html,
        "Algorithm engine",
        &code_html(&manifest.algorithm.engine),
    );
    push_definition(
        &mut html,
        "Graph model",
        &code_html(&manifest.algorithm.graph_model),
    );
    push_definition(
        &mut html,
        "Acceleration",
        &code_html(&manifest.algorithm.acceleration),
    );
    push_definition(
        &mut html,
        "Connectivity policy",
        &code_html(&compact_json_or_not_recorded(
            manifest.connectivity_policy.as_ref(),
        )),
    );
    push_definition(
        &mut html,
        "Fallback policy",
        &code_html(&compact_json_or_not_recorded(
            manifest.fallback_policy.as_ref(),
        )),
    );
    html.push_str("</dl></section>");

    if let Some(summary) = result_summary {
        html.push_str("<section><h2>Results</h2>");
        push_summary_html(&mut html, summary);
        html.push_str("</section>");
    }

    html.push_str("<section><h2>Methods</h2>");
    html.push_str(&format!(
        "<p>{}</p><ul><li>Locked profile hash: {}</li><li>Defaults pack: {}</li></ul>",
        escape_html(&manifest.methods_summary.plain_language),
        code_html(
            manifest
                .methods_summary
                .locked_profile_hash
                .as_deref()
                .unwrap_or("not_recorded")
        ),
        code_html(
            manifest
                .methods_summary
                .defaults_pack
                .as_deref()
                .unwrap_or("not_recorded")
        ),
    ));
    html.push_str("</section>");

    html.push_str(&format!(
        "<section><h2>Message</h2><p>{}</p></section></main></body></html>",
        escape_html(&manifest.message)
    ));
    html
}

fn push_summary_markdown(markdown: &mut String, summary: &RunResultSummary) {
    match summary {
        RunResultSummary::Route(route) => {
            markdown.push_str(&format!(
                "- Route ID: `{}`\n\
                 - Outcome: `{}`\n\
                 - Total distance (m): `{}`\n\
                 - Total travel time (s): `{:.3}`\n\
                 - Total generalized cost: `{:.3}`\n\
                 - Illegal movement penalty (s): `{:.3}`\n\
                 - Illegal movement penalty (cost): `{:.3}`\n\
                 - Violations: `{}`\n\
                 - Segment count: `{}`\n\
                 - Diagnostics: `{}`\n",
                route.route_id,
                analysis_outcome_name(route.outcome),
                route.total_distance_m,
                route.total_travel_time_s,
                route.total_generalized_cost,
                route.illegal_movement_penalty_s,
                route.illegal_movement_penalty_cost,
                route.violation_count,
                route.segment_count,
                route.diagnostics_count,
            ));
            push_warning_markdown(markdown, &route.warnings);
        }
        RunResultSummary::Batch(batch) => {
            markdown.push_str(&format!(
                "- Item type: `{}`\n\
                 - Item count: `{}`\n\
                 - Succeeded: `{}`\n\
                 - Ignored: `{}`\n\
                 - Failed: `{}`\n\
                 - Legal: `{}`\n\
                 - Degraded: `{}`\n\
                 - Partial: `{}`\n\
                 - Unreachable: `{}`\n\
                 - Diagnostics: `{}`\n",
                batch.label,
                batch.item_count,
                batch.succeeded_count,
                batch.ignored_count,
                batch.failed_count,
                batch.legal_count,
                batch.degraded_count,
                batch.partial_count,
                batch.unreachable_count,
                batch.diagnostics_count,
            ));
            push_warning_markdown(markdown, &batch.warnings);
        }
        RunResultSummary::ServiceArea(service_area) => {
            markdown.push_str(&format!(
                "- Analysis ID: `{}`\n\
                 - Outcome: `{}`\n\
                 - Output mode: `{}`\n\
                 - Band mode: `{}`\n\
                 - Multi-origin mode: `{}`\n\
                 - Origins requested: `{}`\n\
                 - Origins processed: `{}`\n\
                 - Origins skipped: `{}`\n\
                 - Origins with fallback: `{}`\n\
                 - Thresholds: `{}`\n\
                 - Features: `{}`\n\
                 - Diagnostics: `{}`\n",
                service_area.analysis_id,
                analysis_outcome_name(service_area.outcome),
                service_area_output_mode_name(service_area.output_mode),
                service_area_band_mode_name(service_area.band_mode),
                service_area_multi_origin_mode_name(service_area.multi_origin_mode),
                service_area.origin_count,
                service_area.processed_origin_count,
                service_area.skipped_origin_count,
                service_area.fallback_origin_count,
                service_area.threshold_count,
                service_area.feature_count,
                service_area.diagnostics_count,
            ));
            push_warning_markdown(markdown, &service_area.warnings);
        }
    }
}

fn push_warning_markdown(markdown: &mut String, warnings: &[String]) {
    if warnings.is_empty() {
        markdown.push_str("- Warnings: `none`\n");
        return;
    }
    markdown.push_str("- Warnings:\n");
    for warning in warnings {
        markdown.push_str(&format!("  - {}\n", warning));
    }
}

fn push_summary_html(html: &mut String, summary: &RunResultSummary) {
    match summary {
        RunResultSummary::Route(route) => {
            html.push_str("<dl>");
            push_definition(html, "Route ID", &code_html(&route.route_id));
            push_definition(
                html,
                "Outcome",
                &code_html(analysis_outcome_name(route.outcome)),
            );
            push_definition(
                html,
                "Total distance (m)",
                &code_html(&route.total_distance_m.to_string()),
            );
            push_definition(
                html,
                "Total travel time (s)",
                &code_html(&format!("{:.3}", route.total_travel_time_s)),
            );
            push_definition(
                html,
                "Total generalized cost",
                &code_html(&format!("{:.3}", route.total_generalized_cost)),
            );
            push_definition(
                html,
                "Illegal movement penalty (s)",
                &code_html(&format!("{:.3}", route.illegal_movement_penalty_s)),
            );
            push_definition(
                html,
                "Illegal movement penalty (cost)",
                &code_html(&format!("{:.3}", route.illegal_movement_penalty_cost)),
            );
            push_definition(
                html,
                "Violations",
                &code_html(&route.violation_count.to_string()),
            );
            push_definition(
                html,
                "Segment count",
                &code_html(&route.segment_count.to_string()),
            );
            push_definition(
                html,
                "Diagnostics",
                &code_html(&route.diagnostics_count.to_string()),
            );
            html.push_str("</dl>");
            push_warning_html(html, &route.warnings);
        }
        RunResultSummary::Batch(batch) => {
            html.push_str("<dl>");
            push_definition(html, "Item type", &code_html(batch.label));
            push_definition(
                html,
                "Item count",
                &code_html(&batch.item_count.to_string()),
            );
            push_definition(
                html,
                "Succeeded",
                &code_html(&batch.succeeded_count.to_string()),
            );
            push_definition(
                html,
                "Ignored",
                &code_html(&batch.ignored_count.to_string()),
            );
            push_definition(html, "Failed", &code_html(&batch.failed_count.to_string()));
            push_definition(html, "Legal", &code_html(&batch.legal_count.to_string()));
            push_definition(
                html,
                "Degraded",
                &code_html(&batch.degraded_count.to_string()),
            );
            push_definition(
                html,
                "Partial",
                &code_html(&batch.partial_count.to_string()),
            );
            push_definition(
                html,
                "Unreachable",
                &code_html(&batch.unreachable_count.to_string()),
            );
            push_definition(
                html,
                "Diagnostics",
                &code_html(&batch.diagnostics_count.to_string()),
            );
            html.push_str("</dl>");
            push_warning_html(html, &batch.warnings);
        }
        RunResultSummary::ServiceArea(service_area) => {
            html.push_str("<dl>");
            push_definition(html, "Analysis ID", &code_html(&service_area.analysis_id));
            push_definition(
                html,
                "Outcome",
                &code_html(analysis_outcome_name(service_area.outcome)),
            );
            push_definition(
                html,
                "Output mode",
                &code_html(service_area_output_mode_name(service_area.output_mode)),
            );
            push_definition(
                html,
                "Band mode",
                &code_html(service_area_band_mode_name(service_area.band_mode)),
            );
            push_definition(
                html,
                "Multi-origin mode",
                &code_html(service_area_multi_origin_mode_name(
                    service_area.multi_origin_mode,
                )),
            );
            push_definition(
                html,
                "Origins requested",
                &code_html(&service_area.origin_count.to_string()),
            );
            push_definition(
                html,
                "Origins processed",
                &code_html(&service_area.processed_origin_count.to_string()),
            );
            push_definition(
                html,
                "Origins skipped",
                &code_html(&service_area.skipped_origin_count.to_string()),
            );
            push_definition(
                html,
                "Origins with fallback",
                &code_html(&service_area.fallback_origin_count.to_string()),
            );
            push_definition(
                html,
                "Thresholds",
                &code_html(&service_area.threshold_count.to_string()),
            );
            push_definition(
                html,
                "Features",
                &code_html(&service_area.feature_count.to_string()),
            );
            push_definition(
                html,
                "Diagnostics",
                &code_html(&service_area.diagnostics_count.to_string()),
            );
            html.push_str("</dl>");
            push_warning_html(html, &service_area.warnings);
        }
    }
}

#[derive(Default)]
struct OutcomeTally {
    legal_count: usize,
    degraded_count: usize,
    partial_count: usize,
    unreachable_count: usize,
    diagnostics_count: usize,
}

fn tally_outcomes(items: impl IntoIterator<Item = (AnalysisOutcome, usize)>) -> OutcomeTally {
    let mut tally = OutcomeTally::default();
    for (outcome, diagnostics_count) in items {
        match outcome {
            AnalysisOutcome::Legal => tally.legal_count += 1,
            AnalysisOutcome::Degraded => tally.degraded_count += 1,
            AnalysisOutcome::Partial => tally.partial_count += 1,
            AnalysisOutcome::Unreachable | AnalysisOutcome::NotImplemented => {
                tally.unreachable_count += 1
            }
        }
        tally.diagnostics_count += diagnostics_count;
    }
    tally
}

fn analysis_outcome_name(outcome: AnalysisOutcome) -> &'static str {
    match outcome {
        AnalysisOutcome::Legal => "legal",
        AnalysisOutcome::Degraded => "degraded",
        AnalysisOutcome::Partial => "partial",
        AnalysisOutcome::Unreachable => "unreachable",
        AnalysisOutcome::NotImplemented => "not_implemented",
    }
}

fn service_area_output_mode_name(mode: ServiceAreaOutputMode) -> &'static str {
    match mode {
        ServiceAreaOutputMode::Network => "network",
        ServiceAreaOutputMode::Polygon => "polygon",
        ServiceAreaOutputMode::Both => "both",
    }
}

fn service_area_band_mode_name(mode: ServiceAreaBandMode) -> &'static str {
    match mode {
        ServiceAreaBandMode::Cumulative => "cumulative",
        ServiceAreaBandMode::Ring => "ring",
    }
}

fn service_area_multi_origin_mode_name(mode: ServiceAreaMultiOriginMode) -> &'static str {
    match mode {
        ServiceAreaMultiOriginMode::Merge => "merge",
        ServiceAreaMultiOriginMode::Overlap => "overlap",
        ServiceAreaMultiOriginMode::Cut => "cut",
    }
}

fn compact_json_or_not_recorded<T: Serialize>(value: Option<&T>) -> String {
    value
        .map(|value| serde_json::to_string(value).unwrap_or_else(|_| "\"unavailable\"".to_string()))
        .unwrap_or_else(|| "not_recorded".to_string())
}

fn push_warning_html(html: &mut String, warnings: &[String]) {
    if warnings.is_empty() {
        html.push_str("<p>Warnings: <code>none</code></p>");
        return;
    }
    html.push_str("<p>Warnings</p><ul>");
    for warning in warnings {
        html.push_str(&format!("<li>{}</li>", escape_html(warning)));
    }
    html.push_str("</ul>");
}

fn push_definition(html: &mut String, key: &str, value: &str) {
    html.push_str(&format!("<dt>{}</dt><dd>{}</dd>", escape_html(key), value));
}

fn code_html(value: &str) -> String {
    format!("<code>{}</code>", escape_html(value))
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::{
        AlgorithmInfo, MethodsSummary, RouteSummary, RunKind, RunManifest, RunResultSummary,
        RunStatus, SoftwareInfo, load_run_result_summary, render_run_html, render_run_markdown,
    };
    use netweevil_query::{
        AnalysisOutcome, ConnectivityPolicy, DisconnectedNetworkMode, FallbackPolicy,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn renders_markdown_with_result_summary() {
        let manifest = test_manifest();
        let summary = RunResultSummary::Route(RouteSummary {
            route_id: "baseline_route".to_string(),
            outcome: AnalysisOutcome::Legal,
            total_distance_m: 1_665,
            total_travel_time_s: 152.496,
            total_generalized_cost: 152.496,
            illegal_movement_penalty_s: 0.0,
            illegal_movement_penalty_cost: 0.0,
            violation_count: 0,
            segment_count: 12,
            diagnostics_count: 1,
            warnings: vec!["Turn penalties are not modeled yet.".to_string()],
        });

        let markdown = render_run_markdown(&manifest, Some(&summary));

        assert!(markdown.contains("## Results"));
        assert!(markdown.contains("baseline_route"));
        assert!(markdown.contains("152.496"));
        assert!(markdown.contains("Locked profile hash"));
        assert!(markdown.contains("Outcome"));
        assert!(markdown.contains("Connectivity policy"));
    }

    #[test]
    fn renders_html_with_escaped_message() {
        let mut manifest = test_manifest();
        manifest.message = "Solved <route> & archived.".to_string();

        let html = render_run_html(&manifest, None);

        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("Solved &lt;route&gt; &amp; archived."));
        assert!(html.contains("netweevil run report"));
    }

    #[test]
    fn skips_result_summary_for_non_json_outputs() {
        let temp_path = std::env::temp_dir().join(format!(
            "netweevil-report-{}.geojson",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time works")
                .as_nanos()
        ));
        fs::write(
            &temp_path,
            "{\"type\":\"FeatureCollection\",\"features\":[]}",
        )
        .expect("geojson written");

        let mut manifest = test_manifest();
        manifest.result_path = Some(temp_path.display().to_string());

        let summary = load_run_result_summary(&manifest).expect("summary lookup succeeds");

        assert!(summary.is_none());
        fs::remove_file(temp_path).ok();
    }

    fn test_manifest() -> RunManifest {
        RunManifest {
            run_id: "run-123".to_string(),
            run_kind: RunKind::Route,
            status: RunStatus::Succeeded,
            created_at: "2026-03-18T12:00:00Z".to_string(),
            dataset_id: "groningen_2026_03".to_string(),
            profile_id: "car_research_v1".to_string(),
            compiled_profile_bundle_id: Some("metric-groningen-abc".to_string()),
            request_source: "examples/requests/route.json".to_string(),
            result_path: Some(".netweevil/runs/run-123-result.json".to_string()),
            report_path: None,
            software: SoftwareInfo {
                executable: "netweevil".to_string(),
                version: "0.1.0".to_string(),
                git_commit: Some("abc123".to_string()),
            },
            algorithm: AlgorithmInfo {
                engine: "dijkstra_exact".to_string(),
                graph_model: "directed_edge_graph".to_string(),
                acceleration: "none".to_string(),
            },
            methods_summary: MethodsSummary {
                plain_language: "Exact Dijkstra search over the compiled graph.".to_string(),
                locked_profile_hash: Some("deadbeef".to_string()),
                defaults_pack: Some("research".to_string()),
            },
            connectivity_policy: Some(ConnectivityPolicy {
                disconnected: DisconnectedNetworkMode::Strict,
                max_hop_distance_m: None,
                report_hop_distance_separately: false,
            }),
            fallback_policy: Some(FallbackPolicy::default()),
            message: "Route solved.".to_string(),
        }
    }
}
