mod output;

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use netan_core::{BuildStage, CacheBundleId, DatasetId, TopologyBundleMeta, TravelMode};
use netan_profile::ProfileDocument;
use netan_query::{MatrixResult, OdResult, RouteResult};
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

pub use output::{write_matrix_result, write_od_result, write_route_result};

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
    pub topology_meta: Option<TopologyBundleMeta>,
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
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Route,
    Od,
    Matrix,
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
        message: message.into(),
    })
}

#[derive(Debug, Clone)]
pub enum RunResultSummary {
    Route(RouteSummary),
    Batch(BatchSummary),
}

#[derive(Debug, Clone)]
pub struct RouteSummary {
    pub route_id: String,
    pub total_distance_m: u64,
    pub total_travel_time_s: f64,
    pub total_generalized_cost: f64,
    pub segment_count: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BatchSummary {
    pub label: &'static str,
    pub item_count: usize,
    pub succeeded_count: usize,
    pub failed_count: usize,
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
                total_distance_m: route.summary.total_distance_m,
                total_travel_time_s: route.summary.total_travel_time_s,
                total_generalized_cost: route.summary.total_generalized_cost,
                segment_count: route.summary.segment_count,
                warnings: route.warnings,
            })
        }
        RunKind::Od => {
            let od: OdResult = serde_json::from_str(&raw).context("parsing OD result JSON")?;
            RunResultSummary::Batch(BatchSummary {
                label: "OD pairs",
                item_count: od.pair_count,
                succeeded_count: od.succeeded_count,
                failed_count: od.failed_count,
                warnings: od.warnings,
            })
        }
        RunKind::Matrix => {
            let matrix: MatrixResult =
                serde_json::from_str(&raw).context("parsing matrix result JSON")?;
            RunResultSummary::Batch(BatchSummary {
                label: "matrix cells",
                item_count: matrix.cell_count,
                succeeded_count: matrix.succeeded_count,
                failed_count: matrix.failed_count,
                warnings: matrix.warnings,
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
    markdown.push_str("# netan run report\n\n");
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
         - Acceleration: `{}`\n\n",
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
         <title>netan run report</title><style>\
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
    html.push_str("<h1>netan run report</h1>");
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
                 - Total distance (m): `{}`\n\
                 - Total travel time (s): `{:.3}`\n\
                 - Total generalized cost: `{:.3}`\n\
                 - Segment count: `{}`\n",
                route.route_id,
                route.total_distance_m,
                route.total_travel_time_s,
                route.total_generalized_cost,
                route.segment_count,
            ));
            push_warning_markdown(markdown, &route.warnings);
        }
        RunResultSummary::Batch(batch) => {
            markdown.push_str(&format!(
                "- Item type: `{}`\n\
                 - Item count: `{}`\n\
                 - Succeeded: `{}`\n\
                 - Failed: `{}`\n",
                batch.label, batch.item_count, batch.succeeded_count, batch.failed_count,
            ));
            push_warning_markdown(markdown, &batch.warnings);
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
                "Segment count",
                &code_html(&route.segment_count.to_string()),
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
            push_definition(html, "Failed", &code_html(&batch.failed_count.to_string()));
            html.push_str("</dl>");
            push_warning_html(html, &batch.warnings);
        }
    }
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
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn renders_markdown_with_result_summary() {
        let manifest = test_manifest();
        let summary = RunResultSummary::Route(RouteSummary {
            route_id: "baseline_route".to_string(),
            total_distance_m: 1_665,
            total_travel_time_s: 152.496,
            total_generalized_cost: 152.496,
            segment_count: 12,
            warnings: vec!["Turn penalties are not modeled yet.".to_string()],
        });

        let markdown = render_run_markdown(&manifest, Some(&summary));

        assert!(markdown.contains("## Results"));
        assert!(markdown.contains("baseline_route"));
        assert!(markdown.contains("152.496"));
        assert!(markdown.contains("Locked profile hash"));
    }

    #[test]
    fn renders_html_with_escaped_message() {
        let mut manifest = test_manifest();
        manifest.message = "Solved <route> & archived.".to_string();

        let html = render_run_html(&manifest, None);

        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("Solved &lt;route&gt; &amp; archived."));
        assert!(html.contains("netan run report"));
    }

    #[test]
    fn skips_result_summary_for_non_json_outputs() {
        let temp_path = std::env::temp_dir().join(format!(
            "netan-report-{}.geojson",
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
            result_path: Some(".netan/runs/run-123-result.json".to_string()),
            report_path: None,
            software: SoftwareInfo {
                executable: "netan".to_string(),
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
            message: "Route solved.".to_string(),
        }
    }
}
