#!/usr/bin/env python3
"""Render PNG maps and plots from processed accessibility artifacts.

SVG is used only as an in-memory/intermediate drawing format and converted to
PNG with rsvg-convert. The durable deliverables are PNG files.
"""

from __future__ import annotations

import argparse
import csv
import json
import shutil
import subprocess
import tempfile
from pathlib import Path


STUDY_DIR = Path(__file__).resolve().parents[1]
H3_SCORES = STUDY_DIR / "outputs/maps/accessibility_scores_h3.geojson"
SUMMARY = STUDY_DIR / "metadata/generated/accessibility_summary.json"
BOTTLENECKS = STUDY_DIR / "outputs/processed/category_bottlenecks.csv"
SENSITIVITY = STUDY_DIR / "metadata/generated/sensitivity_threshold_summary.json"
SENSITIVITY_SCENARIOS = STUDY_DIR / "metadata/generated/sensitivity_scenario_summary.json"
ROUTE_QUALITY = STUDY_DIR / "metadata/generated/cycling_route_quality_summary.json"
TRANSIT = STUDY_DIR / "metadata/generated/transit_neighbourhood_time_windows_summary.json"
TRANSIT_BUNDLE = STUDY_DIR / "metadata/generated/transit_service_bundle_summary.json"
MAP_DIR = STUDY_DIR / "outputs/maps/png"
PLOT_DIR = STUDY_DIR / "outputs/plots/png"
GIS_DIR = STUDY_DIR / "outputs/maps/gis"
ROUTE_QUALITY_ROUTES = GIS_DIR / "cycling_route_quality_routes.geojson"

