use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{
    ScenarioAnalysisSnapshot, ScenarioBatchRequest, ScenarioBatchSelector, ScenarioFeatureScore,
    ScenarioGroupSelector, ScenarioTopKSelector, diff_snapshots, load_scenario_batch_request,
    load_scenario_feature_ranking, rank_source_features,
};
use crate::{
    AccessibilityCategoryResult, AccessibilityResult, AnalysisOutcome, BatchItemStatus,
    BetweennessEdgeScore, BetweennessPairResult, BetweennessResult, MatrixResult, OdResult,
    RouteResult, ScenarioAccessibilityExecution, ScenarioBetweennessExecution,
    ScenarioMatrixExecution, ScenarioOdExecution, ScenarioRouteExecution,
};

use BatchItemStatus::{Failed, Succeeded};

#[test]
fn computes_disconnection_and_accessibility_loss() {
    let baseline = ScenarioAnalysisSnapshot {
        routes: vec![route_execution(Some(route_result(10.0)))],
        accessibility: vec![accessibility_execution(accessibility_result(Succeeded, 3))],
        ..Default::default()
    };
    let scenario = ScenarioAnalysisSnapshot {
        routes: vec![route_execution(None)],
        accessibility: vec![accessibility_execution(accessibility_result(Failed, 0))],
        ..Default::default()
    };
    let diff = diff_snapshots(&baseline, &scenario);
    assert_eq!(diff.disconnected_route_count, 1);
    assert_eq!(diff.disconnected_accessibility_row_count, 1);
    assert_eq!(diff.disconnected_demand_count, 2);
    assert_eq!(diff.accessibility_reachable_destination_loss, 3);
}

#[test]
fn computes_od_matrix_and_weighted_betweenness_loss() {
    let baseline = ScenarioAnalysisSnapshot {
        od: vec![od_execution(Some(od_result(Succeeded, Some(10.0))))],
        matrices: vec![matrix_execution(matrix_result(Succeeded, Some(20.0)))],
        betweenness: vec![betweenness_execution(betweenness_result(
            1,
            0,
            6.0,
            vec![(0, 10, 6.0)],
        ))],
        ..Default::default()
    };
    let scenario = ScenarioAnalysisSnapshot {
        od: vec![od_execution(None)],
        matrices: vec![matrix_execution(matrix_result(Failed, None))],
        betweenness: vec![betweenness_execution(betweenness_result(
            0,
            1,
            0.0,
            Vec::new(),
        ))],
        ..Default::default()
    };

    let diff = diff_snapshots(&baseline, &scenario);
    assert_eq!(diff.disconnected_od_pair_count, 1);
    assert_eq!(diff.disconnected_matrix_cell_count, 1);
    assert_eq!(diff.disconnected_betweenness_pair_count, 1);
    assert_eq!(diff.disconnected_demand_count, 3);
    assert_eq!(diff.weighted_disconnected_demand, 6.0);
    assert_eq!(diff.betweenness_edge_score_absolute_change, 6.0);
}

#[test]
fn exact_betweenness_diff_tracks_lost_baseline_pairs() {
    let mut baseline_result = betweenness_result(2, 0, 12.0, Vec::new());
    baseline_result.pairs = vec![pair(0, 5.0, true), pair(1, 7.0, true)];
    // The scenario adds a routed pair the baseline never had; only the lost
    // baseline pair (destination 0) counts as disconnected demand.
    let mut scenario_result = betweenness_result(2, 1, 12.0, Vec::new());
    scenario_result.pairs = vec![pair(0, 5.0, false), pair(1, 7.0, true), pair(2, 5.0, true)];
    let baseline = ScenarioAnalysisSnapshot {
        betweenness: vec![betweenness_execution(baseline_result)],
        ..Default::default()
    };
    let scenario = ScenarioAnalysisSnapshot {
        betweenness: vec![betweenness_execution(scenario_result)],
        ..Default::default()
    };

    let diff = diff_snapshots(&baseline, &scenario);
    assert_eq!(diff.disconnected_betweenness_pair_count, 1);
    assert_eq!(diff.weighted_disconnected_demand, 5.0);
}

