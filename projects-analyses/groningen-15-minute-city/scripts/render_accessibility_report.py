#!/usr/bin/env python3
"""Render a Markdown accessibility report from processed summary artifacts."""

from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path


STUDY_DIR = Path(__file__).resolve().parents[1]
SUMMARY = STUDY_DIR / "metadata/generated/accessibility_summary.json"
BOTTLENECKS = STUDY_DIR / "outputs/processed/category_bottlenecks.csv"
SENSITIVITY = STUDY_DIR / "metadata/generated/sensitivity_threshold_summary.json"
SENSITIVITY_SCENARIOS = STUDY_DIR / "metadata/generated/sensitivity_scenario_summary.json"
ROUTE_QUALITY = STUDY_DIR / "metadata/generated/cycling_route_quality_summary.json"
NEIGHBOURHOODS = STUDY_DIR / "metadata/generated/neighbourhood_accessibility_summary.json"
TRANSIT = STUDY_DIR / "metadata/generated/transit_neighbourhood_time_windows_summary.json"
TRANSIT_BUNDLE = STUDY_DIR / "metadata/generated/transit_service_bundle_summary.json"
OUT = STUDY_DIR / "outputs/reports/accessibility_report.md"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--summary", type=Path, default=SUMMARY)
    parser.add_argument("--bottlenecks", type=Path, default=BOTTLENECKS)
    parser.add_argument("--sensitivity", type=Path, default=SENSITIVITY)
    parser.add_argument("--sensitivity-scenarios", type=Path, default=SENSITIVITY_SCENARIOS)
    parser.add_argument("--route-quality", type=Path, default=ROUTE_QUALITY)
    parser.add_argument("--neighbourhoods", type=Path, default=NEIGHBOURHOODS)
    parser.add_argument("--transit", type=Path, default=TRANSIT)
    parser.add_argument("--transit-bundle", type=Path, default=TRANSIT_BUNDLE)
    parser.add_argument("--out", type=Path, default=OUT)
    args = parser.parse_args()

    with args.summary.open() as handle:
        summary = json.load(handle)
    bottlenecks = read_rows(args.bottlenecks)
    sensitivity = read_json(args.sensitivity)
    sensitivity_scenarios = read_json(args.sensitivity_scenarios)
    route_quality = read_json(args.route_quality)
    neighbourhoods = read_json(args.neighbourhoods)
    transit = read_json(args.transit)
    transit_bundle = read_json(args.transit_bundle)

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(
        render_markdown(
            summary,
            bottlenecks,
            sensitivity,
            sensitivity_scenarios,
            route_quality,
            neighbourhoods,
            transit,
            transit_bundle,
        )
    )


def read_rows(path: Path) -> list[dict[str, str]]:
    if not path.exists() or path.stat().st_size == 0:
        return []
    with path.open(newline="") as handle:
        return list(csv.DictReader(handle))


def read_json(path: Path) -> dict:
    if not path.exists() or path.stat().st_size == 0:
        return {}
    with path.open() as handle:
        return json.load(handle)