WIDTH = 1200
HEIGHT = 900
PADDING = 58
MODES = ["walking", "cycling", "car"]
BASELINE_CATEGORIES = [
    "food_retail",
    "education",
    "healthcare",
    "public_services",
    "parks_recreation",
    "transit_stops",
]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--h3-scores", type=Path, default=H3_SCORES)
    parser.add_argument("--summary", type=Path, default=SUMMARY)
    parser.add_argument("--bottlenecks", type=Path, default=BOTTLENECKS)
    parser.add_argument("--sensitivity", type=Path, default=SENSITIVITY)
    parser.add_argument("--sensitivity-scenarios", type=Path, default=SENSITIVITY_SCENARIOS)
    parser.add_argument("--route-quality", type=Path, default=ROUTE_QUALITY)
    parser.add_argument("--transit", type=Path, default=TRANSIT)
    parser.add_argument("--transit-bundle", type=Path, default=TRANSIT_BUNDLE)
    parser.add_argument("--map-dir", type=Path, default=MAP_DIR)
    parser.add_argument("--plot-dir", type=Path, default=PLOT_DIR)
    args = parser.parse_args()

    converter = shutil.which("rsvg-convert")
    if converter is None:
        raise SystemExit("rsvg-convert is required to render PNG maps and plots")

    args.map_dir.mkdir(parents=True, exist_ok=True)
    args.plot_dir.mkdir(parents=True, exist_ok=True)

    with args.h3_scores.open() as handle:
        geojson = json.load(handle)
    with args.summary.open() as handle:
        summary = json.load(handle)
    bottlenecks = read_rows(args.bottlenecks)
    sensitivity = read_json(args.sensitivity)
    sensitivity_scenarios = read_json(args.sensitivity_scenarios)
    route_quality = read_json(args.route_quality)
    transit = read_json(args.transit)
    transit_bundle = read_json(args.transit_bundle)

    features_by_mode = group_features_by_mode(geojson.get("features", []))
    bounds = geometry_bounds(geojson.get("features", []))
    outputs: list[Path] = []

    for mode in MODES:
        features = features_by_mode.get(mode, [])
        if not features:
            continue
        outputs.append(
            render_png(
                converter,
                render_score_map(mode, features, bounds),
                args.map_dir / f"{mode}_strict_score_h3.png",
            )
        )
        outputs.append(
            render_png(
                converter,
                render_complete_map(mode, features, bounds),
                args.map_dir / f"{mode}_complete_15m_h3.png",
            )
        )
        outputs.append(
            render_png(
                converter,
                render_category_time_plot(mode, summary.get("category_stats", [])),
                args.plot_dir / f"{mode}_category_median_times.png",
            )
        )
        outputs.append(
            render_png(
                converter,
                render_bottleneck_plot(mode, bottlenecks),
                args.plot_dir / f"{mode}_baseline_missing_categories.png",
            )
        )

    outputs.append(
        render_png(
            converter,
            render_mode_comparison_map(features_by_mode, bounds),
            args.map_dir / "mode_comparison_strict_score_h3.png",
        )
    )
    outputs.append(
        render_png(
            converter,
            render_advantage_map(features_by_mode, bounds),
            args.map_dir / "car_vs_cycling_advantage_h3.png",
        )
    )
    outputs.append(
        render_png(
            converter,
            render_missing_service_map(features_by_mode.get("cycling", []), bounds),
            args.map_dir / "cycling_missing_service_adequacy_h3.png",
        )
    )
    outputs.append(
        render_png(
            converter,
            render_index_map(features_by_mode, bounds),
            args.map_dir / "fifteen_minute_index_h3.png",
        )
    )
    outputs.append(
        render_png(
            converter,
            render_category_small_multiples(GIS_DIR, bounds),
            args.map_dir / "cycling_nearest_service_time_small_multiples.png",
        )
    )
    outputs.append(
        render_png(
            converter,
            render_route_quality_map(ROUTE_QUALITY_ROUTES),
            args.map_dir / "cycling_route_quality_routes.png",
        )
    )

    outputs.append(
        render_png(
            converter,
            render_threshold_plot(sensitivity.get("threshold_modes", []), "strict_complete_share", "Strict Complete Share"),
            args.plot_dir / "threshold_sensitivity_strict.png",
        )
    )
    outputs.append(
        render_png(
            converter,
            render_threshold_plot(sensitivity.get("threshold_modes", []), "relaxed_complete_share", "Relaxed Complete Share"),
            args.plot_dir / "threshold_sensitivity_relaxed.png",
        )
    )
    outputs.append(
        render_png(
            converter,
            render_scenario_band_plot(sensitivity_scenarios.get("bands", [])),
            args.plot_dir / "sensitivity_scenario_bands.png",
        )
    )
    outputs.append(
        render_png(
            converter,
            render_route_quality_plot(route_quality.get("category_summary", [])),
            args.plot_dir / "cycling_route_quality_median_times.png",
        )
    )
    outputs.append(
        render_png(
            converter,
            render_transit_window_plot(transit.get("window_summary", [])),
            args.plot_dir / "transit_neighbourhood_time_windows.png",
        )
    )
    outputs.append(
        render_png(
            converter,
            render_transit_bundle_plot(transit_bundle.get("window_thresholds", [])),
            args.plot_dir / "transit_service_bundle_strict_scores.png",
        )
    )

    manifest = {
        "map_png_count": sum(1 for path in outputs if path.parent == args.map_dir),
        "plot_png_count": sum(1 for path in outputs if path.parent == args.plot_dir),
        "outputs": [str(path) for path in outputs],
    }
    (args.plot_dir / "png_outputs_manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")


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


def render_png(converter: str, svg: str, out: Path) -> Path:
    with tempfile.NamedTemporaryFile("w", suffix=".svg", delete=False) as handle:
        handle.write(svg)
        tmp_path = Path(handle.name)
    try:
        subprocess.run([converter, str(tmp_path), "-o", str(out)], check=True)
    finally:
        tmp_path.unlink(missing_ok=True)
    return out


def group_features_by_mode(features: list[dict]) -> dict[str, list[dict]]:
    grouped: dict[str, list[dict]] = {}
    for feature in features:
        mode = feature.get("properties", {}).get("mode")
        if mode:
            grouped.setdefault(mode, []).append(feature)
    return grouped


def geometry_bounds(features: list[dict]) -> tuple[float, float, float, float]:
    lons: list[float] = []
    lats: list[float] = []
    for feature in features:
        for lon, lat in iter_coords(feature.get("geometry", {})):
            lons.append(lon)
            lats.append(lat)
    return min(lons), min(lats), max(lons), max(lats)


