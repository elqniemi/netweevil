import assert from "node:assert/strict";
import test from "node:test";
import { elevatedData } from "../src/geo/elevation.ts";
import { stationGeometryFeatures } from "../src/geo/stationGeometry.ts";

const outer = [[114, 22, -12], [114.01, 22, -12], [114.01, 22.01, -12], [114, 22.01, -12], [114, 22, -12]];
const hole = [[114.001, 22.001, -12], [114.002, 22.001, -12], [114.002, 22.002, -12], [114.001, 22.001, -12]];
const source = { type: "FeatureCollection", metadata: { available: true, matched: 2, returned: 2, truncated: false, surface_source_available: true }, features: [
  { type: "Feature", geometry: { type: "Polygon", coordinates: [outer, hole] }, properties: { kind: "platform_surface", source_kind: "platform_level_fallback", station_name_en: "Admiralty", platform_numbers: [5, 6], numbered_platforms: [{ ref: "5", directions: [{ line_code: "EAL", direction: "DT", destination_station_code: "LOW" }] }] } },
  { type: "Feature", geometry: { type: "Point", coordinates: [114.001, 22.001, -14] }, properties: { kind: "boarding_point", name: "Admiralty platform 5" } },
] };
const options = { verticalExaggeration: 3, altitudeSlice: false, minAltitude: -13, maxAltitude: -11 };

test("station surfaces preserve XYZ and holes without shifting them to bound boarding height", () => {
  const original = JSON.stringify(source);
  const styled = stationGeometryFeatures(source);
  const data = elevatedData(styled, options);
  assert.equal(data.polygons.length, 1);
  assert.equal(data.polygons[0].polygon.length, 2);
  assert.equal(data.polygons[0].polygon[0][0][2], -36);
  assert.equal(data.points[0].position[2], -42);
  assert.equal(styled.features[0].properties._label, "Admiralty\nPlatform 5, 6");
  assert.match(styled.features[0].properties.direction, /Platform 5: EAL DT → LOW/);
  assert.deepEqual(styled.features[0].properties.numbered_platforms, source.features[0].properties.numbered_platforms);
  assert.match(styled.features[0].properties.surface_note, /Whole platform-floor footprint/);
  assert.equal(styled.features[0].properties.source_kind, "platform_level_fallback");
  assert.equal(styled.features[1].properties.geometry_elevation_source, "bound_gtfs_boarding_point");
  assert.equal(JSON.stringify(source), original);
});

test("source-altitude slabs retain intersecting platform surfaces and hide other floors", () => {
  const styled = stationGeometryFeatures(source);
  const data = elevatedData(styled, { ...options, altitudeSlice: true });
  assert.equal(data.polygons.length, 1);
  assert.equal(data.points.length, 0);
  assert.equal(elevatedData(styled, { ...options, altitudeSlice: true, minAltitude: 0, maxAltitude: 10 }).polygons.length, 0);
  const multipart = { type: "FeatureCollection", features: [{ ...styled.features[0], geometry: { type: "MultiPolygon", coordinates: [[outer, hole], [outer]] } }] };
  assert.equal(elevatedData(multipart, options).polygons.length, 2);
});
