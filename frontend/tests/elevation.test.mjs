import assert from "node:assert/strict";
import test from "node:test";
import { clipPath, elevatedData } from "../src/geo/elevation.ts";
import { cleanFeatures, extractFeatures } from "../src/geo/features.ts";
import { hongKongTransitDefaults } from "../src/state/transitDefaults.ts";

const options = { verticalExaggeration: 4, altitudeSlice: false, minAltitude: -10, maxAltitude: 10 };
const feature = (coordinates, properties = {}) => ({ type: "Feature", geometry: { type: "LineString", coordinates }, properties });
const collection = (...features) => ({ type: "FeatureCollection", features });

test("JSON route and transit leg extraction and export preserve actual elevations", () => {
  const geometry = [[114.15, 22.28, -12], [114.1501, 22.2801, 4.5]];
  const route = extractFeatures({ result: { geometry, legs: [{ leg_type: "transit", geometry }] } });
  assert.equal(route.features.length, 2);
  for (const f of cleanFeatures(route).features) assert.deepEqual(f.geometry.coordinates, geometry);
  const point = extractFeatures({ stop: { location: [114.15, 22.28, -12] } });
  assert.deepEqual(point.features[0].geometry.coordinates, [114.15, 22.28, -12]);
  const boundStop = extractFeatures({ stops: [{ lon: 114.15, lat: 22.28, z: -14.13 }] });
  assert.deepEqual(boundStop.features[0].geometry.coordinates, [114.15, 22.28, -14.13]);
});

test("exaggeration changes only display copies, including negative heights and vertical lifts", () => {
  const source = collection(feature([[114, 22, -10], [114, 22, 8]]));
  const before = JSON.stringify(source);
  const data = elevatedData(source, options);
  assert.deepEqual(data.paths[0].path, [[114, 22, -40], [114, 22, 32]]);
  assert.equal(JSON.stringify(source), before);
  assert.equal(data.min, -10);
  assert.equal(data.max, 8);
});

test("altitude slicing interpolates crossing segments and separates excursions", () => {
  const path = [[114, 22, -20], [114, 22, 20], [114.1, 22, 20], [114.1, 22, -20]];
  assert.deepEqual(clipPath(path, -5, 5), [
    [[114, 22, -5], [114, 22, 5]],
    [[114.1, 22, 5], [114.1, 22, -5]],
  ]);
  assert.deepEqual(clipPath(path, 5, -5), []);
  assert.deepEqual(clipPath([[0, 0, 0], [1, 1, 0]], 1, 10), []);
});

test("missing altitude is identified, and no missing coordinate becomes a fabricated bridge", () => {
  const data = elevatedData(collection(feature([[114, 22], [114.1, 22, null]])), options);
  assert.equal(data.unknown, 1);
  assert.equal(data.paths[0].known, false);
  assert.equal(data.min, null);
  assert.deepEqual(data.paths[0].path, [[114, 22, 0], [114.1, 22, 0]]);
  const explicit = elevatedData(collection(feature([[114, 22, 0], [114.1, 22, 0]], { elevation_known: false })), options);
  assert.equal(explicit.paths[0].known, false);
  const gtfs = elevatedData(collection(feature([[114, 22, 0], [114.1, 22, 0]], { geometry_elevation_source: "unknown_gtfs_elevation" })), options);
  assert.equal(gtfs.paths[0].known, false);
});

test("slab filtering uses source metres before exaggerating and handles multipart paths", () => {
  const source = collection({ type: "Feature", properties: {}, geometry: { type: "MultiLineString", coordinates: [
    [[114, 22, -20], [114, 22, 20]], [[115, 22, 100], [115, 22, 101]],
  ] } });
  const data = elevatedData(source, { ...options, altitudeSlice: true });
  assert.equal(data.paths.length, 1);
  assert.deepEqual(data.paths[0].path, [[114, 22, -40], [114, 22, 40]]);
});

test("network source attributes remain inspectable with known elevation metadata", () => {
  const data = extractFeatures(collection(feature([[114, 22, -4], [114, 22, -2]], {
    elevation_known: true, source_attributes: { FeatureType: "SUBWAY", Location: "INDOOR" },
  })));
  assert.equal(data.features[0].properties["source.FeatureType"], "SUBWAY");
  assert.equal(data.features[0].properties.elevation_known, true);
});

test("Hong Kong transit defaults choose network access and registered transfers", () => {
  const service = { loaded_profiles: [{ profile_id: "hk_walk", mode: "foot" }], loaded_transit_feeds: [{ agency_timezone: "Asia/Hong_Kong", service_start_date: "2026-09-10", transfer_profile_ids: ["other_foot_profile", "hk_walk"] }] };
  assert.deepEqual(hongKongTransitDefaults(service), { street_access: "network", walking_geometry: "network", include_stop_segments: true, pedestrian_profile_id: "hk_walk", transfer_profile_id: "hk_walk", datetime: "2026-09-10T08:30:00+08:00" });
  assert.equal(hongKongTransitDefaults({ ...service, loaded_profiles: [] }), null);
});
