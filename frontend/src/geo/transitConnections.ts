import type { Feature, FeatureCollection, Position } from "./features";

function point(value: unknown): { id: unknown; coordinate: Position } | null {
  if (!value || typeof value !== "object") return null;
  const p = value as Record<string, unknown>;
  if (typeof p.lon !== "number" || typeof p.lat !== "number" || !Number.isFinite(p.lon) || !Number.isFinite(p.lat)) return null;
  return { id: p.id, coordinate: typeof p.z === "number" && Number.isFinite(p.z) ? [p.lon, p.lat, p.z] : [p.lon, p.lat] };
}

function horizontalDistance(a: Position, b: Position): number {
  const rad = Math.PI / 180;
  const lat = (b[1] - a[1]) * rad, lon = (b[0] - a[0]) * rad;
  const h = Math.sin(lat / 2) ** 2 + Math.cos(a[1] * rad) * Math.cos(b[1] * rad) * Math.sin(lon / 2) ** 2;
  return 6371008.8 * 2 * Math.asin(Math.sqrt(Math.min(1, h)));
}

/** Show the gap to the surveyed network without claiming it is a routed path.
 * Always pass the saved request for this result, never the current map markers. */
export function addTransitConnections(collection: FeatureCollection, requestBody: unknown): FeatureCollection {
  const request = (requestBody as { request?: { origin?: unknown; destination?: unknown } } | null)?.request;
  const origin = point(request?.origin), destination = point(request?.destination);
  if (!origin && !destination) return collection;
  const features = collection.features.filter(f => f.properties._kind !== "off_network_connection");
  const connections: Feature[] = [];
  for (const feature of features) {
    const p = feature.properties;
    if (p.leg_type !== "access" && p.leg_type !== "egress") continue;
    const positions = feature.geometry.type === "LineString" ? feature.geometry.coordinates as Position[]
      : feature.geometry.type === "Point" ? [feature.geometry.coordinates as Position] : [];
    if (!positions.length) continue;
    const add = (endpoint: "origin" | "destination", requested: NonNullable<ReturnType<typeof point>>, snapped: Position) => {
      if (!Array.isArray(snapped) || !Number.isFinite(snapped[0]) || !Number.isFinite(snapped[1])) return;
      const gap = horizontalDistance(requested.coordinate, snapped);
      const known = requested.coordinate.length > 2 && typeof snapped[2] === "number" && Number.isFinite(snapped[2]);
      const verticalGap = known ? Math.abs(requested.coordinate[2] - snapped[2]) : 0;
      if (gap < 0.5 && verticalGap < 0.5) return;
      const coordinates = endpoint === "origin" ? [requested.coordinate, snapped.slice()] : [snapped.slice(), requested.coordinate];
      connections.push({ type: "Feature", geometry: { type: "LineString", coordinates }, properties: {
        _kind: "off_network_connection", _path: `${p._path}.${endpoint}_connection`, _index: p._index,
        name: "Off-network connection (not routed)", endpoint, routed: false,
        horizontal_gap_m: Math.round(gap * 10) / 10,
        connection_note: "Unsurveyed connection to the network. Distance and time are estimates; this line does not establish a traversable path.",
        elevation_known: known, geometry_elevation_source: "off_network_connection",
      } });
    };
    if (origin && origin.id != null && p.from_id === origin.id) add("origin", origin, positions[0]);
    // A direct walking journey can use one access leg for both endpoints.
    if (destination && destination.id != null && p.to_id === destination.id) add("destination", destination, positions[positions.length - 1]);
  }
  return { ...collection, features: [...features, ...connections].map((feature, id) => ({ ...feature, id })) };
}