def render_markdown(
    summary: dict,
    bottlenecks: list[dict[str, str]],
    sensitivity: dict,
    sensitivity_scenarios: dict,
    route_quality: dict,
    neighbourhoods: dict,
    transit: dict,
    transit_bundle: dict,
) -> str:
    lines = [
        "# Groningen 15-Minute City Accessibility Report",
        "",
        f"Generated at: `{summary.get('generated_at', '')}`",
        "",
        f"Primary threshold: `{summary.get('primary_threshold_s', '')}` seconds",
        "",
        "## Coverage",
        "",
        f"- Nearest-service rows: `{summary.get('nearest_row_count', 0)}`",
        f"- Score rows: `{summary.get('score_row_count', 0)}`",
        "",
        "## Mode Completeness",
        "",
        "| Mode | Origins | Strict complete | Strict share | Relaxed complete | Relaxed share |",
        "| --- | ---: | ---: | ---: | ---: | ---: |",
    ]
    modes = summary.get("modes", {})
    if modes:
        for mode, values in sorted(modes.items()):
            lines.append(
                "| {mode} | {origin_count} | {strict_complete_count} | {strict_complete_share:.3f} | "
                "{relaxed_complete_count} | {relaxed_complete_share:.3f} |".format(
                    mode=mode,
                    origin_count=values.get("origin_count", 0),
                    strict_complete_count=values.get("strict_complete_count", 0),
                    strict_complete_share=float(values.get("strict_complete_share", 0.0)),
                    relaxed_complete_count=values.get("relaxed_complete_count", 0),
                    relaxed_complete_share=float(values.get("relaxed_complete_share", 0.0)),
                )
            )
    else:
        lines.append("| none | 0 | 0 | 0.000 | 0 | 0.000 |")

    lines.extend(
        [
            "",
            "## Category Timing",
            "",
            "| Mode | Category | Origins | Median nearest time (s) | Failed rows | Ignored rows |",
            "| --- | --- | ---: | ---: | ---: | ---: |",
        ]
    )
    for row in summary.get("category_stats", []):
        lines.append(
            f"| {row.get('mode', '')} | {row.get('category', '')} | {row.get('origin_count', '')} | "
            f"{row.get('median_nearest_travel_time_s', '')} | {row.get('failed_cell_count', '')} | {row.get('ignored_cell_count', '')} |"
        )
    if not summary.get("category_stats"):
        lines.append("| none | none | 0 |  | 0 | 0 |")

    lines.extend(["", "## Bottlenecks", "", "| Mode | Category | Missing origin count |", "| --- | --- | ---: |"])
    for row in bottlenecks:
        lines.append(f"| {row.get('mode', '')} | {row.get('category', '')} | {row.get('missing_origin_count', '')} |")
    if not bottlenecks:
        lines.append("| none | none | 0 |")

    threshold_modes = sensitivity.get("threshold_modes", [])
    lines.extend(
        [
            "",
            "## Threshold Sensitivity",
            "",
            "| Threshold (s) | Mode | Strict share | Relaxed share |",
            "| ---: | --- | ---: | ---: |",
        ]
    )
    for row in threshold_modes:
        lines.append(
            "| {threshold:.0f} | {mode} | {strict:.3f} | {relaxed:.3f} |".format(
                threshold=float(row.get("threshold_s", 0)),
                mode=row.get("mode", ""),
                strict=float(row.get("strict_complete_share", 0.0)),
                relaxed=float(row.get("relaxed_complete_share", 0.0)),
            )
        )
    if not threshold_modes:
        lines.append("|  | none | 0.000 | 0.000 |")

    scenario_bands = sensitivity_scenarios.get("bands", [])
    lines.extend(
        [
            "",
            "## Scenario Sensitivity Bands",
            "",
            f"Scenario rows: `{sensitivity_scenarios.get('scenario_row_count', 0)}`",
            "",
            "| Family | Mode | Bundle | Strict share band | Relaxed share band | Scenarios |",
            "| --- | --- | --- | ---: | ---: | ---: |",
        ]
    )
    for row in scenario_bands:
        lines.append(
            "| {family} | {mode} | {bundle} | {strict_min:.3f}-{strict_max:.3f} | "
            "{relaxed_min:.3f}-{relaxed_max:.3f} | {count} |".format(
                family=row.get("scenario_family", ""),
                mode=row.get("mode", ""),
                bundle=row.get("bundle", ""),
                strict_min=float(row.get("min_strict_complete_share", 0.0)),
                strict_max=float(row.get("max_strict_complete_share", 0.0)),
                relaxed_min=float(row.get("min_relaxed_complete_share", 0.0)),
                relaxed_max=float(row.get("max_relaxed_complete_share", 0.0)),
                count=row.get("scenario_count", 0),
            )
        )
    if not scenario_bands:
        lines.append("| none | none | none | 0.000-0.000 | 0.000-0.000 | 0 |")

    route_rows = route_quality.get("category_summary", [])
    lines.extend(
        [
            "",
            "## Cycling Route Quality Sample",
            "",
            f"Representative routes: `{route_quality.get('route_count', 0)}`",
            "",
            "| Category | Routes | Legal | Violations | Median time (s) | P90 time (s) | Median distance (m) |",
            "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    for row in route_rows:
        lines.append(
            f"| {row.get('category', '')} | {row.get('route_count', '')} | {row.get('legal_count', '')} | "
            f"{row.get('violation_count', '')} | {row.get('median_travel_time_s', '')} | "
            f"{row.get('p90_travel_time_s', '')} | {row.get('median_distance_m', '')} |"
        )
    if not route_rows:
        lines.append("| none | 0 | 0 | 0 |  |  |  |")

    neighbourhood_inequality = neighbourhoods.get("mode_inequality", [])
    lines.extend(
        [
            "",
            "## Neighbourhood Aggregation",
            "",
            f"OSM place polygons: `{neighbourhoods.get('neighbourhood_count', 0)}`",
            f"Assigned H3 cells: `{neighbourhoods.get('assigned_h3_count', 0)}`",
            f"Unassigned H3 cells: `{neighbourhoods.get('unassigned_h3_count', 0)}`",
            "",
            "| Mode | Neighbourhoods | P10 score | P90 score | IQR | P90/P10 |",
            "| --- | ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    for row in neighbourhood_inequality:
        ratio = row.get("p90_p10_ratio")
        lines.append(
            "| {mode} | {count} | {p10:.3f} | {p90:.3f} | {iqr:.3f} | {ratio} |".format(
                mode=row.get("mode", ""),
                count=row.get("neighbourhood_count", 0),
                p10=float(row.get("p10_weighted_strict_score", 0.0)),
                p90=float(row.get("p90_weighted_strict_score", 0.0)),
                iqr=float(row.get("iqr_weighted_strict_score", 0.0)),
                ratio="" if ratio is None else f"{float(ratio):.3f}",
            )
        )
    if not neighbourhood_inequality:
        lines.append("| none | 0 |  |  |  |  |")

    transit_windows = transit.get("window_summary", [])
    lines.extend(
        [
            "",
            "## Transit Time Windows",
            "",
            f"Neighbourhood-window routes: `{transit.get('route_count', 0)}`",
            f"Scheduled: `{transit.get('scheduled_count', 0)}`; unreachable: `{transit.get('unreachable_count', 0)}`",
            "",
            "| Window | Routes | Median time (s) | P90 time (s) | <=15m | <=30m | <=45m |",
            "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
        ]
    )
    for row in transit_windows:
        lines.append(
            f"| {row.get('window', '')} | {row.get('route_count', '')} | "
            f"{row.get('median_total_travel_time_s', '')} | {row.get('p90_total_travel_time_s', '')} | "
            f"{row.get('reachable_15m_count', '')} | {row.get('reachable_30m_count', '')} | {row.get('reachable_45m_count', '')} |"
        )
    if not transit_windows:
        lines.append("| none | 0 |  |  | 0 | 0 | 0 |")

    transit_bundle_rows = transit_bundle.get("window_thresholds", [])
    lines.extend(
        [
            "",
            "## Transit Service Bundle Sample",
            "",
            f"Sampled service-bundle routes: `{transit_bundle.get('route_count', 0)}`",
            f"Scheduled: `{transit_bundle.get('scheduled_count', 0)}`; unreachable: `{transit_bundle.get('unreachable_count', 0)}`",
            "",
            "| Window | Threshold (s) | Neighbourhoods | Strict share | Relaxed share | Top missing |",
            "| --- | ---: | ---: | ---: | ---: | --- |",
        ]
    )
    for row in transit_bundle_rows:
        lines.append(
            "| {window} | {threshold:.0f} | {count} | {strict:.3f} | {relaxed:.3f} | {missing} |".format(
                window=row.get("window", ""),
                threshold=float(row.get("threshold_s", 0)),
                count=row.get("origin_window_count", 0),
                strict=float(row.get("strict_complete_share", 0.0)),
                relaxed=float(row.get("relaxed_complete_share", 0.0)),
                missing=row.get("top_missing_category", ""),
            )
        )
    if not transit_bundle_rows:
        lines.append("| none | 0 | 0 | 0.000 | 0.000 |  |")

    lines.extend(
        [
            "",
            "## Interpretation Notes",
            "",
            "These results use bounded reduced accessibility outputs for the full H3 grid; matrix outputs are retained only for smoke/sample validation.",
            "Map deliverables include durable PNG maps/plots plus GIS-ready H3 GeoJSON layers with QGIS styles; SVG is used only as a temporary rendering format.",
            "Scenario sensitivity bands include residential-proxy weighted summaries; cycling-speed and snap-distance scenarios are post-processed from reduced outputs and should be read as sensitivity bounds rather than independent reroutes.",
            "Transit service-bundle scores are a 50-neighbourhood sample using nearest OSM POIs per baseline category, not a full scheduled transit matrix for every H3 cell.",
            "Neighbourhood aggregation uses OSM place polygons; unassigned H3 cells are reported separately because local place polygons do not cover every residential H3 cell.",
            "Use the implementation audit to distinguish smoke/sample outputs from full baseline outputs.",
            "",
        ]
    )
    return "\n".join(lines)


if __name__ == "__main__":
    main()