/// File references in a batch document are inlined relative to the
/// document; the executed request carries content only.
#[test]
fn loader_inlines_files_referenced_relative_to_the_document() {
    let directory = temp_path("netweevil-scenario-batch");
    for (file, contents) in [
        (
            "scenarios/closure.yml",
            "id: closure\nfeatures:\n  - source_feature_id: 10\n    force_closed: true\n",
        ),
        (
            "rankings/usage.csv",
            "source_feature_id,score\n10,2.5\n11,1\n",
        ),
        ("calendars/holidays.yml", "- 2026-12-25\n"),
        (
            "weather/shade.csv",
            "feature_id,t_start,t_end,shade\n10,2026-07-10T08:00:00Z,2026-07-10T09:00:00Z,0.4\n",
        ),
    ] {
        let path = directory.join(file);
        fs::create_dir_all(path.parent().unwrap()).expect("fixture directory");
        fs::write(path, contents).expect("fixture file");
    }
    let request_path = directory.join("batch.yml");
    fs::write(
        &request_path,
        r#"batch_id: relative_paths
scenarios:
  - id: closure
    overlay_file: scenarios/closure.yml
  - id: top_usage
    top_k: {ranking_file: rankings/usage.csv, count: 2}
  - id: mall
    group: {attribute: asset_group, value: mall}
routes:
  - route_id: example
    origin: {id: a, lon: 1.0, lat: 2.0}
    destination: {id: b, lon: 1.1, lat: 2.1}
    holiday_calendar_file: calendars/holidays.yml
    overlay_files: [weather/shade.csv]
matrices:
  - analysis_id: matrix
    origins:
      points: [{id: a, lon: 1.0, lat: 2.0}]
      scenario_file: scenarios/closure.yml
    destinations:
      points: [{id: b, lon: 1.1, lat: 2.1}]
"#,
    )
    .expect("scenario request fixture");

    let request = load_scenario_batch_request(&request_path);
    let _ = fs::remove_dir_all(&directory);
    let request = request.expect("loaded scenario batch");

    let ScenarioBatchSelector::Overlay { overlay } = &request.scenarios[0].selector else {
        panic!("expected an inline overlay");
    };
    assert_eq!(overlay.id.as_deref(), Some("closure"));
    assert!(overlay.features[0].force_closed);
    assert_eq!(
        request.scenarios[1].selector,
        ScenarioBatchSelector::TopK {
            top_k: ScenarioTopKSelector {
                ranking: vec![
                    ScenarioFeatureScore {
                        source_feature_id: 10,
                        score: 2.5
                    },
                    ScenarioFeatureScore {
                        source_feature_id: 11,
                        score: 1.0
                    },
                ],
                count: 2,
            }
        }
    );
    assert_eq!(
        request.scenarios[2].selector,
        ScenarioBatchSelector::Group {
            group: ScenarioGroupSelector {
                attribute: "asset_group".to_string(),
                value: "mall".to_string(),
            }
        }
    );
    let route = &request.routes[0].temporal;
    assert_eq!(route.holidays, ["2026-12-25"]);
    assert_eq!(route.overlays[0].rows[0].values["shade"], 0.4);
    assert_eq!(
        request.matrices[0]
            .origins
            .temporal
            .scenario
            .as_ref()
            .and_then(|scenario| scenario.id.as_deref()),
        Some("closure")
    );
}

#[test]
fn scenario_selector_rejects_multiple_sources() {
    let error = serde_yaml::from_str::<ScenarioBatchRequest>(
        r#"batch_id: ambiguous
scenarios:
  - id: ambiguous
    overlay: {id: closure}
    top_k: {ranking: [], count: 1}
routes: []
"#,
    )
    .expect_err("multiple scenario selector sources must be rejected");
    assert!(
        error.to_string().contains("exactly one selector"),
        "unexpected error: {error}"
    );
}

#[test]
fn ranks_source_features_from_prior_betweenness_result() {
    let path = temp_path("netweevil-ranking").with_extension("json");
    let prior = betweenness_result(1, 0, 1.0, vec![(0, 10, 3.0), (1, 10, 4.0), (2, 11, 6.0)]);
    fs::write(&path, serde_json::to_vec(&prior).unwrap()).expect("write betweenness ranking");

    let scores = load_scenario_feature_ranking(&path);
    let _ = fs::remove_file(&path);
    let ranked = rank_source_features(&scores.expect("ranking loads"), 2);
    // Way 10 sums to 7.0 across its two edges and outranks way 11.
    assert_eq!(ranked.expect("ranked source features"), vec![10, 11]);
}

fn temp_path(prefix: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{unique}"))
}

fn closed_error<T>(result: &Option<T>) -> Option<String> {
    result.is_none().then(|| "closed".to_string())
}

fn route_execution(result: Option<RouteResult>) -> ScenarioRouteExecution {
    ScenarioRouteExecution {
        route_id: "r1".to_string(),
        error: closed_error(&result),
        result,
    }
}

fn od_execution(result: Option<OdResult>) -> ScenarioOdExecution {
    ScenarioOdExecution {
        analysis_id: "od".to_string(),
        error: closed_error(&result),
        result,
    }
}

fn accessibility_execution(result: AccessibilityResult) -> ScenarioAccessibilityExecution {
    ScenarioAccessibilityExecution {
        analysis_id: "accessibility_1".to_string(),
        result: Some(result),
        error: None,
    }
}

fn matrix_execution(result: MatrixResult) -> ScenarioMatrixExecution {
    ScenarioMatrixExecution {
        analysis_id: "matrix".to_string(),
        result: Some(result),
        error: None,
    }
}

