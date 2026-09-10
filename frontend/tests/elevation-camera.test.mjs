import assert from "node:assert/strict";
import test from "node:test";
import { MapView } from "@deck.gl/core";
import { ELEVATION_MAP_VIEW } from "../src/map/ThreeDOverlay.ts";

const viewport = (view, zoom, pitch) => view.makeViewport({
  width: 1200, height: 800,
  viewState: { latitude: 22.28, longitude: 114.165, zoom, pitch },
});

test("underground geometry remains inside the depth clip at street zoom", () => {
  const underground = [114.165, 22.28, -33.63 * 20];
  assert.ok(viewport(new MapView({ id: "mapbox" }), 18, 0).project(underground)[2] > 1,
    "reproduce the default camera's below-ground far clipping");
  for (const pitch of [0, 45, 60]) for (const zoom of [10, 16, 18, 20, 22, 24]) {
    const z = viewport(ELEVATION_MAP_VIEW, zoom, pitch).project(underground)[2];
    assert.ok(z >= -1 && z <= 1, `depth clipping at zoom ${zoom}, pitch ${pitch}: ${z}`);
  }
});

test("the overlay far-plane override preserves the basemap XY projection", () => {
  for (const pitch of [0, 45, 60]) for (const zoom of [10, 18, 22]) {
    const ordinary = viewport(new MapView({ id: "mapbox" }), zoom, pitch);
    const expanded = viewport(ELEVATION_MAP_VIEW, zoom, pitch);
    for (const point of [[114.165, 22.28, 0], [114.166, 22.281, -33.63], [114.164, 22.279, 27.77]]) {
      const a = ordinary.project(point), b = expanded.project(point);
      assert.ok(Math.abs(a[0] - b[0]) < 1e-6);
      assert.ok(Math.abs(a[1] - b[1]) < 1e-6);
    }
  }
});


test("close elevated geometry ahead of the camera survives near clipping", () => {
  // At zoom 16 this camera is 1324 m above datum. The 20x display of a
  // 63 m feature remains 64 m ahead of it, inside the old near clip.
  const point = [114.165, 22.28, 63 * 20];
  const oldDepth = viewport(new MapView({ id: "mapbox" }), 16, 0).project(point)[2];
  const newDepth = viewport(ELEVATION_MAP_VIEW, 16, 0).project(point)[2];
  assert.ok(oldDepth < -1);
  assert.ok(newDepth >= -1 && newDepth <= 1);
});

test("a dashed-style surveyed transfer with a vertical segment avoids the GPU dash shader", async () => {
  const { createThreeDOverlay } = await import("../src/map/ThreeDOverlay.ts");
  const renderer = createThreeDOverlay(() => {});
  let rendered;
  renderer.control.setProps = props => { rendered = props.layers; };
  const source = { type: "FeatureCollection", features: [
    { type: "Feature", properties: { _kind: "legs", leg_type: "transfer", _dash: 1, elevation_known: true },
      geometry: { type: "LineString", coordinates: [[114.16, 22.28, -10], [114.16, 22.28, -5], [114.1601, 22.2801, -5]] } },
    { type: "Feature", properties: { _kind: "off_network_connection", _dash: 1, elevation_known: false },
      geometry: { type: "LineString", coordinates: [[114.16, 22.28], [114.1601, 22.2801]] } },
  ] };
  const before = JSON.stringify(source);
  renderer.update(source, { view3d: true, verticalExaggeration: 1, altitudeSlice: false,
    minAltitude: -100, maxAltitude: 100, elevationColor: false }, null);
  const solid = rendered.find(layer => layer.id === "network-3d-paths");
  const dashed = rendered.find(layer => layer.id === "network-3d-dashed-paths");
  assert.equal(solid.props.data.length, 1);
  assert.equal(solid.props.extensions.length, 0);
  assert.equal(dashed.props.data.length, 1);
  assert.equal(JSON.stringify(source), before);
});