def iter_coords(geometry: dict):
    if geometry.get("type") == "Polygon":
        for ring in geometry.get("coordinates", []):
            yield from ring
    elif geometry.get("type") == "MultiPolygon":
        for polygon in geometry.get("coordinates", []):
            for ring in polygon:
                yield from ring
    elif geometry.get("type") == "LineString":
        yield from geometry.get("coordinates", [])
    elif geometry.get("type") == "MultiLineString":
        for line in geometry.get("coordinates", []):
            yield from line


def project(lon: float, lat: float, bounds: tuple[float, float, float, float]) -> tuple[float, float]:
    min_lon, min_lat, max_lon, max_lat = bounds
    x = PADDING + (lon - min_lon) / (max_lon - min_lon) * (WIDTH - PADDING * 2)
    y = HEIGHT - PADDING - (lat - min_lat) / (max_lat - min_lat) * (HEIGHT - PADDING * 2)
    return x, y


def polygon_paths(feature: dict, bounds: tuple[float, float, float, float]) -> list[str]:
    geometry = feature.get("geometry", {})
    polygons = []
    if geometry.get("type") == "Polygon":
        polygons = [geometry.get("coordinates", [])]
    elif geometry.get("type") == "MultiPolygon":
        polygons = geometry.get("coordinates", [])
    paths = []
    for polygon in polygons:
        parts = []
        for ring in polygon:
            projected = [project(lon, lat, bounds) for lon, lat in ring]
            if not projected:
                continue
            first = projected[0]
            commands = [f"M {first[0]:.1f} {first[1]:.1f}"]
            commands.extend(f"L {x:.1f} {y:.1f}" for x, y in projected[1:])
            commands.append("Z")
            parts.append(" ".join(commands))
        if parts:
            paths.append(" ".join(parts))
    return paths


def render_score_map(mode: str, features: list[dict], bounds: tuple[float, float, float, float]) -> str:
    body = []
    for feature in features:
        score = float(feature.get("properties", {}).get("strict_score", 0.0) or 0.0)
        for path in polygon_paths(feature, bounds):
            body.append(f'<path d="{path}" fill="{score_color(score)}" stroke="#ffffff" stroke-width="0.28"/>')
    body.extend(
        [
            '<rect x="58" y="838" width="260" height="20" fill="url(#scoreGradient)"/>',
            '<text x="58" y="878" font-size="16">0</text><text x="300" y="878" font-size="16">1</text>',
        ]
    )
    return svg_page(
        f"{mode.title()} Strict 15-Minute Score",
        body,
        defs='<linearGradient id="scoreGradient"><stop offset="0%" stop-color="#d73027"/><stop offset="50%" stop-color="#fee08b"/><stop offset="100%" stop-color="#1a9850"/></linearGradient>',
    )


def render_complete_map(mode: str, features: list[dict], bounds: tuple[float, float, float, float]) -> str:
    body = []
    for feature in features:
        complete = feature.get("properties", {}).get("complete_15m") in {True, "true", "True"}
        color = "#1a9850" if complete else "#d73027"
        for path in polygon_paths(feature, bounds):
            body.append(f'<path d="{path}" fill="{color}" stroke="#ffffff" stroke-width="0.28"/>')
    body.extend(
        [
            '<rect x="58" y="838" width="20" height="20" fill="#1a9850"/><text x="88" y="855" font-size="16">complete</text>',
            '<rect x="210" y="838" width="20" height="20" fill="#d73027"/><text x="240" y="855" font-size="16">incomplete</text>',
        ]
    )
    return svg_page(f"{mode.title()} Complete 15-Minute Cells", body)


