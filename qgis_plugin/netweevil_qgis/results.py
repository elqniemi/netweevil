"""Response-to-layer conversion, styling, grouping, and diagnostics logging."""

import json
import math
import re
from itertools import islice
from pathlib import Path

from qgis.core import (
    QgsCategorizedSymbolRenderer,
    QgsDistanceArea,
    QgsFillSymbol,
    QgsLineSymbol,
    QgsMarkerSymbol,
    QgsPointXY,
    QgsProject,
    QgsRendererCategory,
    QgsVectorLayer,
    QgsWkbTypes,
)

from .compat import (
    GEOM_LINE,
    GEOM_POINT,
    MSG_CRITICAL,
    MSG_WARNING,
    vector_temporal_mode_instant,
)

SERVICE_AREA_DIRECT_LOAD_BYTES = 50 * 1024 * 1024
TEMPORAL_FIELD_CANDIDATES = (
    "departure_time",
    "frame_datetime",
    "datetime",
    "frame_time",
    "timestamp",
    "entry_time",
)


class ResultsMixin:
    def execute_api_request(
        self,
        endpoint,
        payload,
        output_path,
        layer_name,
        analysis_kind,
        response_format_override=None,
    ):
        url = self.service_url(
            endpoint,
            include_format=True,
            response_format=response_format_override,
        )
        self.log("POST {}".format(url))
        local_output_path = self.resolve_local_path(output_path)
        try:
            content_type, saved_path = self.http_post_json_to_file(
                url, payload, local_output_path
            )
        except Exception as exc:
            self.alert("API request failed: {}".format(exc))
            return False

        self.log("Saved API response to {}".format(saved_path))

        if "geo+json" in content_type or saved_path.suffix.lower() == ".geojson":
            if self.should_direct_load_service_area(analysis_kind, saved_path):
                loaded_layer = self.load_output_layer(saved_path, layer_name)
                if loaded_layer is not None:
                    self.set_last_output_layers([loaded_layer])
                return True

            try:
                geojson = json.loads(saved_path.read_text(encoding="utf-8"))
            except Exception as exc:
                self.log(
                    "Failed to parse GeoJSON response for logging; loading the raw layer instead: {}".format(
                        exc
                    ),
                    MSG_WARNING,
                )
                loaded_layer = self.load_output_layer(saved_path, layer_name)
                if loaded_layer is not None:
                    self.set_last_output_layers([loaded_layer])
                return True

            self.log_geojson_messages(analysis_kind, geojson)
            if analysis_kind == "service_area":
                self.load_service_area_layers(geojson, layer_name)
                return True

            loaded_layer = self.load_output_layer(saved_path, layer_name)
            if loaded_layer is not None:
                self.set_last_output_layers([loaded_layer])
            return True

        if self.should_skip_large_service_area_parse(analysis_kind, saved_path):
            return True

        try:
            response_json = json.loads(saved_path.read_text(encoding="utf-8"))
        except Exception as exc:
            self.alert("Failed to parse API JSON response: {}".format(exc))
            return False

        self.log_analysis_messages(analysis_kind, response_json)

        if analysis_kind == "route":
            self.load_route_layers(response_json, layer_name)
            return True

        if analysis_kind == "transit_route":
            self.load_transit_route_layers(response_json, layer_name)
            return True

        geojson = self.analysis_json_to_geojson(analysis_kind, response_json)
        if geojson is None:
            self.log(
                "Response saved, but no spatial geometry could be built from the API result.",
                MSG_WARNING,
            )
            return True

        if analysis_kind == "service_area":
            self.load_service_area_layers(geojson, layer_name)
            return True

        temp_path = self.write_temp_geojson(layer_name, geojson)
        loaded_layer = self.load_output_layer(temp_path, layer_name)
        if loaded_layer is not None:
            self.set_last_output_layers([loaded_layer])
        return True

    def should_direct_load_service_area(self, analysis_kind, saved_path):
        if analysis_kind != "service_area":
            return False
        try:
            size_bytes = Path(saved_path).stat().st_size
        except OSError:
            return False
        if size_bytes <= SERVICE_AREA_DIRECT_LOAD_BYTES:
            return False
        self.log(
            "Service-area GeoJSON is {:.1f} MB; loading it directly to avoid duplicating it in memory.".format(
                size_bytes / (1024 * 1024)
            ),
            MSG_WARNING,
        )
        return True

    def should_skip_large_service_area_parse(self, analysis_kind, saved_path):
        if analysis_kind != "service_area":
            return False
        try:
            size_bytes = Path(saved_path).stat().st_size
        except OSError:
            return False
        if size_bytes <= SERVICE_AREA_DIRECT_LOAD_BYTES:
            return False
        self.log(
            "Service-area JSON is {:.1f} MB; saved it but skipped automatic layer conversion to avoid memory pressure. Use GeoJSON output for direct loading of very large service areas.".format(
                size_bytes / (1024 * 1024)
            ),
            MSG_WARNING,
        )
        return True

    def json_text(self, value):
        return json.dumps(value, sort_keys=True) if value not in [None, ""] else ""

    def route_summary_feature_collection(self, service, result):
        summary = result.get("summary") or {}
        geometry = result.get("geometry")
        features = [
            {
                "type": "Feature",
                "geometry": self.item_geometry(geometry),
                "properties": {
                    "dataset_id": service.get("dataset_id"),
                    "profile_id": service.get("profile_id"),
                    "profile_hash": service.get("profile_hash"),
                    "route_id": result.get("route_id"),
                    "route_rank": 0,
                    "alternative_index": None,
                    "outcome": result.get("outcome"),
                    "fallback_used": result.get("fallback_used"),
                    "network_distance_m": summary.get("network_distance_m"),
                    "network_travel_time_s": summary.get("network_travel_time_s"),
                    "network_generalized_cost": summary.get("network_generalized_cost"),
                    "components_json": self.json_text(summary.get("components") or {}),
                    "waiting_time_s": summary.get("waiting_time_s"),
                    "departure_time": summary.get("departure_time"),
                    "arrival_time": summary.get("arrival_time"),
                    "scenario_id": summary.get("scenario_id"),
                    "illegal_movement_penalty_s": summary.get("illegal_movement_penalty_s"),
                    "illegal_movement_penalty_cost": summary.get("illegal_movement_penalty_cost"),
                    "violation_count": summary.get("violation_count"),
                    "violation_types_json": self.json_text(summary.get("violation_types") or []),
                    "total_distance_m": summary.get("total_distance_m"),
                    "total_travel_time_s": summary.get("total_travel_time_s"),
                    "total_generalized_cost": summary.get("total_generalized_cost"),
                    "segment_count": summary.get("segment_count"),
                    "origin_point_id": result.get("origin", {}).get("point_id"),
                    "destination_point_id": result.get("destination", {}).get("point_id"),
                    "origin_component_id": result.get("origin", {}).get("component_id"),
                    "destination_component_id": result.get("destination", {}).get("component_id"),
                    "origin_snap_distance_m": result.get("origin", {}).get("snap_distance_m"),
                    "destination_snap_distance_m": result.get("destination", {}).get("snap_distance_m"),
                    "origin_hop_distance_m": result.get("origin_hop_distance_m"),
                    "destination_hop_distance_m": result.get("destination_hop_distance_m"),
                    "warnings_json": self.json_text(result.get("warnings") or []),
                },
            }
        ]
        for alternative in result.get("alternatives") or []:
            alt_summary = alternative.get("summary") or {}
            features.append(
                {
                    "type": "Feature",
                    "geometry": self.item_geometry(alternative.get("geometry")),
                    "properties": {
                        "dataset_id": service.get("dataset_id"),
                        "profile_id": service.get("profile_id"),
                        "profile_hash": service.get("profile_hash"),
                        "route_id": result.get("route_id"),
                        "route_rank": alternative.get("rank"),
                        "alternative_index": alternative.get("alternative_index"),
                        "total_distance_m": alt_summary.get("total_distance_m"),
                        "total_travel_time_s": alt_summary.get("total_travel_time_s"),
                        "total_generalized_cost": alt_summary.get("total_generalized_cost"),
                        "components_json": self.json_text(
                            alt_summary.get("components") or {}
                        ),
                        "waiting_time_s": alt_summary.get("waiting_time_s"),
                        "departure_time": alt_summary.get("departure_time"),
                        "arrival_time": alt_summary.get("arrival_time"),
                        "scenario_id": alt_summary.get("scenario_id"),
                        "violation_count": alt_summary.get("violation_count"),
                        "violation_types_json": self.json_text(
                            alt_summary.get("violation_types") or []
                        ),
                        "segment_count": alt_summary.get("segment_count"),
                        "warnings_json": self.json_text(alternative.get("warnings") or []),
                    },
                }
            )
        return {
            "type": "FeatureCollection",
            "features": features,
        }

    def route_distance_area(self):
        distance = QgsDistanceArea()
        try:
            distance.setSourceCrs(
                self.wgs84,
                QgsProject.instance().transformContext(),
            )
        except Exception:
            pass
        try:
            distance.setEllipsoid("WGS84")
        except Exception:
            pass
        return distance

    def point_at_distance(self, coordinates, cumulative_lengths, distance_m):
        if distance_m <= 0.0:
            return list(coordinates[0])
        if distance_m >= cumulative_lengths[-1]:
            return list(coordinates[-1])
        for index in range(len(cumulative_lengths) - 1):
            start_distance = cumulative_lengths[index]
            end_distance = cumulative_lengths[index + 1]
            if distance_m <= end_distance + 1.0e-9:
                if end_distance - start_distance <= 1.0e-9:
                    return list(coordinates[index + 1])
                ratio = (distance_m - start_distance) / (end_distance - start_distance)
                start = coordinates[index]
                end = coordinates[index + 1]
                dimensions = min(len(start), len(end))
                return [
                    start[dimension]
                    + (end[dimension] - start[dimension]) * ratio
                    for dimension in range(dimensions)
                ]
        return list(coordinates[-1])

    def dedupe_coordinates(self, coordinates):
        if not coordinates:
            return coordinates
        deduped = [coordinates[0]]
        for coordinate in coordinates[1:]:
            previous = deduped[-1]
            if len(previous) != len(coordinate) or any(
                abs(previous[index] - coordinate[index]) > 1.0e-12
                for index in range(min(len(previous), len(coordinate)))
            ):
                deduped.append(coordinate)
        return deduped

    def slice_route_geometry(self, coordinates, cumulative_lengths, start_m, end_m):
        if not coordinates:
            return None
        if end_m <= start_m + 1.0e-9:
            point = self.point_at_distance(coordinates, cumulative_lengths, start_m)
            return [point, point]
        sliced = [self.point_at_distance(coordinates, cumulative_lengths, start_m)]
        for index in range(1, len(coordinates) - 1):
            distance_m = cumulative_lengths[index]
            if start_m < distance_m < end_m:
                sliced.append(list(coordinates[index]))
        sliced.append(self.point_at_distance(coordinates, cumulative_lengths, end_m))
        return self.dedupe_coordinates(sliced)

    def split_route_geometry_by_distance(self, coordinates, segment_lengths_m):
        if len(coordinates) < 2 or not segment_lengths_m:
            return [None for _ in segment_lengths_m]
        distance = self.route_distance_area()
        cumulative_lengths = [0.0]
        for start, end in zip(coordinates, coordinates[1:]):
            horizontal_distance = distance.measureLine(
                QgsPointXY(start[0], start[1]),
                QgsPointXY(end[0], end[1]),
            )
            segment_distance = horizontal_distance
            if len(start) >= 3 and len(end) >= 3:
                try:
                    start_z = float(start[2])
                    end_z = float(end[2])
                except (TypeError, ValueError):
                    pass
                else:
                    if math.isfinite(start_z) and math.isfinite(end_z):
                        segment_distance = math.hypot(
                            horizontal_distance,
                            end_z - start_z,
                        )
            cumulative_lengths.append(cumulative_lengths[-1] + max(segment_distance, 0.0))
        total_geometry_length = cumulative_lengths[-1]
        total_segment_length = sum(max(float(length), 0.0) for length in segment_lengths_m)
        if total_geometry_length <= 0.0 or total_segment_length <= 0.0:
            return [None for _ in segment_lengths_m]
        scale = total_geometry_length / total_segment_length
        segment_geometries = []
        start_m = 0.0
        for index, length_m in enumerate(segment_lengths_m):
            scaled_length = max(float(length_m), 0.0) * scale
            end_m = total_geometry_length if index == len(segment_lengths_m) - 1 else min(
                total_geometry_length, start_m + scaled_length
            )
            segment_geometries.append(
                self.slice_route_geometry(
                    coordinates,
                    cumulative_lengths,
                    start_m,
                    end_m,
                )
            )
            start_m = end_m
        return segment_geometries

    def route_segment_feature_collection(self, service, result):
        segments = result.get("segments") or []
        if not segments:
            return {"type": "FeatureCollection", "features": []}
        route_geometry = result.get("geometry") or []
        segment_geometries = self.split_route_geometry_by_distance(
            route_geometry,
            [segment.get("length_m") or 0 for segment in segments],
        )
        features = []
        for index, segment in enumerate(segments, start=1):
            geometry = None
            if index - 1 < len(segment_geometries):
                geometry = self.item_geometry(segment_geometries[index - 1])
            features.append(
                {
                    "type": "Feature",
                    "geometry": geometry,
                    "properties": {
                        "dataset_id": service.get("dataset_id"),
                        "profile_id": service.get("profile_id"),
                        "profile_hash": service.get("profile_hash"),
                        "route_id": result.get("route_id"),
                        "segment_index": index,
                        "edge_id": segment.get("edge_id"),
                        "from_node_id": segment.get("from_node_id"),
                        "to_node_id": segment.get("to_node_id"),
                        "source_way_id": segment.get("source_way_id"),
                        "length_m": segment.get("length_m"),
                        "travel_time_s": segment.get("travel_time_s"),
                        "generalized_cost": segment.get("generalized_cost"),
                        "components_json": self.json_text(
                            segment.get("components") or {}
                        ),
                        "waiting_time_s": segment.get("waiting_time_s"),
                        "entry_time": segment.get("entry_time"),
                        "exit_time": segment.get("exit_time"),
                        "road_class": segment.get("road_class"),
                        "surface": segment.get("surface"),
                        "name": segment.get("name"),
                        "violation_type": segment.get("violation_type"),
                    },
                }
            )
        return {"type": "FeatureCollection", "features": features}

    def route_hop_feature_collection(self, service, result):
        features = []
        for index, hop in enumerate(result.get("hop_segments") or [], start=1):
            features.append(
                {
                    "type": "Feature",
                    "geometry": self.item_geometry(hop.get("geometry")),
                    "properties": {
                        "dataset_id": service.get("dataset_id"),
                        "profile_id": service.get("profile_id"),
                        "profile_hash": service.get("profile_hash"),
                        "route_id": result.get("route_id"),
                        "hop_index": index,
                        "endpoint": hop.get("endpoint"),
                        "distance_m": hop.get("distance_m"),
                    },
                }
            )
        return {"type": "FeatureCollection", "features": features}

    def route_violation_feature_collection(self, service, result):
        features = []
        for index, violation in enumerate(result.get("violations") or [], start=1):
            features.append(
                {
                    "type": "Feature",
                    "geometry": None,
                    "properties": {
                        "dataset_id": service.get("dataset_id"),
                        "profile_id": service.get("profile_id"),
                        "profile_hash": service.get("profile_hash"),
                        "route_id": result.get("route_id"),
                        "violation_index": index,
                        "violation_type": violation.get("violation_type"),
                        "edge_id": violation.get("edge_id"),
                        "from_edge_id": violation.get("from_edge_id"),
                        "to_edge_id": violation.get("to_edge_id"),
                        "distance_m": violation.get("distance_m"),
                        "penalty_s": violation.get("penalty_s"),
                        "penalty_generalized_cost": violation.get(
                            "penalty_generalized_cost"
                        ),
                    },
                }
            )
        return {"type": "FeatureCollection", "features": features}

    def route_breakdown_feature_collection(
        self,
        service,
        result,
        breakdown_type,
        breakdown_values,
    ):
        features = []
        for category, metrics in sorted((breakdown_values or {}).items()):
            features.append(
                {
                    "type": "Feature",
                    "geometry": None,
                    "properties": {
                        "dataset_id": service.get("dataset_id"),
                        "profile_id": service.get("profile_id"),
                        "profile_hash": service.get("profile_hash"),
                        "route_id": result.get("route_id"),
                        "breakdown_type": breakdown_type,
                        "category": category,
                        "distance_m": (metrics or {}).get("distance_m"),
                        "time_s": (metrics or {}).get("time_s"),
                    },
                }
            )
        return {"type": "FeatureCollection", "features": features}

    def transit_route_coordinates(self, result):
        coordinates = []
        for leg in result.get("legs") or []:
            geometry = leg.get("geometry") or []
            if not geometry:
                continue
            if coordinates and geometry[0] == coordinates[-1]:
                coordinates.extend(geometry[1:])
            else:
                coordinates.extend(geometry)
        return self.dedupe_coordinates(coordinates)

    def transit_summary_feature_collection(self, service, result):
        time_context = result.get("time_context") or {}
        summary = result.get("summary") or {}
        coordinates = self.transit_route_coordinates(result)
        features = [
            {
                "type": "Feature",
                "geometry": self.item_geometry(coordinates),
                "properties": {
                    "feed_id": service.get("feed_id"),
                    "agency_timezone": time_context.get("agency_timezone"),
                    "time_origin_unix_s": time_context.get("time_origin_unix_s"),
                    "service_start_date": service.get("service_start_date"),
                    "service_days": service.get("service_days"),
                    "route_engine": service.get("route_engine"),
                    "route_id": result.get("route_id"),
                    "route_rank": 0,
                    "alternative_index": None,
                    "outcome": result.get("outcome"),
                    "departure_s": summary.get("departure_s"),
                    "arrival_s": summary.get("arrival_s"),
                    "total_travel_time_s": summary.get("total_travel_time_s"),
                    "transit_time_s": summary.get("transit_time_s"),
                    "access_egress_time_s": summary.get("access_egress_time_s"),
                    "transfer_time_s": summary.get("transfer_time_s"),
                    "wait_time_s": summary.get("wait_time_s"),
                    "boarding_count": summary.get("boarding_count"),
                    "diagnostics_json": self.json_text(result.get("diagnostics") or []),
                },
            }
        ]
        for alternative in result.get("alternatives") or []:
            alt_summary = alternative.get("summary") or {}
            features.append(
                {
                    "type": "Feature",
                    "geometry": self.item_geometry(self.transit_route_coordinates(alternative)),
                    "properties": {
                        "feed_id": service.get("feed_id"),
                        "agency_timezone": time_context.get("agency_timezone"),
                        "time_origin_unix_s": time_context.get("time_origin_unix_s"),
                        "service_start_date": service.get("service_start_date"),
                        "service_days": service.get("service_days"),
                        "route_engine": service.get("route_engine"),
                        "route_id": result.get("route_id"),
                        "route_rank": alternative.get("rank"),
                        "alternative_index": alternative.get("alternative_index"),
                        "departure_s": alt_summary.get("departure_s"),
                        "arrival_s": alt_summary.get("arrival_s"),
                        "total_travel_time_s": alt_summary.get("total_travel_time_s"),
                        "transit_time_s": alt_summary.get("transit_time_s"),
                        "access_egress_time_s": alt_summary.get("access_egress_time_s"),
                        "transfer_time_s": alt_summary.get("transfer_time_s"),
                        "wait_time_s": alt_summary.get("wait_time_s"),
                        "boarding_count": alt_summary.get("boarding_count"),
                    },
                }
            )
        return {
            "type": "FeatureCollection",
            "features": features,
        }

    def transit_leg_feature_collection(self, service, result):
        time_context = result.get("time_context") or {}
        features = []
        for index, leg in enumerate(result.get("legs") or [], start=1):
            features.append(
                {
                    "type": "Feature",
                    "geometry": self.item_geometry(leg.get("geometry")),
                    "properties": {
                        "feed_id": service.get("feed_id"),
                        "agency_timezone": time_context.get("agency_timezone"),
                        "time_origin_unix_s": time_context.get("time_origin_unix_s"),
                        "route_id": result.get("route_id"),
                        "leg_index": index,
                        "leg_type": leg.get("leg_type"),
                        "from_id": leg.get("from_id"),
                        "to_id": leg.get("to_id"),
                        "from_name": leg.get("from_name"),
                        "to_name": leg.get("to_name"),
                        "departure_s": leg.get("departure_s"),
                        "arrival_s": leg.get("arrival_s"),
                        "duration_s": (
                            leg.get("arrival_s") - leg.get("departure_s")
                            if leg.get("arrival_s") is not None
                            and leg.get("departure_s") is not None
                            else None
                        ),
                        "mode": leg.get("mode"),
                        "street_mode": leg.get("street_mode"),
                        "gtfs_route_id": leg.get("route_id"),
                        "route_short_name": leg.get("route_short_name"),
                        "trip_id": leg.get("trip_id"),
                        "headsign": leg.get("headsign"),
                    },
                }
            )
        for alternative in result.get("alternatives") or []:
            for index, leg in enumerate(alternative.get("legs") or [], start=1):
                features.append(
                    {
                        "type": "Feature",
                        "geometry": self.item_geometry(leg.get("geometry")),
                        "properties": {
                            "feed_id": service.get("feed_id"),
                            "agency_timezone": time_context.get("agency_timezone"),
                            "time_origin_unix_s": time_context.get("time_origin_unix_s"),
                            "route_id": result.get("route_id"),
                            "route_rank": alternative.get("rank"),
                            "alternative_index": alternative.get("alternative_index"),
                            "leg_index": index,
                            "leg_type": leg.get("leg_type"),
                            "from_id": leg.get("from_id"),
                            "to_id": leg.get("to_id"),
                            "from_name": leg.get("from_name"),
                            "to_name": leg.get("to_name"),
                            "departure_s": leg.get("departure_s"),
                            "arrival_s": leg.get("arrival_s"),
                            "duration_s": (
                                leg.get("arrival_s") - leg.get("departure_s")
                                if leg.get("arrival_s") is not None
                                and leg.get("departure_s") is not None
                                else None
                            ),
                            "mode": leg.get("mode"),
                            "street_mode": leg.get("street_mode"),
                            "gtfs_route_id": leg.get("route_id"),
                            "route_short_name": leg.get("route_short_name"),
                            "trip_id": leg.get("trip_id"),
                            "headsign": leg.get("headsign"),
                        },
                    }
                )
        return {"type": "FeatureCollection", "features": features}

    def transit_stop_feature_collection(self, service, result):
        time_context = result.get("time_context") or {}
        features = []
        for stop in result.get("stops") or []:
            features.append(
                {
                    "type": "Feature",
                    "geometry": {
                        "type": "Point",
                        "coordinates": [stop.get("lon"), stop.get("lat")],
                    },
                    "properties": {
                        "feed_id": service.get("feed_id"),
                        "agency_timezone": time_context.get("agency_timezone"),
                        "time_origin_unix_s": time_context.get("time_origin_unix_s"),
                        "route_id": result.get("route_id"),
                        "sequence": stop.get("sequence"),
                        "stop_id": stop.get("stop_id"),
                        "stop_name": stop.get("stop_name"),
                        "arrival_s": stop.get("arrival_s"),
                        "departure_s": stop.get("departure_s"),
                        "mode": stop.get("mode"),
                        "gtfs_route_id": stop.get("route_id"),
                        "route_short_name": stop.get("route_short_name"),
                        "trip_id": stop.get("trip_id"),
                        "headsign": stop.get("headsign"),
                    },
                }
            )
        return {"type": "FeatureCollection", "features": features}

    def transit_stop_segment_feature_collection(self, service, result):
        time_context = result.get("time_context") or {}
        features = []
        for segment in result.get("stop_segments") or []:
            features.append(
                {
                    "type": "Feature",
                    "geometry": self.item_geometry(segment.get("geometry")),
                    "properties": {
                        "feed_id": service.get("feed_id"),
                        "agency_timezone": time_context.get("agency_timezone"),
                        "time_origin_unix_s": time_context.get("time_origin_unix_s"),
                        "route_id": result.get("route_id"),
                        "segment_index": segment.get("segment_index"),
                        "from_stop_id": segment.get("from_stop_id"),
                        "to_stop_id": segment.get("to_stop_id"),
                        "from_stop_name": segment.get("from_stop_name"),
                        "to_stop_name": segment.get("to_stop_name"),
                        "departure_s": segment.get("departure_s"),
                        "arrival_s": segment.get("arrival_s"),
                        "duration_s": segment.get("duration_s"),
                        "mode": segment.get("mode"),
                        "gtfs_route_id": segment.get("route_id"),
                        "route_short_name": segment.get("route_short_name"),
                        "trip_id": segment.get("trip_id"),
                        "headsign": segment.get("headsign"),
                    },
                }
            )
        return {"type": "FeatureCollection", "features": features}

    def apply_route_line_style(self, layer, color, width, line_style="solid"):
        if QgsWkbTypes.geometryType(layer.wkbType()) != GEOM_LINE:
            return
        symbol = QgsLineSymbol.createSimple(
            {
                "line_color": color,
                "line_width": str(width),
                "line_style": line_style,
            }
        )
        layer.renderer().setSymbol(symbol)
        layer.triggerRepaint()

    def apply_transit_leg_style(self, layer):
        if QgsWkbTypes.geometryType(layer.wkbType()) != GEOM_LINE:
            return
        specs = {
            "access": ("#2b8a3e", "dash"),
            "egress": ("#2b8a3e", "dash"),
            "transfer": ("#d9480f", "dot"),
            "transit": ("#1971c2", "solid"),
        }
        categories = []
        for leg_type, (color, line_style) in specs.items():
            symbol = QgsLineSymbol.createSimple(
                {
                    "line_color": color,
                    "line_width": "1.0" if leg_type == "transit" else "0.8",
                    "line_style": line_style,
                }
            )
            categories.append(QgsRendererCategory(leg_type, symbol, leg_type))
        layer.setRenderer(QgsCategorizedSymbolRenderer("leg_type", categories))
        layer.triggerRepaint()

    def apply_transit_stop_style(self, layer):
        if QgsWkbTypes.geometryType(layer.wkbType()) != GEOM_POINT:
            return
        symbol = QgsMarkerSymbol.createSimple(
            {
                "name": "circle",
                "color": "#ffffff",
                "outline_color": "#1971c2",
                "outline_width": "0.6",
                "size": "2.8",
            }
        )
        layer.renderer().setSymbol(symbol)
        layer.triggerRepaint()

    def create_result_group(self, name):
        root = QgsProject.instance().layerTreeRoot()
        existing_group = root.findGroup(name)
        if existing_group is not None:
            for tree_layer in existing_group.findLayers():
                QgsProject.instance().removeMapLayer(tree_layer.layerId())
            root.removeChildNode(existing_group)
        return root.insertGroup(0, name)

    def load_route_layers(self, response_json, layer_name):
        service = response_json.get("service") or {}
        result = response_json.get("result") or {}
        route_id = result.get("route_id") or layer_name or "netweevil_route"
        group = self.create_result_group(route_id)

        breakdowns = result.get("breakdowns") or {}
        layer_specs = [
            ("route", self.route_summary_feature_collection(service, result), "#0b7285", 1.4, "solid"),
            (
                "segments",
                self.route_segment_feature_collection(service, result),
                "#2b8a3e",
                0.9,
                "solid",
            ),
            (
                "hop_segments",
                self.route_hop_feature_collection(service, result),
                "#d9480f",
                1.1,
                "dash",
            ),
            (
                "violations",
                self.route_violation_feature_collection(service, result),
                None,
                None,
                None,
            ),
            (
                "road_type_breakdown",
                self.route_breakdown_feature_collection(
                    service,
                    result,
                    "road_class",
                    breakdowns.get("road_class"),
                ),
                None,
                None,
                None,
            ),
            (
                "surface_breakdown",
                self.route_breakdown_feature_collection(
                    service,
                    result,
                    "surface",
                    breakdowns.get("surface"),
                ),
                None,
                None,
                None,
            ),
        ]

        loaded_layers = []
        for sublayer_name, geojson, color, width, line_style in layer_specs:
            features = geojson.get("features") or []
            if not features:
                continue
            temp_path = self.write_temp_geojson(
                "{}_{}".format(route_id, sublayer_name),
                geojson,
            )
            layer = QgsVectorLayer(str(temp_path), sublayer_name, "ogr")
            if not layer.isValid():
                self.log("Failed to load layer {}".format(temp_path), MSG_WARNING)
                continue
            if color is not None:
                self.apply_route_line_style(layer, color, width, line_style)
            self.configure_temporal_layer(layer)
            QgsProject.instance().addMapLayer(layer, False)
            group.addLayer(layer)
            loaded_layers.append(layer)
            self.log(
                "Loaded route layer '{}' with {} feature(s).".format(
                    sublayer_name, len(features)
                )
            )

        if loaded_layers:
            self.set_last_output_layers(loaded_layers)
        else:
            self.log("Route response had no loadable layers or tables.", MSG_WARNING)

    def load_transit_route_layers(self, response_json, layer_name):
        service = response_json.get("service") or {}
        result = response_json.get("result") or {}
        route_id = result.get("route_id") or layer_name or "netweevil_transit"
        group = self.create_result_group(route_id)

        layer_specs = [
            (
                "transit_route",
                self.transit_summary_feature_collection(service, result),
                "#0b7285",
                1.6,
                "solid",
            ),
            ("legs", self.transit_leg_feature_collection(service, result), None, None, None),
            (
                "stop_segments",
                self.transit_stop_segment_feature_collection(service, result),
                "#7048e8",
                0.8,
                "solid",
            ),
            (
                "stops",
                self.transit_stop_feature_collection(service, result),
                None,
                None,
                None,
            ),
        ]

        loaded_layers = []
        for sublayer_name, geojson, color, width, line_style in layer_specs:
            features = geojson.get("features") or []
            if not features:
                continue
            temp_path = self.write_temp_geojson(
                "{}_{}".format(route_id, sublayer_name),
                geojson,
            )
            layer = QgsVectorLayer(str(temp_path), sublayer_name, "ogr")
            if not layer.isValid():
                self.log("Failed to load layer {}".format(temp_path), MSG_WARNING)
                continue
            if sublayer_name == "legs":
                self.apply_transit_leg_style(layer)
            elif sublayer_name == "stops":
                self.apply_transit_stop_style(layer)
            elif color is not None:
                self.apply_route_line_style(layer, color, width, line_style)
            QgsProject.instance().addMapLayer(layer, False)
            group.addLayer(layer)
            loaded_layers.append(layer)
            self.log(
                "Loaded transit layer '{}' with {} feature(s).".format(
                    sublayer_name, len(features)
                )
            )

        if loaded_layers:
            self.set_last_output_layers(loaded_layers)
        else:
            self.log("Transit route response had no loadable layers.", MSG_WARNING)

    def analysis_json_to_geojson(self, analysis_kind, response_json):
        service = response_json.get("service", {})
        result = response_json.get("result", {})
        if analysis_kind == "route":
            geometry = result.get("geometry")
            if not geometry:
                return None
            return {
                "type": "FeatureCollection",
                "features": [
                    {
                        "type": "Feature",
                        "geometry": {"type": "LineString", "coordinates": geometry},
                        "properties": {
                            "dataset_id": service.get("dataset_id"),
                            "profile_id": service.get("profile_id"),
                            "profile_hash": service.get("profile_hash"),
                            "route_id": result.get("route_id"),
                            "outcome": result.get("outcome"),
                            "fallback_used": result.get("fallback_used"),
                            "total_distance_m": result.get("summary", {}).get("total_distance_m"),
                            "total_travel_time_s": result.get("summary", {}).get(
                                "total_travel_time_s"
                            ),
                            "total_generalized_cost": result.get("summary", {}).get(
                                "total_generalized_cost"
                            ),
                            "components_json": self.json_text(
                                result.get("summary", {}).get("components") or {}
                            ),
                            "waiting_time_s": result.get("summary", {}).get(
                                "waiting_time_s"
                            ),
                            "departure_time": result.get("summary", {}).get(
                                "departure_time"
                            ),
                            "arrival_time": result.get("summary", {}).get(
                                "arrival_time"
                            ),
                            "scenario_id": result.get("summary", {}).get(
                                "scenario_id"
                            ),
                            "segment_count": result.get("summary", {}).get("segment_count"),
                            "origin_point_id": result.get("origin", {}).get("point_id"),
                            "destination_point_id": result.get("destination", {}).get("point_id"),
                            "origin_component_id": result.get("origin", {}).get("component_id"),
                            "destination_component_id": result.get("destination", {}).get(
                                "component_id"
                            ),
                            "origin_snap_distance_m": result.get("origin", {}).get(
                                "snap_distance_m"
                            ),
                            "destination_snap_distance_m": result.get("destination", {}).get(
                                "snap_distance_m"
                            ),
                            "origin_hop_distance_m": result.get("origin_hop_distance_m"),
                            "destination_hop_distance_m": result.get(
                                "destination_hop_distance_m"
                            ),
                        },
                    }
                ],
            }

        if analysis_kind == "service_area":
            features = []
            for feature in result.get("features") or []:
                features.append(
                    {
                        "type": "Feature",
                        "geometry": feature.get("geometry"),
                        "properties": {
                            "dataset_id": service.get("dataset_id"),
                            "profile_id": service.get("profile_id"),
                            "profile_hash": service.get("profile_hash"),
                            "analysis_id": result.get("analysis_id"),
                            "origin_id": feature.get("origin_id"),
                            "threshold_id": feature.get("threshold_id"),
                            "band_start_limit": feature.get("band_start_limit"),
                            "threshold_limit": feature.get("threshold_limit"),
                            "threshold_metric": feature.get("threshold_metric"),
                            "geometry_type": feature.get("geometry_type"),
                            "fallback_used": feature.get("fallback_used"),
                            "origin_component_id": feature.get("origin_component_id"),
                            "origin_hop_distance_m": feature.get("origin_hop_distance_m"),
                            "reachable_network_length_m": feature.get(
                                "reachable_network_length_m"
                            ),
                            "reachable_edge_count": feature.get("reachable_edge_count"),
                            "departure_time": feature.get("departure_time")
                            or result.get("departure_time"),
                            "scenario_id": feature.get("scenario_id")
                            or result.get("scenario_id"),
                            "edge_id": feature.get("edge_id"),
                            "edge_index": feature.get("edge_index"),
                            "source_way_id": feature.get("source_way_id"),
                            "from_node_id": feature.get("from_node_id"),
                            "to_node_id": feature.get("to_node_id"),
                            "start_fraction": feature.get("start_fraction"),
                            "end_fraction": feature.get("end_fraction"),
                            "start_cost": feature.get("start_cost"),
                            "end_cost": feature.get("end_cost"),
                            "segment_distance_m": feature.get("segment_distance_m"),
                            "segment_travel_time_s": feature.get(
                                "segment_travel_time_s"
                            ),
                        },
                    }
                )
            return {
                "type": "FeatureCollection",
                "features": features,
                "metadata": {
                    "dataset_id": service.get("dataset_id"),
                    "profile_id": service.get("profile_id"),
                    "profile_hash": service.get("profile_hash"),
                    "analysis_id": result.get("analysis_id"),
                    "outcome": result.get("outcome"),
                    "origin_count": result.get("origin_count"),
                    "processed_origin_count": result.get("processed_origin_count"),
                    "skipped_origin_count": result.get("skipped_origin_count"),
                    "fallback_origin_count": result.get("fallback_origin_count"),
                    "threshold_count": result.get("threshold_count"),
                    "departure_time": result.get("departure_time"),
                    "scenario_id": result.get("scenario_id"),
                    "warnings": result.get("warnings") or [],
                },
            }

        items = result.get("pairs") if analysis_kind == "od" else result.get("cells")
        if not isinstance(items, list):
            return None

        features = []
        for item in items:
            features.append(
                {
                    "type": "Feature",
                    "geometry": self.item_geometry(item.get("geometry")),
                    "properties": {
                        "dataset_id": service.get("dataset_id"),
                        "profile_id": service.get("profile_id"),
                        "profile_hash": service.get("profile_hash"),
                        "pair_id": item.get("pair_id"),
                        "origin_id": item.get("origin_id"),
                        "destination_id": item.get("destination_id"),
                        "status": item.get("status"),
                        "outcome": item.get("outcome"),
                        "fallback_used": item.get("fallback_used"),
                        "origin_component_id": item.get("origin_component_id"),
                        "destination_component_id": item.get("destination_component_id"),
                        "origin_hop_distance_m": item.get("origin_hop_distance_m"),
                        "destination_hop_distance_m": item.get("destination_hop_distance_m"),
                        "origin_snap_distance_m": item.get("origin_snap_distance_m"),
                        "destination_snap_distance_m": item.get("destination_snap_distance_m"),
                        "total_distance_m": item.get("total_distance_m"),
                        "total_travel_time_s": item.get("total_travel_time_s"),
                        "total_generalized_cost": item.get("total_generalized_cost"),
                        "components_json": self.json_text(item.get("components") or {}),
                        "error": item.get("error"),
                    },
                }
            )

        return {"type": "FeatureCollection", "features": features}

    def item_geometry(self, coordinates):
        if not coordinates:
            return None
        return {"type": "LineString", "coordinates": coordinates}

    def write_temp_geojson(self, layer_name, geojson):
        safe_name = re.sub(r"[^A-Za-z0-9_.-]+", "_", layer_name).strip("_") or "netweevil_layer"
        path = self.temp_layers_dir / "{}.geojson".format(safe_name)
        path.write_text(json.dumps(geojson, separators=(",", ":")), encoding="utf-8")
        return path

    def load_output_layer(self, output_path, layer_name=None):
        output_path = Path(output_path)
        display_name = layer_name or output_path.stem
        self.remove_existing_result_layer(display_name)
        layer = QgsVectorLayer(str(output_path), display_name, "ogr")
        if not layer.isValid():
            self.log("Failed to load layer {}".format(output_path), MSG_WARNING)
            return None
        QgsProject.instance().addMapLayer(layer, False)
        QgsProject.instance().layerTreeRoot().insertLayer(0, layer)
        self.configure_temporal_layer(layer)
        self.log("Loaded layer {}".format(output_path))
        return layer

    def configure_temporal_layer(self, layer, preferred_field=None):
        """Enable feature-instant animation when an output exposes frame time."""
        if layer is None or not hasattr(layer, "temporalProperties"):
            return False
        field_names = {field.name() for field in layer.fields()}
        candidates = []
        if preferred_field:
            candidates.append(preferred_field)
        candidates.extend(TEMPORAL_FIELD_CANDIDATES)
        sample_features = list(islice(layer.getFeatures(), 32))
        temporal_field = None
        for candidate in candidates:
            if candidate not in field_names:
                continue
            for feature in sample_features:
                value = feature[candidate]
                if value is None or str(value).strip().upper() in ("", "NULL"):
                    continue
                temporal_field = candidate
                break
            if temporal_field is not None:
                break
        if temporal_field is None:
            return False
        try:
            properties = layer.temporalProperties()
            properties.setMode(vector_temporal_mode_instant())
            properties.setStartField(temporal_field)
            properties.setIsActive(True)
        except Exception as exc:
            self.log(
                "Loaded '{}', but automatic temporal setup on '{}' failed: {}".format(
                    layer.name(), temporal_field, exc
                ),
                MSG_WARNING,
            )
            return False
        self.log(
            "Temporal layer '{}' uses '{}' as its frame instant; enable the QGIS "
            "Temporal Controller to animate it.".format(layer.name(), temporal_field)
        )
        return True

    def load_service_area_layers(self, geojson, layer_name):
        features = geojson.get("features") or []
        if not features:
            self.log("Service-area response had no spatial features to load.", MSG_WARNING)
            return

        metadata = geojson.get("metadata") or {}
        analysis_id = metadata.get("analysis_id") or layer_name or "netweevil_service_area"
        group = self.create_result_group(analysis_id)

        grouped = {}
        threshold_order = []
        for feature in features:
            properties = feature.get("properties") or {}
            geometry_type = properties.get("geometry_type") or "unknown"
            threshold_key = self.service_area_threshold_label(properties)
            key = (threshold_key, geometry_type)
            if key not in grouped:
                grouped[key] = []
                threshold_order.append(key)
            grouped[key].append(feature)

        loaded_layers = []
        for threshold_index, key in enumerate(threshold_order):
            threshold_label, geometry_type = key
            sublayer_name = "{} {}".format(geometry_type, threshold_label).strip()
            temp_geojson = {"type": "FeatureCollection", "features": grouped[key]}
            temp_path = self.write_temp_geojson(
                "{}_{}".format(analysis_id, sublayer_name.replace(" ", "_")),
                temp_geojson,
            )
            layer = QgsVectorLayer(str(temp_path), sublayer_name, "ogr")
            if not layer.isValid():
                self.log("Failed to load layer {}".format(temp_path), MSG_WARNING)
                continue
            self.apply_service_area_style(layer, grouped[key], geometry_type, threshold_index)
            self.configure_temporal_layer(layer, "departure_time")
            QgsProject.instance().addMapLayer(layer, False)
            group.addLayer(layer)
            loaded_layers.append(layer)
            self.log(
                "Loaded service-area layer '{}' with {} feature(s).".format(
                    sublayer_name, len(grouped[key])
                )
            )

        if loaded_layers:
            self.set_last_output_layers(loaded_layers)

    def service_area_threshold_label(self, properties):
        threshold_id = properties.get("threshold_id")
        if threshold_id:
            return str(threshold_id)
        threshold_limit = properties.get("threshold_limit")
        metric = properties.get("threshold_metric") or "threshold"
        if threshold_limit is None:
            return str(metric)
        band_start = properties.get("band_start_limit")
        if band_start is not None:
            return "{} {}-{}".format(metric, band_start, threshold_limit)
        return "{} {}".format(metric, threshold_limit)

    def apply_service_area_style(self, layer, features, geometry_type, threshold_index):
        base_colors = [
            "#0b6e4f",
            "#137547",
            "#1d6fa5",
            "#9f4f0f",
            "#9a275a",
            "#6358d5",
        ]
        base_color = base_colors[threshold_index % len(base_colors)]
        multiple_origins = sorted(
            {
                str((feature.get("properties") or {}).get("origin_id"))
                for feature in features
                if (feature.get("properties") or {}).get("origin_id") not in [None, ""]
            }
        )
        ring_band = any(
            (feature.get("properties") or {}).get("band_start_limit") is not None
            for feature in features
        )

        if len(multiple_origins) > 1:
            categories = []
            for origin_index, origin_id in enumerate(multiple_origins):
                color = base_colors[(threshold_index + origin_index) % len(base_colors)]
                if geometry_type in ("network", "segment"):
                    symbol = QgsLineSymbol.createSimple(
                        {
                            "line_color": color,
                            "line_width": "0.9",
                            "line_style": "dash" if ring_band else "solid",
                        }
                    )
                elif geometry_type == "stop":
                    symbol = QgsMarkerSymbol.createSimple(
                        {
                            "color": color,
                            "outline_color": "#ffffff",
                            "outline_width": "0.4",
                            "size": "2.4",
                        }
                    )
                else:
                    symbol = QgsFillSymbol.createSimple(
                        {
                            "color": color,
                            "outline_color": color,
                            "outline_style": "dash" if ring_band else "solid",
                            "outline_width": "0.7",
                        }
                    )
                categories.append(QgsRendererCategory(origin_id, symbol, origin_id))
            renderer = QgsCategorizedSymbolRenderer("origin_id", categories)
            layer.setRenderer(renderer)
        else:
            if geometry_type in ("network", "segment"):
                symbol = QgsLineSymbol.createSimple(
                    {
                        "line_color": base_color,
                        "line_width": "1.1",
                        "line_style": "dash" if ring_band else "solid",
                    }
                )
            elif geometry_type == "stop":
                symbol = QgsMarkerSymbol.createSimple(
                    {
                        "color": base_color,
                        "outline_color": "#ffffff",
                        "outline_width": "0.4",
                        "size": "2.4",
                    }
                )
            else:
                symbol = QgsFillSymbol.createSimple(
                    {
                        "color": base_color,
                        "outline_color": base_color,
                        "outline_style": "dash" if ring_band else "solid",
                        "outline_width": "0.7",
                    }
                )
            layer.renderer().setSymbol(symbol)
        layer.triggerRepaint()

    def log_analysis_messages(self, analysis_kind, response_json):
        result = response_json.get("result") or {}
        warnings = result.get("warnings") or []
        for warning in warnings:
            self.log("{} warning: {}".format(analysis_kind, warning), MSG_WARNING)

        diagnostics = result.get("diagnostics") or []
        for diagnostic in diagnostics:
            if not isinstance(diagnostic, dict):
                self.log(
                    "{} diagnostic: {}".format(analysis_kind, diagnostic),
                    MSG_WARNING,
                )
                continue
            level = MSG_WARNING
            if diagnostic.get("severity") == "error":
                level = MSG_CRITICAL
            self.log(
                "{} diagnostic [{}]: {}".format(
                    analysis_kind,
                    diagnostic.get("code", "diagnostic"),
                    diagnostic.get("message", ""),
                ),
                level,
            )
            for action in diagnostic.get("suggested_actions") or []:
                self.log(
                    "{} next action: {}".format(analysis_kind, action),
                    MSG_WARNING,
                )

        if analysis_kind == "route":
            self.log(
                "route outcome={} fallback_used={} violations={}.".format(
                    result.get("outcome", "unknown"),
                    result.get("fallback_used", False),
                    len(result.get("violations") or []),
                )
            )
            violations = result.get("violations") or []
            for violation in violations:
                self.log(
                    "route violation [{}]: penalty_s={}".format(
                        violation.get("violation_type", "unknown"),
                        violation.get("penalty_s", 0.0),
                    ),
                    MSG_WARNING,
                )
            return

        if analysis_kind == "service_area":
            self.log(
                "service_area outcome={} processed_origins={} skipped_origins={} fallback_origins={} features={}.".format(
                    result.get("outcome", "unknown"),
                    result.get("processed_origin_count", 0),
                    result.get("skipped_origin_count", 0),
                    result.get("fallback_origin_count", 0),
                    len(result.get("features") or []),
                )
            )
            return

        if analysis_kind == "transit_route":
            summary = result.get("summary") or {}
            self.log(
                "transit_route outcome={} legs={} boardings={} total_time_s={}.".format(
                    result.get("outcome", "unknown"),
                    len(result.get("legs") or []),
                    summary.get("boarding_count", 0),
                    summary.get("total_travel_time_s"),
                )
            )
            return

        items = result.get("pairs") if analysis_kind == "od" else result.get("cells")
        if not isinstance(items, list):
            return
        counts = {}
        outcomes = {}
        fallback_count = 0
        for item in items:
            status = item.get("status", "unknown")
            counts[status] = counts.get(status, 0) + 1
            outcome = item.get("outcome", "unknown")
            outcomes[outcome] = outcomes.get(outcome, 0) + 1
            if item.get("fallback_used"):
                fallback_count += 1
        self.log(
            "{} summary: statuses={} outcomes={} fallback_items={}.".format(
                analysis_kind, counts, outcomes, fallback_count
            )
        )
        for item in items:
            item_diagnostics = item.get("diagnostics") or []
            if item_diagnostics:
                item_id = item.get("pair_id") or "{}->{}".format(
                    item.get("origin_id", "?"), item.get("destination_id", "?")
                )
                for diagnostic in item_diagnostics:
                    level = MSG_WARNING
                    if diagnostic.get("severity") == "error":
                        level = MSG_CRITICAL
                    self.log(
                        "{} item {} diagnostic [{}]: {}".format(
                            analysis_kind,
                            item_id,
                            diagnostic.get("code", "diagnostic"),
                            diagnostic.get("message", ""),
                        ),
                        level,
                    )
                    for action in diagnostic.get("suggested_actions") or []:
                        self.log(
                            "{} item {} next action: {}".format(
                                analysis_kind,
                                item_id,
                                action,
                            ),
                            MSG_WARNING,
                        )

    def log_geojson_messages(self, analysis_kind, geojson):
        metadata = geojson.get("metadata") or {}
        warnings = metadata.get("warnings") or []
        for warning in warnings:
            self.log("{} warning: {}".format(analysis_kind, warning), MSG_WARNING)
        features = geojson.get("features") or []
        if analysis_kind == "route" and features:
            properties = features[0].get("properties") or {}
            self.log(
                "route outcome={} fallback_used={} violations={}.".format(
                    properties.get("outcome", "unknown"),
                    properties.get("fallback_used", False),
                    properties.get("violation_count", 0),
                )
            )
        elif analysis_kind in ["od", "matrix"] and features:
            status_counts = {}
            outcome_counts = {}
            fallback_count = 0
            for feature in features:
                properties = feature.get("properties") or {}
                status = properties.get("status", "unknown")
                outcome = properties.get("outcome", "unknown")
                status_counts[status] = status_counts.get(status, 0) + 1
                outcome_counts[outcome] = outcome_counts.get(outcome, 0) + 1
                if properties.get("fallback_used"):
                    fallback_count += 1
            self.log(
                "{} summary: statuses={} outcomes={} fallback_items={}.".format(
                    analysis_kind, status_counts, outcome_counts, fallback_count
                )
            )
        if metadata:
            summary = ", ".join(
                "{}={}".format(key, value)
                for key, value in metadata.items()
                if key not in ["warnings"] and value not in [None, "", []]
            )
            if summary:
                self.log("{} summary: {}.".format(analysis_kind, summary))

    def remove_existing_result_layer(self, layer_name):
        project = QgsProject.instance()
        for layer in list(project.mapLayers().values()):
            if layer.name() == layer_name:
                project.removeMapLayer(layer.id())
