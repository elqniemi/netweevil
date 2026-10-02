import assert from "node:assert/strict";
import test from "node:test";
import { TOOL_BY_ID, fieldDefaults } from "../src/tools/registry.ts";
import { extractDirections, signText } from "../src/ui/DirectionsList.tsx";

const route = { id: "r", origin: { id: "a", lon: 6.56, lat: 53.21 }, destination: { id: "b", lon: 6.6, lat: 53.17 }, vias: [] };

test("directions requests carry language, units, driving side and departure time", () => {
  const tool = TOOL_BY_ID.directions;
  const form = { ...fieldDefaults(tool), language: "zh-HK", units: "imperial", driving_side: "left", departure_time: "2026-09-09T08:00:00+02:00" };
  const body = tool.buildEach({ form, routes: [{ route, index: 0 }] }, route, 0);
  assert.deepEqual(body.directions_options, { language: "zh-HK", units: "imperial", driving_side: "left" });
  assert.equal(body.request.departure_time, "2026-09-09T08:00:00+02:00");
  const defaults = tool.buildEach({ form: fieldDefaults(tool), routes: [{ route, index: 0 }] }, route, 0);
  assert.deepEqual(defaults.directions_options, { language: "en", units: "metric" });
});

test("maneuvers keep lanes and signposts from the response", () => {
  const response = { result: { language: "nl", route: { summary: {} }, maneuvers: [
    { kind: "exit_right", instruction: "Neem afrit 36 rechts richting N7, Haren.", distance_m: 400, edge_time_s: 20,
      signs: { exit_number: ["36"], branch: ["N7"], toward: ["Haren"] },
      lanes: [{ indications: ["through"], valid: false, active: false }, { indications: ["slight_right"], valid: true, active: true }] },
  ] } };
  const directions = extractDirections(response);
  assert.equal(directions.language, "nl");
  assert.equal(directions.maneuvers[0].lanes.length, 2);
  assert.equal(signText(directions.maneuvers[0].signs), "Exit 36 · N7, Haren");
});
