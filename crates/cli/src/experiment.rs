use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;
use netweevil_persist::{WorkspacePaths, write_json};
use netweevil_profile::load_profile;
use netweevil_query::{
    AnalysisKind, load_experiment, load_od_pairs, load_point_set, load_route_request,
    load_service_area_request,
};
use serde::Serialize;

use crate::analyze::{
    StoredRun, run_matrix_analysis, run_od_analysis, run_route_analysis, run_service_area_analysis,
};
use crate::profile::profile_compile;

#[derive(Subcommand, Debug)]
pub(crate) enum ExperimentCommand {
    Run { study: PathBuf },
}

#[derive(Debug, Serialize)]
struct ExperimentRunSummary {
    experiment_id: String,
    label: String,
    dataset: String,
    created_at: String,
    status: String,
    scenario_count: usize,
    succeeded_count: usize,
    failed_count: usize,
    scenarios: Vec<ExperimentScenarioSummary>,
}

#[derive(Debug, Serialize)]
struct ExperimentScenarioSummary {
    scenario_id: String,
    analysis: &'static str,
    profile_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    origins_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    destinations_path: Option<String>,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_manifest_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub(crate) fn experiment_run(paths: &WorkspacePaths, study: &Path) -> Result<()> {
    let experiment = load_experiment(study)?;
    let study_dir = study.parent().unwrap_or_else(|| Path::new("."));
    let mut scenarios = Vec::with_capacity(experiment.scenarios.len());
    let mut succeeded_count = 0_usize;

    for (index, scenario) in experiment.scenarios.iter().enumerate() {
        let scenario_id = scenario_id(index, scenario);
        let profile_path = resolve_study_path(study_dir, &scenario.profile);
        let request_path = scenario
            .request
            .as_ref()
            .map(|path| resolve_study_path(study_dir, path));
        let origins_path = scenario
            .origins
            .as_ref()
            .map(|path| resolve_study_path(study_dir, path));
        let destinations_path = scenario
            .destinations
            .as_ref()
            .map(|path| resolve_study_path(study_dir, path));
        let out_path = scenario
            .out
            .as_ref()
            .map(|path| resolve_study_path(study_dir, path))
            .or_else(|| {
                Some(paths.runs_dir.join(format!(
                    "experiment-{}-{}-result.json",
                    experiment.experiment.id, scenario_id
                )))
            });

        let scenario_result = match run_experiment_scenario(
            paths,
            &experiment.experiment.dataset,
            scenario.analysis,
            &profile_path,
            request_path.as_deref(),
            origins_path.as_deref(),
            destinations_path.as_deref(),
            out_path.clone(),
        ) {
            Ok(stored) => {
                succeeded_count += 1;
                ExperimentScenarioSummary {
                    scenario_id,
                    analysis: analysis_name(scenario.analysis),
                    profile_path: profile_path.display().to_string(),
                    request_path: request_path.as_ref().map(|path| path.display().to_string()),
                    origins_path: origins_path.as_ref().map(|path| path.display().to_string()),
                    destinations_path: destinations_path
                        .as_ref()
                        .map(|path| path.display().to_string()),
                    status: "succeeded".to_string(),
                    result_path: Some(stored.result_path.display().to_string()),
                    run_manifest_path: Some(stored.manifest_path.display().to_string()),
                    summary: stored.summary,
                    error: None,
                }
            }
            Err(error) => ExperimentScenarioSummary {
                scenario_id,
                analysis: analysis_name(scenario.analysis),
                profile_path: profile_path.display().to_string(),
                request_path: request_path.as_ref().map(|path| path.display().to_string()),
                origins_path: origins_path.as_ref().map(|path| path.display().to_string()),
                destinations_path: destinations_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
                status: "failed".to_string(),
                result_path: out_path.map(|path| path.display().to_string()),
                run_manifest_path: None,
                summary: None,
                error: Some(error.to_string()),
            },
        };
        scenarios.push(scenario_result);
    }

    let failed_count = scenarios.len().saturating_sub(succeeded_count);
    let summary = ExperimentRunSummary {
        experiment_id: experiment.experiment.id.clone(),
        label: experiment.experiment.label.clone(),
        dataset: experiment.experiment.dataset.clone(),
        created_at: netweevil_manifest::now_rfc3339()?,
        status: if failed_count == 0 {
            "succeeded".to_string()
        } else if succeeded_count == 0 {
            "failed".to_string()
        } else {
            "partial".to_string()
        },
        scenario_count: scenarios.len(),
        succeeded_count,
        failed_count,
        scenarios,
    };
    let summary_path = paths
        .runs_dir
        .join(format!("experiment-{}.json", experiment.experiment.id));
    write_json(&summary_path, &summary)?;
    println!(
        "experiment summary written to {} ({} succeeded, {} failed)",
        summary_path.display(),
        summary.succeeded_count,
        summary.failed_count
    );
    Ok(())
}

fn run_experiment_scenario(
    paths: &WorkspacePaths,
    dataset_id: &str,
    analysis: AnalysisKind,
    profile_path: &Path,
    request_path: Option<&Path>,
    origins_path: Option<&Path>,
    destinations_path: Option<&Path>,
    out: Option<PathBuf>,
) -> Result<StoredRun> {
    profile_compile(paths, dataset_id, profile_path)?;
    let profile = load_profile(profile_path)?;
    profile.validate()?;
    match analysis {
        AnalysisKind::Route => {
            let request_path = request_path.context("route scenario is missing a request path")?;
            let request = load_route_request(request_path)?;
            run_route_analysis(paths, dataset_id, &profile, request_path, request, out)
        }
        AnalysisKind::Od => {
            let request_path = request_path.context("OD scenario is missing a request path")?;
            let request = load_od_pairs(request_path)?;
            run_od_analysis(paths, dataset_id, &profile, request_path, request, out)
        }
        AnalysisKind::Matrix => {
            let origins_path =
                origins_path.context("matrix scenario is missing an origins path")?;
            let destinations_path =
                destinations_path.context("matrix scenario is missing a destinations path")?;
            let origins = load_point_set(origins_path)?;
            let destinations = load_point_set(destinations_path)?;
            run_matrix_analysis(
                paths,
                dataset_id,
                &profile,
                origins_path,
                destinations_path,
                origins,
                destinations,
                out,
            )
        }
        AnalysisKind::ServiceArea => {
            let request_path =
                request_path.context("service-area scenario is missing a request path")?;
            let request = load_service_area_request(request_path)?;
            run_service_area_analysis(paths, dataset_id, &profile, request_path, &request, out)
        }
    }
}

fn scenario_id(index: usize, scenario: &netweevil_query::ScenarioSpec) -> String {
    scenario
        .id
        .clone()
        .unwrap_or_else(|| format!("{:02}_{}", index + 1, analysis_name(scenario.analysis)))
}

fn analysis_name(analysis: AnalysisKind) -> &'static str {
    match analysis {
        AnalysisKind::Route => "route",
        AnalysisKind::Od => "od",
        AnalysisKind::Matrix => "matrix",
        AnalysisKind::ServiceArea => "service_area",
    }
}

fn resolve_study_path(base_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}
