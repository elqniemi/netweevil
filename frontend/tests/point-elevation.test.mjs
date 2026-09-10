import assert from "node:assert/strict";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { getState, moveRouteEnd, placeRoutePoint, setState, updatePoint, updateVia } from "../src/state/store.ts";
import { currentPlan } from "../src/state/run.ts";
import { ToolPanel } from "../src/ui/ToolPanel.tsx";

const origin = { id: "a", lon: 114.1185, lat: 22.3733, z: 13.33 };
const destination = { id: "b", lon: 114.1607, lat: 22.2813, z: -11.98 };
const via = { id: "v", lon: 114.15, lat: 22.3, z: -5, kind: "break" };

function withPoints(run) {
  const previous = getState();
  const previousWindow = globalThis.window;
  globalThis.window = { setTimeout };
  try {
    setState({
      tool: "transit_route", feedId: "hk", activeRoute: 0, activeEnd: "destination",
      routes: [{ id: "r", origin: { ...origin }, destination: { ...destination }, vias: [{ ...via }] }],
      points: { origins: [{ ...origin }] }, form: {}, raw: {},
    });
    run();
  } finally {
    setState(previous);
    globalThis.window = previousWindow;
  }
}

test("dragged endpoints and replacement map clicks do not submit stale platform elevations", () => withPoints(() => {
  moveRouteEnd(0, "origin", origin.lon, origin.lat);
  assert.equal(getState().routes[0].origin.z, 13.33);
  moveRouteEnd(0, "origin", 114.2115, 22.4152);
  assert.equal(Object.hasOwn(getState().routes[0].origin, "z"), false);
  assert.equal(placeRoutePoint(114.1441, 22.2879), "destination");
  assert.equal(Object.hasOwn(getState().routes[0].destination, "z"), false);
  for (const tool of ["transit_route", "transit_directions"]) {
    const plan = currentPlan(tool);
    assert.equal(plan.error, null);
    const request = plan.requests[0].body.request;
    assert.equal(Object.hasOwn(request.origin, "z"), false);
    assert.equal(Object.hasOwn(request.destination, "z"), false);
    assert.deepEqual([request.origin.lon, request.origin.lat], [114.2115, 22.4152]);
  }
}));

test("analysis points retain heights for metadata and unchanged positions but accept explicit new elevations", () => withPoints(() => {
  updatePoint("origins", 0, { id: "renamed", weight: 2 });
  updatePoint("origins", 0, { lon: origin.lon, lat: origin.lat });
  assert.equal(getState().points.origins[0].z, 13.33);
  updatePoint("origins", 0, { lon: 114.2, z: -7.5 });
  assert.equal(getState().points.origins[0].z, -7.5);
  updatePoint("origins", 0, { lat: 22.4 });
  assert.equal(Object.hasOwn(getState().points.origins[0], "z"), false);
  assert.equal(getState().points.origins[0].id, "renamed");
}));

test("vias clear stale heights on horizontal movement and preserve explicitly supplied heights", () => withPoints(() => {
  updateVia(0, 0, { kind: "through", id: "changed", lon: via.lon });
  assert.equal(getState().routes[0].vias[0].z, -5);
  updateVia(0, 0, { lon: 114.2, z: 0 });
  assert.equal(getState().routes[0].vias[0].z, 0);
  updateVia(0, 0, { lat: 22.4 });
  assert.equal(Object.hasOwn(getState().routes[0].vias[0], "z"), false);
  assert.equal(getState().routes[0].vias[0].kind, "through");
}));

test("fixed endpoint heights remain visible and clearable in the flat map view", () => withPoints(() => {
  setState({ mapStyle: { ...getState().mapStyle, view3d: false }, settingsOpen: true });
  const render = () => renderToStaticMarkup(createElement(ToolPanel, { service: null }));
  assert.match(render(), /altitude m/);
  assert.match(render(), /Moving a marker clears its fixed elevation/);
  moveRouteEnd(0, "origin", 114.21, 22.41);
  moveRouteEnd(0, "destination", 114.14, 22.28);
  assert.doesNotMatch(render(), /altitude m/);
  setState({ mapStyle: { ...getState().mapStyle, view3d: true } });
  assert.match(render(), /altitude m/);
}));