def render_mode_comparison_map(
    features_by_mode: dict[str, list[dict]],
    bounds: tuple[float, float, float, float],
) -> str:
    by_origin = score_features_by_origin(features_by_mode)
    colors = {
        "walking": "#7570b3",
        "cycling": "#1b9e77",
        "car": "#d95f02",
        "tie": "#6f6f6f",
    }
    body = []
    for origin_id, mode_features in by_origin.items():
        scores = {
            mode: float(feature.get("properties", {}).get("strict_score", 0.0) or 0.0)
            for mode, feature in mode_features.items()
        }
        if not scores:
            continue
        max_score = max(scores.values())
        winners = [mode for mode, score in scores.items() if abs(score - max_score) < 1.0e-9]
        winner = winners[0] if len(winners) == 1 else "tie"
        feature = mode_features.get("cycling") or next(iter(mode_features.values()))
        for path in polygon_paths(feature, bounds):
            body.append(f'<path d="{path}" fill="{colors[winner]}" stroke="#ffffff" stroke-width="0.28"/>')
    legend_y = 822
    for index, key in enumerate(["walking", "cycling", "car", "tie"]):
        x = 58 + index * 170
        body.append(f'<rect x="{x}" y="{legend_y}" width="22" height="22" fill="{colors[key]}"/>')
        body.append(f'<text x="{x + 32}" y="{legend_y + 17}" font-size="18">{escape(key)}</text>')
    return svg_page("Best Mode By Strict Score", body)


def render_advantage_map(
    features_by_mode: dict[str, list[dict]],
    bounds: tuple[float, float, float, float],
) -> str:
    by_origin = score_features_by_origin(features_by_mode)
    body = []
    for _origin_id, mode_features in by_origin.items():
        car = mode_features.get("car")
        cycling = mode_features.get("cycling")
        if car is None or cycling is None:
            continue
        car_score = float(car.get("properties", {}).get("strict_score", 0.0) or 0.0)
        cycling_score = float(cycling.get("properties", {}).get("strict_score", 0.0) or 0.0)
        advantage = car_score - cycling_score
        color = diverging_color(advantage, -1.0, 1.0)
        for path in polygon_paths(cycling, bounds):
            body.append(f'<path d="{path}" fill="{color}" stroke="#ffffff" stroke-width="0.28"/>')
    body.extend(
        [
            '<rect x="58" y="838" width="260" height="20" fill="url(#advantageGradient)"/>',
            '<text x="58" y="878" font-size="16">cycling</text><text x="235" y="878" font-size="16">car</text>',
        ]
    )
    return svg_page(
        "Car Advantage Over Cycling",
        body,
        defs='<linearGradient id="advantageGradient"><stop offset="0%" stop-color="#1b9e77"/><stop offset="50%" stop-color="#f7f7f4"/><stop offset="100%" stop-color="#d95f02"/></linearGradient>',
    )


def render_missing_service_map(features: list[dict], bounds: tuple[float, float, float, float]) -> str:
    body = []
    for feature in features:
        properties = feature.get("properties", {})
        reached = int(properties.get("baseline_reached_categories", 0) or 0)
        required = int(properties.get("baseline_required_categories", 6) or 6)
        missing = max(0, required - reached)
        color = missing_color(missing, required)
        for path in polygon_paths(feature, bounds):
            body.append(f'<path d="{path}" fill="{color}" stroke="#ffffff" stroke-width="0.28"/>')
    body.extend(
        [
            '<rect x="58" y="838" width="260" height="20" fill="url(#missingGradient)"/>',
            '<text x="58" y="878" font-size="16">0 missing</text><text x="250" y="878" font-size="16">6 missing</text>',
        ]
    )
    return svg_page(
        "Cycling Missing Service Adequacy",
        body,
        defs='<linearGradient id="missingGradient"><stop offset="0%" stop-color="#1a9850"/><stop offset="50%" stop-color="#fee08b"/><stop offset="100%" stop-color="#d73027"/></linearGradient>',
    )


