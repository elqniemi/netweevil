import type { Feature, FeatureCollection } from "./features";

export type XYZ = [number, number, number];
export interface ElevatedPath { path: XYZ[]; feature: Feature; known: boolean }
export interface ElevatedPoint { position: XYZ; feature: Feature; known: boolean }
export interface ElevatedPolygon { polygon: XYZ[][]; feature: Feature; known: boolean }
export interface ElevationOptions {
  verticalExaggeration: number;
  altitudeSlice: boolean;
  minAltitude: number;
  maxAltitude: number;
}

function validPosition(value: unknown): value is number[] {
  return Array.isArray(value) && value.length >= 2 && Number.isFinite(value[0]) && Number.isFinite(value[1]);
}

/** Clip segments against an altitude slab, interpolating at its boundaries.
 * Never join disconnected pieces across an excluded section of the route. */
export function clipPath(path: XYZ[], min: number, max: number): XYZ[][] {
  if (min > max) return [];
  const pieces: XYZ[][] = [];
  let active: XYZ[] = [];
  for (let i = 1; i < path.length; i++) {
    const a = path[i - 1], b = path[i];
    const dz = b[2] - a[2];
    let start = 0, end = 1;
    if (dz === 0) {
      if (a[2] < min || a[2] > max) { active = []; continue; }
    } else {
      const t0 = (min - a[2]) / dz, t1 = (max - a[2]) / dz;
      start = Math.max(0, Math.min(t0, t1));
      end = Math.min(1, Math.max(t0, t1));
      if (start > end) { active = []; continue; }
    }
    const interpolate = (t: number): XYZ => a.map((v, j) => v + (b[j] - v) * t) as XYZ;
    const first = interpolate(start), last = interpolate(end);
    const previous = active[active.length - 1];
    if (!previous || previous.some((v, j) => Math.abs(v - first[j]) > 1e-10)) {
      active = [first];
      pieces.push(active);
    }
    active.push(last);
    if (end < 1) active = [];
  }
  return pieces;
}

/** Build display copies. Exports and stored responses retain original metres. */
export function elevatedData(collection: FeatureCollection, options: ElevationOptions) {
  const paths: ElevatedPath[] = [], points: ElevatedPoint[] = [], polygons: ElevatedPolygon[] = [];
  let min = Infinity, max = -Infinity, unknown = 0;
  const exaggeration = Math.max(0.1, options.verticalExaggeration);
  for (const feature of collection.features) {
    const add = (positions: unknown, point: boolean) => {
      if (!Array.isArray(positions)) return;
      let path: XYZ[] = [];
      let known = feature.properties.elevation_known !== false
        && feature.properties.geometry_elevation_source !== "unknown_gtfs_elevation"
        && feature.properties.geometry_elevation_source !== "bound_endpoint_only_other_elevations_unknown";
      const flush = () => {
        if (!path.length) return;
        if (!known) unknown++;
        if (known) for (const p of path) { min = Math.min(min, p[2]); max = Math.max(max, p[2]); }
        const project = (p: XYZ): XYZ => [p[0], p[1], p[2] * exaggeration];
        if (point) {
          const p = path[0];
          if (!options.altitudeSlice || p[2] >= options.minAltitude && p[2] <= options.maxAltitude) points.push({ position: project(p), feature, known });
        } else {
          const pieces = options.altitudeSlice ? clipPath(path, options.minAltitude, options.maxAltitude) : [path];
          for (const piece of pieces) if (piece.length >= 2) paths.push({ path: piece.map(project), feature, known });
        }
        path = [];
      };
      for (const value of positions) {
        if (!validPosition(value)) { flush(); continue; }
        const hasZ = typeof value[2] === "number" && Number.isFinite(value[2]);
        known &&= hasZ;
        path.push([value[0], value[1], hasZ ? value[2] : 0]);
      }
      flush();
    };
    const { type, coordinates } = feature.geometry;
    const addPolygon = (rings: unknown) => {
      if (!Array.isArray(rings) || !rings.length || !rings.every(ring => Array.isArray(ring) && ring.length >= 4 && ring.every(validPosition))) return;
      const known = feature.properties.elevation_known !== false && rings.every(ring => ring.every((p: number[]) => typeof p[2] === "number" && Number.isFinite(p[2])));
      const polygon: XYZ[][] = rings.map(ring => ring.map((p: number[]): XYZ => [p[0], p[1], Number.isFinite(p[2]) ? p[2] : 0]));
      const heights = polygon.flat().map(p => p[2]);
      const low = Math.min(...heights), high = Math.max(...heights);
      if (known) { min = Math.min(min, low); max = Math.max(max, high); } else unknown++;
      // Preserve each surveyed surface and its holes. A slab includes whole
      // surfaces that intersect it; lines are clipped exactly above.
      if (options.altitudeSlice && (options.minAltitude > options.maxAltitude || high < options.minAltitude || low > options.maxAltitude)) return;
      polygons.push({ polygon: polygon.map(ring => ring.map(p => [p[0], p[1], p[2] * exaggeration])), feature, known });
    };
    if (type === "LineString") add(coordinates, false);
    else if (type === "MultiLineString" && Array.isArray(coordinates)) for (const line of coordinates) add(line, false);
    else if (type === "Point") add([coordinates], true);
    else if (type === "MultiPoint" && Array.isArray(coordinates)) for (const p of coordinates) add([p], true);
    else if (type === "Polygon") addPolygon(coordinates);
    else if (type === "MultiPolygon" && Array.isArray(coordinates)) for (const polygon of coordinates) addPolygon(polygon);
  }
  return { paths, points, polygons, min: Number.isFinite(min) ? min : null, max: Number.isFinite(max) ? max : null, unknown };
}

/** Absolute elevation, not an inferred floor or a claim about ground cover. */
export function elevationColor(z: number, min: number, max: number): [number, number, number] {
  const t = Math.max(0, Math.min(1, (z - min) / (max - min || 1)));
  return [Math.round(37 + t * 212), Math.round(99 + t * 16), Math.round(235 - t * 213)];
}
