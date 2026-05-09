#!/usr/bin/env python3
"""Export map-ready GIS layers and QGIS styles from accessibility scores."""

from __future__ import annotations

import argparse
import csv
import json
from datetime import datetime, timezone
from pathlib import Path


STUDY_DIR = Path(__file__).resolve().parents[1]
SCORES = STUDY_DIR / "outputs/maps/accessibility_scores_h3.geojson"
H3 = STUDY_DIR / "outputs/processed/residential_h3_r9.geojson"
NEAREST = STUDY_DIR / "outputs/processed/accessibility_nearest_services.csv"
OUT_DIR = STUDY_DIR / "outputs/maps/gis"
META_DIR = STUDY_DIR / "metadata/generated"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--scores", type=Path, default=SCORES)
    parser.add_argument("--h3", type=Path, default=H3)
    parser.add_argument("--nearest", type=Path, default=NEAREST)
    parser.add_argument("--out-dir", type=Path, default=OUT_DIR)
    parser.add_argument("--manifest", type=Path, default=META_DIR / "map_layer_manifest.json")
    args = parser.parse_args()

    args.out_dir.mkdir(parents=True, exist_ok=True)
    args.manifest.parent.mkdir(parents=True, exist_ok=True)

    with args.scores.open() as handle:
        score_collection = json.load(handle)
    with args.h3.open() as handle:
        h3_collection = json.load(handle)
    nearest_rows = read_rows(args.nearest)

    grouped: dict[str, list[dict]] = {}
    for feature in score_collection.get("features", []):
        mode = feature.get("properties", {}).get("mode")
        if mode:
            grouped.setdefault(mode, []).append(feature)

    layers = []
    for mode, features in sorted(grouped.items()):
        layer_path = args.out_dir / f"{mode}_accessibility_scores_h3.geojson"
        write_feature_collection(layer_path, features)
        layers.append(
            {
                "mode": mode,
                "path": str(layer_path),
                "feature_count": len(features),
                "styles": [
                    str(args.out_dir / "qgis_strict_score_graduated.qml"),
                    str(args.out_dir / "qgis_complete_15m_categorized.qml"),
                ],
            }
        )

    h3_by_id = {
        feature.get("properties", {}).get("id"): feature
        for feature in h3_collection.get("features", [])
        if feature.get("properties", {}).get("id")
    }
    for mode, category, features in nearest_category_layers(h3_by_id, nearest_rows):
        layer_path = args.out_dir / f"{mode}_{category}_nearest_h3.geojson"
        write_feature_collection(layer_path, features)
        layers.append(
            {
                "mode": mode,
                "category": category,
                "path": str(layer_path),
                "feature_count": len(features),
                "styles": [
                    str(args.out_dir / "qgis_nearest_time_graduated.qml"),
                    str(args.out_dir / "qgis_reachable_15m_categorized.qml"),
                ],
            }
        )

    (args.out_dir / "qgis_strict_score_graduated.qml").write_text(strict_score_qml())
    (args.out_dir / "qgis_complete_15m_categorized.qml").write_text(complete_qml())
    (args.out_dir / "qgis_nearest_time_graduated.qml").write_text(nearest_time_qml())
    (args.out_dir / "qgis_reachable_15m_categorized.qml").write_text(reachable_qml())
    args.manifest.write_text(
        json.dumps(
            {
                "generated_at": datetime.now(timezone.utc).isoformat(),
                "source": str(args.scores),
                "layers": layers,
            },
            indent=2,
        )
        + "\n"
    )


def write_feature_collection(path: Path, features: list[dict]) -> None:
    path.write_text(
        json.dumps(
            {
                "type": "FeatureCollection",
                "features": features,
            },
            separators=(",", ":"),
        )
        + "\n"
    )


def read_rows(path: Path) -> list[dict[str, str]]:
    with path.open(newline="") as handle:
        return list(csv.DictReader(handle))


def nearest_category_layers(
    h3_by_id: dict[str, dict],
    rows: list[dict[str, str]],
) -> list[tuple[str, str, list[dict]]]:
    grouped: dict[tuple[str, str], list[dict]] = {}
    for row in rows:
        origin_id = row.get("origin_id", "")
        base_feature = h3_by_id.get(origin_id)
        if base_feature is None:
            continue
        feature = {
            "type": "Feature",
            "geometry": base_feature.get("geometry"),
            "properties": {
                **base_feature.get("properties", {}),
                "mode": row.get("mode", ""),
                "category": row.get("category", ""),
                "nearest_destination_id": row.get("nearest_destination_id", ""),
                "nearest_travel_time_s": parse_float(row.get("nearest_travel_time_s")),
                "reachable_5m": row.get("reachable_within_300s") == "true",
                "reachable_10m": row.get("reachable_within_600s") == "true",
                "reachable_15m": row.get("reachable_within_900s") == "true",
                "reachable_20m": row.get("reachable_within_1200s") == "true",
                "destination_count_within_15m": parse_int(
                    row.get("destination_count_within_900s")
                ),
            },
        }
        grouped.setdefault((row.get("mode", ""), row.get("category", "")), []).append(feature)
    return [(mode, category, features) for (mode, category), features in sorted(grouped.items())]


def parse_float(value: str | None) -> float | None:
    if value in {None, ""}:
        return None
    return float(value)


def parse_int(value: str | None) -> int:
    if value in {None, ""}:
        return 0
    return int(float(value))