def render_index_map(
    features_by_mode: dict[str, list[dict]],
    bounds: tuple[float, float, float, float],
) -> str:
    by_origin = score_features_by_origin(features_by_mode)
    body = []
    for _origin_id, mode_features in by_origin.items():
        scores = [
            float(feature.get("properties", {}).get("strict_score", 0.0) or 0.0)
            for feature in mode_features.values()
        ]
        if not scores:
            continue
        index = sum(scores) / len(scores)
        feature = mode_features.get("cycling") or next(iter(mode_features.values()))
        for path in polygon_paths(feature, bounds):
            body.append(f'<path d="{path}" fill="{score_color(index)}" stroke="#ffffff" stroke-width="0.28"/>')
    body.extend(
        [
            '<rect x="58" y="838" width="260" height="20" fill="url(#scoreGradient)"/>',
            '<text x="58" y="878" font-size="16">0</text><text x="300" y="878" font-size="16">1</text>',
        ]
    )
    return svg_page(
        "Synthetic 15-Minute City Index",
        body,
        defs='<linearGradient id="scoreGradient"><stop offset="0%" stop-color="#d73027"/><stop offset="50%" stop-color="#fee08b"/><stop offset="100%" stop-color="#1a9850"/></linearGradient>',
    )


def render_category_small_multiples(gis_dir: Path, bounds: tuple[float, float, float, float]) -> str:
    panel_width = 360
    panel_height = 250
    panel_left = 52
    panel_top = 92
    body = []
    for index, category in enumerate(BASELINE_CATEGORIES):
        path = gis_dir / f"cycling_{category}_nearest_h3.geojson"
        features = []
        if path.exists():
            with path.open() as handle:
                features = json.load(handle).get("features", [])
        col = index % 3
        row = index // 3
        x0 = panel_left + col * 380
        y0 = panel_top + row * 330
        body.append(f'<text x="{x0}" y="{y0 - 16}" font-size="20" font-weight="700">{escape(category)}</text>')
        body.append(f'<rect x="{x0}" y="{y0}" width="{panel_width}" height="{panel_height}" fill="#ecebe6" stroke="#c7c4b8"/>')
        for feature in features:
            value = parse_float(feature.get("properties", {}).get("nearest_travel_time_s"))
            color = time_color(value)
            for item in polygon_paths_to_box(feature, bounds, x0 + 8, y0 + 8, panel_width - 16, panel_height - 16):
                body.append(f'<path d="{item}" fill="{color}" stroke="#ffffff" stroke-width="0.16"/>')
    body.extend(
        [
            '<rect x="58" y="812" width="240" height="18" fill="url(#timeGradient)"/>',
            '<text x="58" y="852" font-size="15">0 min</text><text x="248" y="852" font-size="15">20+ min</text>',
        ]
    )
    return svg_page(
        "Cycling Nearest-Service Time By Category",
        body,
        defs='<linearGradient id="timeGradient"><stop offset="0%" stop-color="#1a9850"/><stop offset="50%" stop-color="#fee08b"/><stop offset="75%" stop-color="#f46d43"/><stop offset="100%" stop-color="#8b8b8b"/></linearGradient>',
    )


def render_route_quality_map(path: Path) -> str:
    if not path.exists():
        return svg_page("Cycling Route Quality Routes", ['<text x="58" y="110" font-size="18">No route GeoJSON found.</text>'])
    with path.open() as handle:
        features = json.load(handle).get("features", [])
    bounds = geometry_bounds(features) if features else (6.45, 53.1, 6.75, 53.3)
    body = []
    for feature in features:
        value = parse_float(feature.get("properties", {}).get("total_travel_time_s"))
        color = time_color(value)
        width = 1.2
        for path_d in line_paths(feature, bounds):
            body.append(f'<path d="{path_d}" fill="none" stroke="{color}" stroke-width="{width}" stroke-opacity="0.62"/>')
    body.extend(
        [
            '<rect x="58" y="838" width="240" height="18" fill="url(#timeGradient)"/>',
            '<text x="58" y="878" font-size="15">short</text><text x="252" y="878" font-size="15">long</text>',
        ]
    )
    return svg_page(
        "Cycling Route Quality Sample Routes",
        body,
        defs='<linearGradient id="timeGradient"><stop offset="0%" stop-color="#1a9850"/><stop offset="50%" stop-color="#fee08b"/><stop offset="75%" stop-color="#f46d43"/><stop offset="100%" stop-color="#8b8b8b"/></linearGradient>',
    )


