// Map features of trip and height probe responses that the generic walker
// cannot derive: trip visit-order badges at the stops, and DEM samples at the
// probed positions.

import type { Feature, FeatureCollection } from "./features";

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function finite(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

interface RequestStop {
  id: string;
  lon: number;
  lat: number;
}

/** Stops of a trip request body (`{request: {stops: [{point}]}}`). */
function requestStops(body: unknown): RequestStop[] {
  const request = isObject(body) && isObject(body.request) ? body.request : null;
  if (!request || !Array.isArray(request.stops)) return [];
  return request.stops.map((stop) => {
    const point = isObject(stop) && isObject(stop.point) ? stop.point : {};
    return { id: String(point.id ?? ""), lon: Number(point.lon), lat: Number(point.lat) };
  });
}

/**
 * Tags trip legs with their order and adds one numbered badge per visited
 * stop. A roundtrip's closing visit returns to the start, whose badge already
 * reads 1, so it is not repeated.
 */
export function tripFeatures(collection: FeatureCollection, body: unknown, data: unknown): FeatureCollection {
  const features: Feature[] = [];
  let legCount = 0;
  for (const f of collection.features) {
    const p = f.properties;
    if (p.kind === "trip_stop") continue; // GeoJSON responses: rebuilt below from the request stops
    const legIndex = finite(p.leg_index) ? p.leg_index : p._kind === "legs" && finite(p._index) ? p._index : null;
    if (legIndex !== null && (f.geometry.type === "LineString" || f.geometry.type === "MultiLineString")) {
      legCount = Math.max(legCount, legIndex + 1);
      features.push({ ...f, properties: { ...p, _kind: "legs", _index: legIndex, leg: legIndex + 1 } });
    } else features.push(f);
  }
  for (const f of features) if (f.properties._kind === "legs") f.properties._leg_count = legCount;

  const root = isObject(data) ? data : {};
  const result = isObject(root.result) ? root.result : root;
  const visits = Array.isArray(result.visits) ? result.visits.filter(isObject) : [];
  const geojsonStops = collection.features.filter((f) => f.properties.kind === "trip_stop");
  const stops = requestStops(body);
  const roundtrip = result.mode === "roundtrip";
  const visitRows: Record<string, unknown>[] = visits.length
    ? visits
    : geojsonStops.map((f) => ({ ...f.properties, stop_index: f.properties.stop_index }));
  visitRows.forEach((visit, position) => {
    const stopIndex = Number(visit.stop_index);
    if (roundtrip && position > 0 && position === visitRows.length - 1 && stopIndex === Number(visitRows[0].stop_index)) return;
    const stop = stops[stopIndex];
    let coordinates: number[] | null = stop && finite(stop.lon) && finite(stop.lat) ? [stop.lon, stop.lat] : null;
    if (!coordinates) {
      const fromGeojson = geojsonStops.find((f) => Number(f.properties.visit_index) === position);
      coordinates = (fromGeojson?.geometry.coordinates as number[] | undefined) ?? null;
    }
    if (!coordinates) return;
    features.push({
      type: "Feature",
      geometry: { type: "Point", coordinates },
      properties: {
        visit: position + 1,
        point_id: visit.point_id ?? stop?.id ?? null,
        stop_index: stopIndex,
        arrival_s: visit.arrival_s,
        wait_s: visit.wait_s,
        service_s: visit.service_s,
        departure_s: visit.departure_s,
        lateness_s: visit.lateness_s,
        _kind: "trip_stops",
        _label: String(position + 1),
        _late: finite(visit.lateness_s) && visit.lateness_s > 1e-6 ? 1 : 0,
      },
    });
  });
  return { ...collection, features: features.map((f, id) => ({ ...f, id })) };
}

/**
 * DEM samples of a `/v1/height` response as points (and the probed line in
 * profile mode). Heights line up with the resampled `shape` when the request
 * resampled, and with the request points otherwise.
 */
export function heightFeatures(body: unknown, data: unknown): FeatureCollection {
  const request = isObject(body) ? body : {};
  const root = isObject(data) ? data : {};
  const result = isObject(root.result) ? root.result : {};
  const input = Array.isArray(request.points) ? (request.points as unknown[]).filter((p): p is number[] => Array.isArray(p) && finite(p[0]) && finite(p[1])) : [];
  const shape = Array.isArray(result.shape) ? (result.shape as number[][]) : input;
  const features: Feature[] = [];
  const range = Array.isArray(result.range_heights) ? (result.range_heights as [number, number | null][]) : null;
  const heights = range ? range.map((pair) => pair[1]) : Array.isArray(result.heights) ? (result.heights as (number | null)[]) : [];
  if (range && shape.length >= 2) {
    features.push({
      type: "Feature",
      geometry: { type: "LineString", coordinates: shape.map((p) => [p[0], p[1]]) },
      properties: { _kind: "height_line", length_m: range[range.length - 1]?.[0] ?? null },
    });
  }
  // A dense resampled profile would bury the map in labels: mark the probed
  // points, and only label every sample when there are few.
  const labelAll = shape.length <= 40;
  shape.forEach((p, index) => {
    const height = heights[index];
    const known = finite(height);
    const isInput = input.some((q) => Math.abs(q[0] - p[0]) < 1e-7 && Math.abs(q[1] - p[1]) < 1e-7);
    if (!labelAll && !isInput) return;
    features.push({
      type: "Feature",
      geometry: { type: "Point", coordinates: known ? [p[0], p[1], height] : [p[0], p[1]] },
      properties: {
        sample: index + 1,
        height_m: known ? height : null,
        ...(range ? { distance_m: range[index]?.[0] ?? null } : {}),
        _kind: "heights",
        _label: known ? `${height.toFixed(1)} m` : "no data",
      },
    });
  });
  return { type: "FeatureCollection", features: features.map((f, id) => ({ ...f, id })) };
}
