import assert from "node:assert/strict";
import test from "node:test";
import { StationGeometryCache } from "../src/geo/stationGeometryCache.ts";

const response = (lon = 114.16) => ({ type: "FeatureCollection", metadata: { available: true, matched: 1, returned: 1, truncated: false, surface_source_available: true }, features: [
  { type: "Feature", geometry: { type: "Point", coordinates: [lon, 22.28, -12] }, properties: { kind: "boarding_point", stop_id: "ADM-5", name: "Admiralty platform 5" } },
] });

test("panning retains visible stations during loading and transient failure, then replaces them on success", () => {
  const cache = new StationGeometryCache();
  const first = cache.begin("api/workspace/dataset/feed");
  const loaded = cache.complete(first.ticket, response()).stationGeometry;
  const pan = cache.begin("api/workspace/dataset/feed");
  assert.equal(pan.display.stationGeometry, loaded);
  assert.match(pan.display.stationGeometryStatus, /Updating/);
  const failed = cache.fail(pan.ticket, new Error("temporary network failure"));
  assert.equal(failed.stationGeometry, loaded);
  assert.match(failed.stationGeometryStatus, /Showing the previous viewport/);
  const retry = cache.begin("api/workspace/dataset/feed");
  const replacement = cache.complete(retry.ticket, response(114.2)).stationGeometry;
  assert.notEqual(replacement, loaded);
  assert.equal(replacement.features[0].geometry.coordinates[0], 114.2);
});

test("identical station responses preserve geometry identity and avoid rebuilding the renderer", () => {
  const cache = new StationGeometryCache();
  const first = cache.begin("hk");
  const original = cache.complete(first.ticket, response()).stationGeometry;
  const pan = cache.begin("hk");
  assert.equal(cache.complete(pan.ticket, JSON.parse(JSON.stringify(response()))).stationGeometry, original);
});

test("newer views, changed dataset/feed scope, and disabling cannot be overwritten by old responses", () => {
  const cache = new StationGeometryCache();
  const initial = cache.begin("api/dataset/feed-a");
  cache.complete(initial.ticket, response());
  const oldPan = cache.begin("api/dataset/feed-a");
  cache.cancel(oldPan.ticket);
  const next = cache.begin("api/dataset/feed-b");
  assert.equal(next.display.stationGeometry, null);
  assert.equal(cache.complete(oldPan.ticket, response()), null);
  assert.equal(cache.fail(oldPan.ticket, "old error"), null);
  const newGeometry = cache.complete(next.ticket, response(115)).stationGeometry;
  assert.equal(newGeometry.features[0].geometry.coordinates[0], 115);
  const disabled = cache.begin(null);
  assert.deepEqual(disabled.display, { stationGeometry: null, stationGeometryStatus: null });
  assert.equal(cache.complete(next.ticket, response()), null);
  assert.equal(cache.begin("different-api/dataset/feed-b").display.stationGeometry, null);
});

test("a complete feed remains covered across camera movement; larger feeds use viewport fallback only", () => {
  const cache = new StationGeometryCache();
  const first = cache.begin("hk");
  cache.complete(first.ticket, response(), true);
  cache.cancel(first.ticket);
  assert.equal(cache.coversFeed("hk"), true);
  assert.equal(cache.coversFeed("other-feed"), false);
  const large = cache.begin("large-feed");
  assert.equal(cache.coversFeed("large-feed"), false);
  cache.markTruncated(large.ticket);
  cache.complete(large.ticket, response(), false);
  assert.equal(cache.needsViewport("large-feed"), true);
  assert.equal(cache.coversFeed("large-feed"), false);
  cache.begin("different-api");
  assert.equal(cache.needsViewport("different-api"), false);
});
