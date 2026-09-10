import assert from "node:assert/strict";
import test from "node:test";
import { getState, resetForm, setState } from "../src/state/store.ts";
import { DEFAULT_TRANSIT_MODES, transitModeSelection } from "../src/state/transitModes.ts";
import { TRANSIT_ANALYSIS_TOOLS } from "../src/state/transitDefaults.ts";

const service = {
  default_profile_id: "hk_foot",
  loaded_profiles: [{ profile_id: "hk_foot", mode: "foot" }],
  loaded_transit_feeds: [{ agency_timezone: "Asia/Hong_Kong", service_start_date: "2026-09-10", transfer_profile_ids: ["hk_foot"] }],
};

test("resetting either Hong Kong transit tool restores network access and feed-local date", () => {
  const previous = getState();
  const previousWindow = globalThis.window;
  globalThis.window = { setTimeout };
  try {
    for (const tool of TRANSIT_ANALYSIS_TOOLS) {
      setState({ service, form: { [tool]: { datetime: "2020-01-01", street_access: "straight_line", transit_modes: [] }, route: { route_id: "keep" } }, raw: { [tool]: "{}", route: "{\"keep\":true}" } });
      resetForm(tool);
      const next = getState();
      assert.deepEqual(next.form[tool], {
        street_access: "network", walking_geometry: "network", include_stop_segments: true, pedestrian_profile_id: "hk_foot",
        transfer_profile_id: "hk_foot", datetime: "2026-09-10T08:30:00+08:00",
      });
      assert.deepEqual(transitModeSelection(next.form[tool].transit_modes), DEFAULT_TRANSIT_MODES);
      assert.equal(next.raw[tool], undefined);
      assert.deepEqual(next.form.route, { route_id: "keep" });
      assert.equal(next.raw.route, '{"keep":true}');
    }
  } finally {
    setState(previous);
    globalThis.window = previousWindow;
  }
});
