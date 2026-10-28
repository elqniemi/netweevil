use netweevil_persist::manifest::{
    AlgorithmInfo, MethodsSummary, RunKind, RunManifest, RunStatus, SoftwareInfo,
};

use super::{
    RouteSummary, RunResultSummary, load_run_result_summary, render_run_html, render_run_markdown,
};
use netweevil_query::{
    AnalysisOutcome, ConnectivityPolicy, DisconnectedNetworkMode, FallbackPolicy,
};
use std::fs;

use crate::test_support::TempFile;

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
    assert!(markdown.contains("- Profile: `"));
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
    let result = TempFile::new("result.geojson");
    fs::write(&result, "{\"type\":\"FeatureCollection\",\"features\":[]}")
        .expect("geojson written");

    let mut manifest = test_manifest();
    manifest.result_path = Some(result.as_ref().display().to_string());

    let summary = load_run_result_summary(&manifest).expect("summary lookup succeeds");
    assert!(summary.is_none());
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
        effective_request: None,
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
