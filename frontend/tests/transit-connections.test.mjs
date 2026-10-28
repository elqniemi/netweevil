import assert from "node:assert/strict";
import test from "node:test";
import { extractFeatures, styleFeatures } from "../src/geo/features.ts";
import { addTransitConnections } from "../src/geo/transitConnections.ts";

const request = { request: { origin: { id: "a", lon: 114.2, lat: 22.4 }, destination: { id: "b", lon: 114.15, lat: 22.28, z: -4 } } };
const line = (leg_type, from_id, to_id, geometry) => ({ leg_type, from_id, to_id, geometry });
const gaps = collection => collection.features.filter(f => f.properties._kind === "off_network_connection");

test("access and egress show unsurveyed gaps from saved endpoints, with source XYZ untouched", () => {
  const response = { legs: [line("access", "a", "stop1", [[114.2001, 22.4, 8], [114.21, 22.4, 8]]), line("egress", "stop2", "b", [[114.14, 22.28, -4], [114.1501, 22.28, -4]])] };
  const before = JSON.stringify(response);
  const result = styleFeatures(addTransitConnections(extractFeatures(response), request), { tool: "transit_route" });
  assert.equal(gaps(result).length, 2);
  assert.deepEqual(gaps(result)[0].geometry.coordinates[0], [114.2, 22.4]);
  assert.deepEqual(gaps(result)[1].geometry.coordinates.at(-1), [114.15, 22.28, -4]);
  assert.equal(gaps(result)[0].properties.elevation_known, false);
  assert.equal(gaps(result)[1].properties.elevation_known, true);
  for (const f of gaps(result)) {
    assert.equal(f.properties.routed, false);
    assert.equal(f.properties.name, "Off-network connection (not routed)");
    assert.equal(f.properties._dash, 1);
    assert.ok(f.properties.horizontal_gap_m > 10 && f.properties.horizontal_gap_m < 11);
  }
  assert.equal(JSON.stringify(response), before);
  assert.equal(gaps(addTransitConnections(result, request)).length, 2);
});

test("direct access leg can have two gaps; numerical noise and mismatched endpoint IDs are ignored", () => {
  const direct = extractFeatures({ legs: [line("access", "a", "b", [[114.2001, 22.4, 8], [114.1501, 22.28, -4]])] });
  assert.equal(gaps(addTransitConnections(direct, request)).length, 2);
  assert.equal(gaps(addTransitConnections(direct, { request: { origin: { ...request.request.origin, id: "other" } } })).length, 0);
  const exact = extractFeatures({ legs: [line("access", "a", "b", [[114.2 + 1e-10, 22.4, 8], [114.15, 22.28, -4]])] });
  assert.equal(gaps(addTransitConnections(exact, request)).length, 0);
});

test("vertical-only and single-position egress attachments are still visible", () => {
  const result = addTransitConnections(extractFeatures({ legs: [line("egress", "stop", "b", [[114.15, 22.28, -8]])] }), request);
  assert.equal(gaps(result).length, 1);
  assert.equal(gaps(result)[0].properties.horizontal_gap_m, 0);
  assert.deepEqual(gaps(result)[0].geometry.coordinates, [[114.15, 22.28, -8], [114.15, 22.28, -4]]);
});
