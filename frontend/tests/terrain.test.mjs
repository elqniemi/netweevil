import assert from "node:assert/strict";
import test from "node:test";
import { applyTerrain, selectedTerrain, terrainSourceSpec, terrainTileError, TERRAIN_SOURCE, TERRAIN_HILLSHADE } from "../src/map/terrain.ts";
import { DEFAULT_MAP_STYLE } from "../src/state/store.ts";

const source = { id: "hong_kong_5m", name: "Hong Kong LandsD 5 m terrain", version: "abc", tiles: ["/v1/terrain/hong_kong_5m/abc/{z}/{x}/{y}.png"], bounds: [113.8, 22.1, 114.5, 22.6], minzoom: 8, maxzoom: 15, encoding: "terrarium", tile_size: 256, resolution_m: 5, vertical_datum: "HKPD", attribution: "Terrain © Lands Department" };
const options = { view3d: true, enabled: true, exaggeration: 3, source, apiBase: "http://127.0.0.1:8080" };

function mapFixture() {
  const sources = new Map(), layers = new Map();
  const calls = [];
  let terrain = null;
  return { calls, sources, layers,
    getSource: id => sources.has(id) ? { serialize: () => sources.get(id) } : undefined,
    getLayer: id => layers.get(id), getTerrain: () => terrain,
    getStyle: () => ({ layers: [{ id: "background", type: "background" }, { id: "place-labels", type: "symbol" }] }),
    addSource: (id, spec) => { calls.push(["source", id]); sources.set(id, spec); },
    removeSource: id => { assert.notEqual(terrain?.source, id, "detach terrain before deleting its source"); calls.push(["remove-source", id]); sources.delete(id); },
    addLayer: (spec, before) => { calls.push(["layer", spec.id, before]); layers.set(spec.id, spec); },
    removeLayer: id => { calls.push(["remove-layer", id]); layers.delete(id); },
    setTerrain: spec => { calls.push(["terrain", spec]); terrain = spec; },
  };
}

test("local terrain preserves HKPD metre encoding and resolves versioned tiles against the chosen API", () => {
  const spec = terrainSourceSpec(source, options.apiBase + "/");
  assert.equal(spec.encoding, "terrarium");
  assert.equal(spec.tileSize, 256);
  assert.deepEqual(spec.tiles, ["http://127.0.0.1:8080/v1/terrain/hong_kong_5m/abc/{z}/{x}/{y}.png"]);
  assert.deepEqual(spec.bounds, source.bounds);
  assert.equal(spec.maxzoom, 15);
  assert.equal(spec.attribution, source.attribution);
  assert.equal(selectedTerrain([source], "missing-source"), source);
  assert.equal(selectedTerrain([], null), null);
  assert.equal(DEFAULT_MAP_STYLE.terrainEnabled, false);
});

test("terrain reuses its source, updates only exaggeration, and restores after a basemap style replacement", () => {
  const map = mapFixture();
  applyTerrain(map, options);
  assert.deepEqual(map.getTerrain(), { source: TERRAIN_SOURCE, exaggeration: 3 });
  assert.ok(map.calls.some(call => call[0] === "layer" && call[2] === "place-labels"));
  const initialCalls = map.calls.length;
  applyTerrain(map, { ...options });
  assert.equal(map.calls.length, initialCalls, "unchanged state must not reload DEM tiles");
  applyTerrain(map, { ...options, exaggeration: 8 });
  assert.equal(map.calls.length, initialCalls + 1);
  assert.equal(map.getTerrain().exaggeration, 8);
  const replacedStyle = mapFixture();
  applyTerrain(replacedStyle, { ...options, exaggeration: 8 });
  assert.equal(replacedStyle.getTerrain().exaggeration, 8);
  assert.ok(replacedStyle.getLayer(TERRAIN_HILLSHADE));
});

test("2D and disabling remove terrain cleanly; a changed API replaces its old tile source", () => {
  const map = mapFixture();
  applyTerrain(map, { ...options, view3d: false });
  assert.equal(map.calls.length, 0);
  applyTerrain(map, options);
  applyTerrain(map, { ...options, apiBase: "http://localhost:8081" });
  assert.match(map.sources.get(TERRAIN_SOURCE).tiles[0], /^http:\/\/localhost:8081\//);
  applyTerrain(map, { ...options, enabled: false });
  assert.equal(map.getTerrain(), null);
  assert.equal(map.getSource(TERRAIN_SOURCE), undefined);
  assert.equal(map.getLayer(TERRAIN_HILLSHADE), undefined);
  applyTerrain(map, { ...options, source: null });
  assert.equal(map.getTerrain(), null);
});

test("missing-coverage tiles are distinguished from other terrain failures", () => {
  assert.match(terrainTileError(Object.assign(new Error("not found"), { status: 404 })), /no DEM coverage/);
  assert.match(terrainTileError(new Error("HTTP 503")), /could not load/);
});
