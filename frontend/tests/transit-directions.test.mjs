import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { TOOL_BY_ID, fieldDefaults, requestPath } from "../src/tools/registry.ts";
import { extractFeatures, styleFeatures } from "../src/geo/features.ts";
import { extractTransitDirections, TransitDirectionsList } from "../src/ui/TransitDirectionsList.tsx";

const geometry = [[114.16, 22.28, -12], [114.1601, 22.2801, -8]];
const response = { result: { time_context: { agency_timezone: "Asia/Hong_Kong", time_origin_unix_s: Date.UTC(2026, 8, 9, 16) / 1000 }, legs: [{ leg_type: "access", geometry }] }, directions: [
  { sequence: 1, kind: "access", instruction: "Walk to Admiralty platform 5.", departure_s: 30600, arrival_s: 30620, duration_s: 20, location: geometry[0], walking_maneuvers: [{ instruction: "Take the lift to the platform level.", location: [114.16, 22.28], distance_m: 4, edge_time_s: 8 }] },
  { sequence: 2, kind: "board", instruction: "Board the East Rail Line toward Lo Wu.", departure_s: 30620, arrival_s: 30620, duration_s: 0, platform: "5", route_short_name: "EAL", headsign: "Lo Wu", location: geometry[1] },
] };

test("transit directions shares route request semantics including exact XYZ and mode exclusions", () => {
  const route = { id: "r", origin: { id: "a", lon: 114.16, lat: 22.28, z: -12 }, destination: { id: "b", lon: 114.17, lat: 22.29, z: 0 }, vias: [] };
  const tool = TOOL_BY_ID.transit_directions;
  const context = { form: { ...fieldDefaults(tool), transit_modes: ["subway"], walking_geometry: "network", pedestrian_profile_id: "hk_foot" }, feedId: "hk", routes: [{ route, index: 0 }] };
  assert.ok(tool.fields.every(field => TOOL_BY_ID.transit_route.fields.includes(field)));
  assert.equal(tool.fields.some(field => field.key === "include_geometry"), false);
  const routeContext = { ...context, form: { ...context.form, include_geometry: true, include_stops: true, include_stop_segments: true } };
  assert.deepEqual(tool.buildEach(context, route, 0), TOOL_BY_ID.transit_route.buildEach(routeContext, route, 0));
  assert.equal(tool.buildEach(context, route, 0).request.destination.z, 0);
  assert.equal(requestPath(tool, "geojson"), "/v1/transit-directions");
});

test("readable journey instructions include platform, feed-local times and walking facilities", () => {
  const html = renderToStaticMarkup(createElement(TransitDirectionsList, { run: { response } }));
  assert.match(html, /Journey directions/);
  assert.match(html, /Walk to Admiralty platform 5/);
  assert.match(html, /Board the East Rail Line toward Lo Wu/);
  assert.match(html, /Platform 5/);
  assert.match(html, /08:30:00/);
  assert.match(html, /GMT\+8/);
  assert.match(html, /Take the lift to the platform level/);
  assert.equal(extractTransitDirections(response).length, 2);
});

test("directions reference the existing XYZ route without adding duplicate map geometry", () => {
  const extracted = extractFeatures(response);
  assert.equal(extracted.features.length, 1);
  assert.deepEqual(extracted.features[0].geometry.coordinates, geometry);
  assert.deepEqual(styleFeatures(extractFeatures(response), { tool: "transit_directions" }), styleFeatures(extractFeatures(response), { tool: "transit_route" }));
  assert.deepEqual(extractTransitDirections(null), []);
  assert.match(renderToStaticMarkup(createElement(TransitDirectionsList, { run: { response: { directions: [] } } })), /No journey directions/);
});