def line_paths(feature: dict, bounds: tuple[float, float, float, float]) -> list[str]:
    geometry = feature.get("geometry", {})
    lines = []
    if geometry.get("type") == "LineString":
        lines = [geometry.get("coordinates", [])]
    elif geometry.get("type") == "MultiLineString":
        lines = geometry.get("coordinates", [])
    paths = []
    for line in lines:
        projected = [project(lon, lat, bounds) for lon, lat in line]
        if not projected:
            continue
        first = projected[0]
        commands = [f"M {first[0]:.1f} {first[1]:.1f}"]
        commands.extend(f"L {x:.1f} {y:.1f}" for x, y in projected[1:])
        paths.append(" ".join(commands))
    return paths


def score_features_by_origin(features_by_mode: dict[str, list[dict]]) -> dict[str, dict[str, dict]]:
    by_origin: dict[str, dict[str, dict]] = {}
    for mode, features in features_by_mode.items():
        for feature in features:
            origin_id = feature.get("properties", {}).get("id")
            if origin_id:
                by_origin.setdefault(origin_id, {})[mode] = feature
    return by_origin


def score_color(score: float) -> str:
    score = max(0.0, min(1.0, score))
    if score < 0.5:
        return interpolate("#d73027", "#fee08b", score / 0.5)
    return interpolate("#fee08b", "#1a9850", (score - 0.5) / 0.5)


def diverging_color(value: float, min_value: float, max_value: float) -> str:
    midpoint = (min_value + max_value) / 2
    if value <= midpoint:
        return interpolate("#1b9e77", "#f7f7f4", (value - min_value) / (midpoint - min_value))
    return interpolate("#f7f7f4", "#d95f02", (value - midpoint) / (max_value - midpoint))


def missing_color(missing: int, required: int) -> str:
    fraction = 0.0 if required == 0 else missing / required
    if fraction < 0.5:
        return interpolate("#1a9850", "#fee08b", fraction / 0.5)
    return interpolate("#fee08b", "#d73027", (fraction - 0.5) / 0.5)


def time_color(value: float | None) -> str:
    if value is None:
        return "#8b8b8b"
    if value <= 600:
        return interpolate("#1a9850", "#fee08b", value / 600)
    if value <= 1200:
        return interpolate("#fee08b", "#f46d43", (value - 600) / 600)
    return "#8b8b8b"


def interpolate(left: str, right: str, t: float) -> str:
    t = max(0.0, min(1.0, t))
    l = tuple(int(left[i : i + 2], 16) for i in (1, 3, 5))
    r = tuple(int(right[i : i + 2], 16) for i in (1, 3, 5))
    values = tuple(round(a + (b - a) * t) for a, b in zip(l, r))
    return "#{:02x}{:02x}{:02x}".format(*values)


def polygon_paths_to_box(
    feature: dict,
    bounds: tuple[float, float, float, float],
    left: float,
    top: float,
    width: float,
    height: float,
) -> list[str]:
    geometry = feature.get("geometry", {})
    polygons = []
    if geometry.get("type") == "Polygon":
        polygons = [geometry.get("coordinates", [])]
    elif geometry.get("type") == "MultiPolygon":
        polygons = geometry.get("coordinates", [])
    paths = []
    for polygon in polygons:
        parts = []
        for ring in polygon:
            projected = [project_to_box(lon, lat, bounds, left, top, width, height) for lon, lat in ring]
            if not projected:
                continue
            first = projected[0]
            commands = [f"M {first[0]:.1f} {first[1]:.1f}"]
            commands.extend(f"L {x:.1f} {y:.1f}" for x, y in projected[1:])
            commands.append("Z")
            parts.append(" ".join(commands))
        if parts:
            paths.append(" ".join(parts))
    return paths


def project_to_box(
    lon: float,
    lat: float,
    bounds: tuple[float, float, float, float],
    left: float,
    top: float,
    width: float,
    height: float,
) -> tuple[float, float]:
    min_lon, min_lat, max_lon, max_lat = bounds
    x = left + (lon - min_lon) / (max_lon - min_lon) * width
    y = top + height - (lat - min_lat) / (max_lat - min_lat) * height
    return x, y