fn betweenness_execution(result: BetweennessResult) -> ScenarioBetweennessExecution {
    ScenarioBetweennessExecution {
        analysis_id: "usage".to_string(),
        result: Some(result),
        error: None,
    }
}

fn route_result(travel_time_s: f64) -> RouteResult {
    let snapped = |id: &str| {
        serde_json::json!({
            "point_id": id,
            "requested_lon": 0.0,
            "requested_lat": 0.0,
            "snapped_node_id": 0,
            "snapped_lon": 0.0,
            "snapped_lat": 0.0,
            "snapped_z": 0.0,
            "snap_distance_m": 0.0
        })
    };
    serde_json::from_value(serde_json::json!({
        "route_id": "r1",
        "origin": snapped("a"),
        "destination": snapped("b"),
        "outcome": "legal",
        "summary": {
            "total_distance_m": 100,
            "total_travel_time_s": travel_time_s,
            "total_generalized_cost": travel_time_s,
            "segment_count": 1
        }
    }))
    .expect("minimal route result")
}

fn od_result(status: BatchItemStatus, travel_time_s: Option<f64>) -> OdResult {
    serde_json::from_value(serde_json::json!({
        "pair_count": 1,
        "succeeded_count": usize::from(status == Succeeded),
        "failed_count": usize::from(status == Failed),
        "pairs": [{
            "pair_id": "a_b",
            "origin_id": "a",
            "destination_id": "b",
            "status": status,
            "total_travel_time_s": travel_time_s,
        }]
    }))
    .expect("minimal OD result")
}

fn matrix_result(status: BatchItemStatus, travel_time_s: Option<f64>) -> MatrixResult {
    serde_json::from_value(serde_json::json!({
        "origin_count": 1,
        "destination_count": 1,
        "cell_count": 1,
        "succeeded_count": usize::from(status == Succeeded),
        "failed_count": usize::from(status == Failed),
        "cells": [{
            "origin_id": "a",
            "destination_id": "b",
            "status": status,
            "total_travel_time_s": travel_time_s,
        }]
    }))
    .expect("minimal matrix result")
}

fn pair(destination_index: usize, demand: f64, routed: bool) -> BetweennessPairResult {
    BetweennessPairResult {
        origin_index: 0,
        destination_index,
        demand,
        routed,
    }
}

/// One origin/destination pair with demand 6.0 (or `routed_demand` when
/// larger), plus edge scores given as `(edge_id, source_way_id, score)`.
fn betweenness_result(
    routed_pair_count: usize,
    unreachable_pair_count: usize,
    routed_demand: f64,
    edges: Vec<(u32, i64, f64)>,
) -> BetweennessResult {
    BetweennessResult {
        analysis_id: "usage".to_string(),
        departure_time: None,
        arrive_by: None,
        scenario_id: None,
        origin_count: 1,
        destination_count: 1,
        requested_pair_count: 1,
        routed_pair_count,
        unreachable_pair_count,
        routed_demand,
        time_dependent: true,
        pairs: vec![pair(0, routed_demand.max(6.0), routed_pair_count > 0)],
        edges: edges
            .into_iter()
            .map(|(edge_id, source_way_id, score)| BetweennessEdgeScore {
                edge_id,
                source_way_id,
                from_node_id: edge_id,
                to_node_id: edge_id + 1,
                score,
                normalized_score: 1.0,
                routed_pair_count: u64::from(routed_pair_count > 0),
                geometry: Vec::new(),
            })
            .collect(),
        warnings: Vec::new(),
    }
}

fn accessibility_result(status: BatchItemStatus, count: usize) -> AccessibilityResult {
    AccessibilityResult {
        departure_time: None,
        arrive_by: None,
        scenario_id: None,
        origin_count: 1,
        category_count: 1,
        destination_count: 3,
        max_travel_time_s: 900.0,
        thresholds_s: vec![900.0],
        row_count: 1,
        succeeded_count: usize::from(status == Succeeded),
        failed_count: usize::from(status == Failed),
        skipped_origin_count: 0,
        rows: vec![AccessibilityCategoryResult {
            origin_id: "a".to_string(),
            category_id: "jobs".to_string(),
            status,
            outcome: if status == Succeeded {
                AnalysisOutcome::Legal
            } else {
                AnalysisOutcome::Unreachable
            },
            fallback_used: false,
            origin_component_id: None,
            origin_hop_distance_m: None,
            origin_snap_distance_m: None,
            destination_count: 3,
            snapped_destination_count: count,
            nearest_destination_id: None,
            nearest_travel_time_s: None,
            nearest_destination_snap_distance_m: None,
            counts_within_threshold_s: BTreeMap::from([("900".to_string(), count)]),
            diagnostics: Vec::new(),
            error: None,
        }],
        diagnostics: Vec::new(),
        warnings: Vec::new(),
    }
}
