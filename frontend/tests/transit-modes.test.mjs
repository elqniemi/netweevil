import assert from "node:assert/strict";
import test from "node:test";
import { DEFAULT_TRANSIT_MODES, TRANSIT_MODE_OPTIONS, selectedModes, transitModeSelection } from "../src/state/transitModes.ts";
import { TOOL_BY_ID, fieldDefaults } from "../src/tools/registry.ts";
import { TRANSIT_ANALYSIS_TOOLS } from "../src/state/transitDefaults.ts";

const route = { id: "hk", origin: { id: "a", lon: 114.1185, lat: 22.3733 }, destination: { id: "b", lon: 114.1607, lat: 22.2813 }, vias: [] };
function context(tool, selection) {
  return { form: { ...fieldDefaults(tool), transit_modes: selection }, points: () => [route.origin], routes: [{ route, index: 0 }], feedId: "hong_kong_multimodal", profileId: null, service: null };
}

test("legacy text accepts complete enums and rejects partially typed subway before sending", () => {
  assert.deepEqual(transitModeSelection("subway, bus, subway"), ["subway", "bus"]);
  for (const value of ["s", "su", "subw", ["bus", "su"]]) {
    assert.throws(() => transitModeSelection(value), /Choose modes using the checkboxes/);
  }
});

test("blank legacy field retains API defaults, while an empty checkbox selection allows none", () => {
  const all = TRANSIT_MODE_OPTIONS.map((o) => o.value);
  assert.deepEqual(transitModeSelection(undefined), DEFAULT_TRANSIT_MODES);
  assert.deepEqual(transitModeSelection(""), DEFAULT_TRANSIT_MODES);
  assert.equal(DEFAULT_TRANSIT_MODES.length, 9);
  assert.equal(DEFAULT_TRANSIT_MODES.includes("air"), false);
  assert.equal(DEFAULT_TRANSIT_MODES.includes("other"), false);
  assert.deepEqual(transitModeSelection(all), all);
  assert.deepEqual(transitModeSelection([]), []);
});

for (const id of TRANSIT_ANALYSIS_TOOLS) {
  test(`${id} sends exactly the allowed modes, including explicit none`, () => {
    const tool = TOOL_BY_ID[id];
    assert.equal(tool.fields.find((f) => f.key === "transit_modes").type, "multiselect");
    const field = tool.fields.find((f) => f.key === "transit_modes");
    assert.deepEqual(field.default, DEFAULT_TRANSIT_MODES);
    assert.deepEqual(selectedModes("", field.options.map((option) => option.value), field.default), DEFAULT_TRANSIT_MODES);
    const build = (selection) => {
      const ctx = context(tool, selection);
      return tool.buildEach ? tool.buildEach(ctx, route, 0) : tool.build(ctx);
    };
    assert.deepEqual(build(["subway"]).request.modes.transit, ["subway"]);
    assert.deepEqual(build(["bus", "ferry"]).request.modes.transit, ["bus", "ferry"]);
    assert.deepEqual(build([]).request.modes.transit, []);
    assert.equal(build(["subway"]).request.modes.max_endpoint_snap_distance_m, 50);
    const ctx = context(tool, ["subway"]);
    ctx.form.max_endpoint_snap_distance_m = 0;
    const exact = tool.buildEach ? tool.buildEach(ctx, route, 0) : tool.build(ctx);
    assert.equal(exact.request.modes.max_endpoint_snap_distance_m, 0);
    assert.throws(() => build("su"), /Unknown transit mode/);
  });
}