def strict_score_qml() -> str:
    return """<!DOCTYPE qgis PUBLIC 'http://mrcc.com/qgis.dtd' 'SYSTEM'>
<qgis version="3.34" styleCategories="Symbology">
  <renderer-v2 type="graduatedSymbol" attr="strict_score" graduatedMethod="GraduatedColor">
    <ranges>
      <range lower="0" upper="0.333333" symbol="0" label="0.00 - 0.33"/>
      <range lower="0.333333" upper="0.666667" symbol="1" label="0.33 - 0.67"/>
      <range lower="0.666667" upper="0.999999" symbol="2" label="0.67 - 1.00"/>
      <range lower="1" upper="1" symbol="3" label="complete"/>
    </ranges>
    <symbols>
      <symbol name="0" type="fill"><layer class="SimpleFill"><Option name="color" value="215,48,39,255"/><Option name="outline_color" value="255,255,255,160"/><Option name="outline_width" value="0.08"/></layer></symbol>
      <symbol name="1" type="fill"><layer class="SimpleFill"><Option name="color" value="254,224,139,255"/><Option name="outline_color" value="255,255,255,160"/><Option name="outline_width" value="0.08"/></layer></symbol>
      <symbol name="2" type="fill"><layer class="SimpleFill"><Option name="color" value="166,217,106,255"/><Option name="outline_color" value="255,255,255,160"/><Option name="outline_width" value="0.08"/></layer></symbol>
      <symbol name="3" type="fill"><layer class="SimpleFill"><Option name="color" value="26,152,80,255"/><Option name="outline_color" value="255,255,255,180"/><Option name="outline_width" value="0.08"/></layer></symbol>
    </symbols>
  </renderer-v2>
</qgis>
"""


def complete_qml() -> str:
    return """<!DOCTYPE qgis PUBLIC 'http://mrcc.com/qgis.dtd' 'SYSTEM'>
<qgis version="3.34" styleCategories="Symbology">
  <renderer-v2 type="categorizedSymbol" attr="complete_15m">
    <categories>
      <category value="true" symbol="0" label="complete"/>
      <category value="false" symbol="1" label="incomplete"/>
    </categories>
    <symbols>
      <symbol name="0" type="fill"><layer class="SimpleFill"><Option name="color" value="26,152,80,255"/><Option name="outline_color" value="255,255,255,180"/><Option name="outline_width" value="0.08"/></layer></symbol>
      <symbol name="1" type="fill"><layer class="SimpleFill"><Option name="color" value="215,48,39,255"/><Option name="outline_color" value="255,255,255,160"/><Option name="outline_width" value="0.08"/></layer></symbol>
    </symbols>
  </renderer-v2>
</qgis>
"""


def nearest_time_qml() -> str:
    return """<!DOCTYPE qgis PUBLIC 'http://mrcc.com/qgis.dtd' 'SYSTEM'>
<qgis version="3.34" styleCategories="Symbology">
  <renderer-v2 type="graduatedSymbol" attr="nearest_travel_time_s" graduatedMethod="GraduatedColor">
    <ranges>
      <range lower="0" upper="300" symbol="0" label="0 - 5 min"/>
      <range lower="300" upper="600" symbol="1" label="5 - 10 min"/>
      <range lower="600" upper="900" symbol="2" label="10 - 15 min"/>
      <range lower="900" upper="1200" symbol="3" label="15 - 20 min"/>
    </ranges>
    <symbols>
      <symbol name="0" type="fill"><layer class="SimpleFill"><Option name="color" value="26,152,80,255"/><Option name="outline_color" value="255,255,255,170"/><Option name="outline_width" value="0.08"/></layer></symbol>
      <symbol name="1" type="fill"><layer class="SimpleFill"><Option name="color" value="166,217,106,255"/><Option name="outline_color" value="255,255,255,170"/><Option name="outline_width" value="0.08"/></layer></symbol>
      <symbol name="2" type="fill"><layer class="SimpleFill"><Option name="color" value="254,224,139,255"/><Option name="outline_color" value="255,255,255,160"/><Option name="outline_width" value="0.08"/></layer></symbol>
      <symbol name="3" type="fill"><layer class="SimpleFill"><Option name="color" value="215,48,39,255"/><Option name="outline_color" value="255,255,255,160"/><Option name="outline_width" value="0.08"/></layer></symbol>
    </symbols>
  </renderer-v2>
</qgis>
"""


def reachable_qml() -> str:
    return """<!DOCTYPE qgis PUBLIC 'http://mrcc.com/qgis.dtd' 'SYSTEM'>
<qgis version="3.34" styleCategories="Symbology">
  <renderer-v2 type="categorizedSymbol" attr="reachable_15m">
    <categories>
      <category value="true" symbol="0" label="reachable within 15 min"/>
      <category value="false" symbol="1" label="not reachable within 15 min"/>
    </categories>
    <symbols>
      <symbol name="0" type="fill"><layer class="SimpleFill"><Option name="color" value="26,152,80,255"/><Option name="outline_color" value="255,255,255,180"/><Option name="outline_width" value="0.08"/></layer></symbol>
      <symbol name="1" type="fill"><layer class="SimpleFill"><Option name="color" value="215,48,39,255"/><Option name="outline_color" value="255,255,255,160"/><Option name="outline_width" value="0.08"/></layer></symbol>
    </symbols>
  </renderer-v2>
</qgis>
"""


if __name__ == "__main__":
    main()