def render_category_time_plot(mode: str, category_stats: list[dict]) -> str:
    rows = [
        row
        for row in category_stats
        if row.get("mode") == mode and row.get("median_nearest_travel_time_s")
    ]
    rows.sort(key=lambda row: float(row["median_nearest_travel_time_s"]), reverse=True)
    values = [(row["category"], float(row["median_nearest_travel_time_s"])) for row in rows]
    return render_bar_chart(f"{mode.title()} Median Nearest Times", values, "seconds", "#4c78a8")


def render_bottleneck_plot(mode: str, bottlenecks: list[dict[str, str]]) -> str:
    rows = [row for row in bottlenecks if row.get("mode") == mode]
    rows.sort(key=lambda row: int(row.get("missing_origin_count", "0")), reverse=True)
    values = [(row["category"], float(row["missing_origin_count"])) for row in rows]
    return render_bar_chart(f"{mode.title()} Missing Baseline Categories", values, "origins", "#d95f02")


def render_route_quality_plot(rows: list[dict]) -> str:
    values = [
        (row["category"], float(row["median_travel_time_s"]))
        for row in rows
        if row.get("median_travel_time_s")
    ]
    values.sort(key=lambda item: item[1], reverse=True)
    return render_bar_chart("Cycling Route Quality Median Times", values, "seconds", "#5f7f3f")


def render_transit_window_plot(rows: list[dict]) -> str:
    values = [
        (row["window"], float(row["median_total_travel_time_s"]))
        for row in rows
        if row.get("median_total_travel_time_s") is not None
    ]
    values.sort(key=lambda item: item[0])
    return render_bar_chart("Transit Median Time To Zernike", values, "seconds", "#6a51a3")


def render_transit_bundle_plot(rows: list[dict]) -> str:
    values = [
        (row["window"], float(row["strict_complete_share"]))
        for row in rows
        if float(row.get("threshold_s", 0)) == 900.0
    ]
    values.sort(key=lambda item: item[0])
    return render_bar_chart("Transit 15-Minute Service Bundle Sample", values, "share", "#6a51a3")


def render_threshold_plot(rows: list[dict], field: str, title: str) -> str:
    by_mode: dict[str, list[tuple[float, float]]] = {}
    for row in rows:
        by_mode.setdefault(row["mode"], []).append((float(row["threshold_s"]), float(row[field])))
    colors = {"walking": "#7570b3", "cycling": "#1b9e77", "car": "#d95f02"}
    thresholds = sorted({threshold for values in by_mode.values() for threshold, _share in values}) or [600, 720, 900, 1200]
    min_threshold = min(thresholds)
    max_threshold = max(thresholds)
    span = max(max_threshold - min_threshold, 1.0)
    body = chart_axes(title, "threshold seconds", "share", thresholds)
    plot_left, plot_top, plot_width, plot_height = 150, 95, 870, 520
    for mode, values in sorted(by_mode.items()):
        values.sort()
        points = []
        for threshold, share in values:
            x = plot_left + (threshold - min_threshold) / span * plot_width
            y = plot_top + (1 - share) * plot_height
            points.append((x, y))
        color = colors.get(mode, "#333333")
        if points:
            body.append(
                '<polyline fill="none" stroke="{color}" stroke-width="5" points="{points}"/>'.format(
                    color=color,
                    points=" ".join(f"{x:.1f},{y:.1f}" for x, y in points),
                )
            )
            for x, y in points:
                body.append(f'<circle cx="{x:.1f}" cy="{y:.1f}" r="7" fill="{color}"/>')
    legend_y = 690
    for index, mode in enumerate(["walking", "cycling", "car"]):
        x = 150 + index * 170
        body.append(f'<rect x="{x}" y="{legend_y}" width="22" height="22" fill="{colors[mode]}"/>')
        body.append(f'<text x="{x + 32}" y="{legend_y + 17}" font-size="18">{escape(mode)}</text>')
    return svg_page(title, body, width=1100, height=760)


def render_scenario_band_plot(rows: list[dict]) -> str:
    selected = [
        row
        for row in rows
        if row.get("bundle") == "essential_services" and row.get("scenario_family") in {"threshold", "cycling_speed", "snap_acceptance"}
    ]
    selected.sort(key=lambda row: (row.get("scenario_family", ""), row.get("mode", "")))
    values = [
        (
            f"{row.get('scenario_family', '')} {row.get('mode', '')}",
            max(0.0, float(row.get("max_strict_complete_share", 0.0)) - float(row.get("min_strict_complete_share", 0.0))),
        )
        for row in selected
    ]
    return render_bar_chart("Weighted Strict Score Sensitivity Band", values, "share spread", "#386cb0")


def chart_axes(title: str, x_label: str, y_label: str, thresholds: list[float]) -> list[str]:
    left, top, width, height = 150, 95, 870, 520
    body = [
        f'<line x1="{left}" y1="{top + height}" x2="{left + width}" y2="{top + height}" stroke="#333" stroke-width="2"/>',
        f'<line x1="{left}" y1="{top}" x2="{left}" y2="{top + height}" stroke="#333" stroke-width="2"/>',
        f'<text x="{left + width / 2}" y="{top + height + 66}" text-anchor="middle" font-size="18">{escape(x_label)}</text>',
        f'<text x="42" y="{top + height / 2}" transform="rotate(-90 42 {top + height / 2})" text-anchor="middle" font-size="18">{escape(y_label)}</text>',
    ]
    min_threshold = min(thresholds)
    max_threshold = max(thresholds)
    span = max(max_threshold - min_threshold, 1.0)
    for threshold in thresholds:
        x = left + (threshold - min_threshold) / span * width
        body.append(f'<line x1="{x:.1f}" y1="{top + height}" x2="{x:.1f}" y2="{top + height + 8}" stroke="#333"/>')
        body.append(f'<text x="{x:.1f}" y="{top + height + 30}" text-anchor="middle" font-size="15">{threshold}</text>')
    for share in [0, 0.25, 0.5, 0.75, 1.0]:
        y = top + (1 - share) * height
        body.append(f'<line x1="{left - 8}" y1="{y:.1f}" x2="{left}" y2="{y:.1f}" stroke="#333"/>')
        body.append(f'<text x="{left - 16}" y="{y + 5:.1f}" text-anchor="end" font-size="15">{share:.2f}</text>')
        body.append(f'<line x1="{left}" y1="{y:.1f}" x2="{left + width}" y2="{y:.1f}" stroke="#e5e2dc" stroke-width="1"/>')
    return body


def render_bar_chart(title: str, values: list[tuple[str, float]], unit: str, color: str) -> str:
    chart_width = 620 if "share" in unit else 760
    bar_height = 34
    gap = 16
    top = 108
    left = 280
    max_value = max([value for _, value in values] or [1.0])
    height = max(420, top + len(values) * (bar_height + gap) + 80)
    body = []
    for index, (label, value) in enumerate(values):
        y = top + index * (bar_height + gap)
        width = 0 if max_value == 0 else value / max_value * chart_width
        value_label = f"{value:.3f}" if "share" in unit else f"{value:.0f}"
        body.append(f'<text x="{left - 16}" y="{y + 23}" text-anchor="end" font-size="18">{escape(label)}</text>')
        body.append(f'<rect x="{left}" y="{y}" width="{width:.1f}" height="{bar_height}" fill="{color}"/>')
        body.append(f'<text x="{left + width + 12:.1f}" y="{y + 23}" font-size="16">{value_label} {unit}</text>')
    return svg_page(title, body, width=1180, height=height)


def svg_page(title: str, body: list[str], defs: str = "", width: int = WIDTH, height: int = HEIGHT) -> str:
    return "\n".join(
        [
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">',
            f"<defs>{defs}</defs>" if defs else "<defs/>",
            '<rect width="100%" height="100%" fill="#f7f7f4"/>',
            f'<text x="58" y="48" font-size="30" font-family="Arial, sans-serif" font-weight="700">{escape(title)}</text>',
            '<g font-family="Arial, sans-serif">',
            *body,
            "</g>",
            "</svg>",
            "",
        ]
    )


def escape(value: str) -> str:
    return value.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def parse_float(value: object) -> float | None:
    if value in {None, ""}:
        return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


if __name__ == "__main__":
    main()
