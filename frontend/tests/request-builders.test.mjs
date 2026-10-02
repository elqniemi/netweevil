import assert from "node:assert/strict";
import test from "node:test";
import { TOOL_BY_ID, fieldDefaults, liveTrafficBody, tripStop } from "../src/tools/registry.ts";
import { formatClock, parseClock, stopLetter } from "../src/tools/clock.ts";
import { heightFeatures, tripFeatures } from "../src/geo/trip.ts";
import { extractFeatures } from "../src/geo/features.ts";
import { accessLabel } from "../src/geo/network.ts";
import { niceTicks } from "../src/ui/RouteExtras.tsx";

const route = { id: "r1", origin: { id: "a", lon: 6.5665, lat: 53.2194 }, destination: { id: "b", lon: 6.5, lat: 53.2 }, vias: [] };

function context(toolId, form = {}, points = {}) {
  const tool = TOOL_BY_ID[toolId];
  return {
    tool,
    ctx: {
      form: { ...fieldDefaults(tool), ...form },
      points: (slot) => points[slot] ?? [],
      routes: [{ route, index: 0 }],
      profileId: "car",
      feedId: null,
      engineMode: "auto",
      service: null,
      viewportBbox: null,
      viewportZoom: null,
    },
  };
}

function build(toolId, form, points) {
  const { tool, ctx } = context(toolId, form, points);
  const ready = tool.ready(ctx);
  if (ready) return { ready };
  return tool.buildEach ? tool.buildEach(ctx, route, 0) : tool.build(ctx);
}

test("clock parsing accepts HH:MM, HH:MM:SS and seconds", () => {
  assert.equal(parseClock("08:30"), 30600);
  assert.equal(parseClock("8:05:09"), 29109);
  assert.equal(parseClock("27:00"), 97200);
  assert.equal(parseClock("3600"), 3600);
  assert.equal(parseClock(""), undefined);
  assert.equal(parseClock(undefined), undefined);
  assert.throws(() => parseClock("8:75"), /not a valid time/);
  assert.throws(() => parseClock("soon"), /not a time/);
  assert.equal(formatClock(30600), "08:30");
  assert.equal(formatClock(29109), "08:05:09");
  assert.equal(formatClock(undefined), "");
  assert.deepEqual([0, 25, 26, 27, 701].map(stopLetter), ["A", "Z", "AA", "AB", "ZZ"]);
});

test("traffic is omitted on auto and sent otherwise, for route, OD, matrix and service area", () => {
  const origins = [{ id: "o", lon: 6.56, lat: 53.21 }];
  const destinations = [{ id: "d", lon: 6.6, lat: 53.2 }];
  const timed = { departure_time: "2026-10-05T08:15:00+02:00" };
  assert.equal(build("route", timed).request.traffic, undefined);
  assert.equal(build("route", { ...timed, traffic: "time_dependent" }).request.traffic, "time_dependent");
  assert.equal(build("od", { ...timed, traffic: "historical" }).request.traffic, "historical");
  const matrix = build("matrix", { ...timed, traffic: "off" }, { origins, destinations });
  assert.equal(matrix.request.origins.traffic, "off");
  assert.equal(matrix.request.destinations.traffic, "off");
  assert.equal(build("service_area", { ...timed, traffic: "historical" }, { origins }).request.traffic, "historical");
  // Batches cannot use the exact time-dependent engine.
  for (const id of ["od", "matrix", "service_area"]) {
    const options = TOOL_BY_ID[id].fields.find((f) => f.key === "traffic").options;
    assert.equal(options.some((o) => o.value === "time_dependent"), false, id);
  }
});

test("route returns carry admin_areas and elevation only when asked", () => {
  const plain = build("route", {}).request.returns;
  assert.equal("admin_areas" in plain, false);
  assert.equal("elevation" in plain, false);
  const both = build("route", { admin_areas: true, elevation: true }).request.returns;
  assert.equal(both.admin_areas, true);
  assert.equal(both.elevation, true);
});

test("service areas send inbound direction and every polygon option", () => {
  const body = build(
    "service_area",
    { direction: "inbound", denoise: 0.5, min_part_area_m2: 20000, min_hole_area_m2: 5000, simplification_tolerance_m: 10 },
    { origins: [{ id: "o", lon: 6.56, lat: 53.21 }] },
  );
  assert.equal(body.request.direction, "inbound");
  assert.deepEqual(body.request.polygon, { hull_aggressiveness: 1, simplification_tolerance_m: 10, denoise: 0.5, min_part_area_m2: 20000, min_hole_area_m2: 5000 });
  const defaults = build("service_area", {}, { origins: [{ id: "o", lon: 6.56, lat: 53.21 }] });
  assert.deepEqual(defaults.request.polygon, { hull_aggressiveness: 1 });
  assert.equal(defaults.request.direction, "outbound");
});

test("trip requests carry mode, clock departure, service times, windows and optimizer options", () => {
  const stops = [
    { id: "depot", lon: 6.5665, lat: 53.2194 },
    { id: "s1", lon: 6.58, lat: 53.21, service_s: 300, earliest_s: 32400, latest_s: 36000 },
    { id: "s2", lon: 6.55, lat: 53.23, latest_s: 39600 },
  ];
  assert.match(build("trip", {}, { stops: stops.slice(0, 1) }).ready, /at least 2 stops/);
  const body = build("trip", { mode: "fixed_start", departure: "08:30", service_s: 120, allow_late: true, seed: 7, exact_max_stops: 12 }, { stops });
  assert.equal(body.profile_id, "car");
  const request = body.request;
  assert.equal(request.mode, "fixed_start");
  assert.equal(request.departure_s, 30600);
  assert.equal(request.allow_late, true);
  assert.deepEqual(request.optimizer, { seed: 7, exact_max_stops: 12 });
  assert.equal(request.returns.geometry, "full");
  assert.deepEqual(request.stops[0], { point: { id: "depot", lon: 6.5665, lat: 53.2194 }, service_s: 120 });
  assert.deepEqual(request.stops[1], { point: { id: "s1", lon: 6.58, lat: 53.21 }, service_s: 300, time_window: { earliest_s: 32400, latest_s: 36000 } });
  assert.deepEqual(request.stops[2].time_window, { earliest_s: 0, latest_s: 39600 });
  const defaults = build("trip", {}, { stops: stops.slice(0, 2).map(({ id, lon, lat }) => ({ id, lon, lat })) }).request;
  assert.equal(defaults.mode, "roundtrip");
  assert.equal(defaults.departure_s, 28800);
  assert.equal("optimizer" in defaults, false);
  assert.equal("service_s" in defaults.stops[0], false);
  assert.throws(() => tripStop({ id: "x", lon: 0, lat: 0, earliest_s: 100, latest_s: 50 }, 0), /closes before it opens/);
  assert.throws(() => build("trip", { exact_max_stops: 19 }, { stops }), /at most 18/);
});

test("vehicle parameters are sent only when filled, on route, OD, matrix, service area and trip", () => {
  const origins = [{ id: "o", lon: 6.56, lat: 53.21 }];
  const destinations = [{ id: "d", lon: 6.6, lat: 53.2 }];
  const stops = [...origins, ...destinations];
  const form = { vehicle_height_m: 3.4, vehicle_weight_t: "12", vehicle_hazmat: "true", vehicle_trailers: 0 };
  const expected = { height_m: 3.4, weight_t: 12, hazmat: true, trailers: 0 };
  assert.deepEqual(build("route", form).request.vehicle, expected);
  assert.deepEqual(build("od", form).request.vehicle, expected);
  const matrix = build("matrix", form, { origins, destinations }).request;
  assert.deepEqual(matrix.origins.vehicle, expected);
  assert.equal("vehicle" in matrix.destinations, false);
  assert.deepEqual(build("service_area", form, { origins }).request.vehicle, expected);
  assert.deepEqual(build("trip", form, { stops }).request.vehicle, expected);

  // Empty fields keep the profile's vehicle: no block at all.
  assert.equal("vehicle" in build("route", {}).request, false);
  assert.equal("vehicle" in build("trip", { vehicle_height_m: "" }, { stops }).request, false);
  assert.deepEqual(build("route", { vehicle_hazmat: "false", vehicle_axle_load_t: 8 }).request.vehicle, { axle_load_t: 8, hazmat: false });
  assert.throws(() => build("route", { vehicle_width_m: 0 }), /width m must be greater than 0/);
  assert.throws(() => build("od", { vehicle_trailers: 1.5 }), /whole number/);
  for (const toolId of ["route", "od", "matrix", "service_area", "trip"]) {
    const groups = new Set(TOOL_BY_ID[toolId].fields.map((field) => field.group));
    assert.ok(groups.has("Vehicle"), toolId);
  }
});

test("height requests send [lon, lat] points, range for profiles and resampling", () => {
  const probe = [{ id: "p1", lon: 114.1455, lat: 22.2759 }, { id: "p2", lon: 114.1699, lat: 22.2793 }];
  assert.deepEqual(build("height", {}, { probe }), { points: [[114.1455, 22.2759], [114.1699, 22.2793]] });
  assert.deepEqual(build("height", { probe_mode: "profile", resample_distance_m: 25 }, { probe }), { points: [[114.1455, 22.2759], [114.1699, 22.2793]], range: true, resample_distance_m: 25 });
  assert.match(build("height", { probe_mode: "profile" }, { probe: probe.slice(0, 1) }).ready, /at least 2/);
  assert.equal(TOOL_BY_ID.height.usesProfile, false);
});

test("live traffic bodies close, slow or clear each distinct way once", () => {
  const ways = [
    { id: "w1", lon: 6.56, lat: 53.21, way_id: 4463602 },
    { id: "w2", lon: 6.561, lat: 53.211, way_id: 4463602 },
    { id: "w3", lon: 6.57, lat: 53.22, way_id: 6348460 },
  ];
  assert.deepEqual(liveTrafficBody({ action: "close", traffic_direction: "both", ttl_s: 1800 }, ways), {
    updates: [
      { feature_id: 4463602, closed: true, ttl_s: 1800 },
      { feature_id: 6348460, closed: true, ttl_s: 1800 },
    ],
    replace: false,
  });
  assert.deepEqual(liveTrafficBody({ action: "slow", speed_kph: 12, traffic_direction: "forward", ttl_s: 600, replace: true }, ways.slice(2)), {
    updates: [{ feature_id: 6348460, direction: "forward", speed_kph: 12, ttl_s: 600 }],
    replace: true,
  });
  assert.deepEqual(liveTrafficBody({ action: "clear", ttl_s: 600 }, ways.slice(2)).updates, [{ feature_id: 6348460, clear: true }]);
  assert.throws(() => liveTrafficBody({ action: "slow", speed_kph: 0 }, ways), /speed above 0/);
  const { tool, ctx } = context("traffic", {}, { traffic_ways: [{ id: "u", lon: 6.5, lat: 53.2 }] });
  assert.match(tool.ready(ctx), /Looking up/);
  assert.equal(TOOL_BY_ID.traffic.path, "/v1/traffic/live?wait=true");
});

test("trip responses get legs in visit order and one numbered badge per stop", () => {
  const body = { request: { stops: [{ point: { id: "a", lon: 6.5, lat: 53.2 } }, { point: { id: "b", lon: 6.6, lat: 53.2 } }, { point: { id: "c", lon: 6.55, lat: 53.25 } }] } };
  const leg = (from, to) => ({ route_id: `leg_${from}_${to}`, summary: {}, geometry: [[body.request.stops[from].point.lon, 53.2], [body.request.stops[to].point.lon, 53.21]] });
  const data = {
    result: {
      mode: "roundtrip",
      stop_order: [0, 2, 1, 0],
      legs: [leg(0, 2), leg(2, 1), leg(1, 0)],
      visits: [0, 2, 1, 0].map((stop_index, i) => ({ stop_index, point_id: "abc"[stop_index], arrival_s: i * 100, wait_s: 0, departure_s: i * 100, lateness_s: stop_index === 1 ? 30 : 0 })),
    },
  };
  const features = tripFeatures(extractFeatures(data), body, data).features;
  const legs = features.filter((f) => f.properties._kind === "legs");
  assert.deepEqual(legs.map((f) => f.properties.leg), [1, 2, 3]);
  assert.ok(legs.every((f) => f.properties._leg_count === 3));
  const badges = features.filter((f) => f.properties._kind === "trip_stops");
  // The closing visit back at the start is not repeated.
  assert.deepEqual(badges.map((f) => [f.properties._label, f.properties.point_id]), [["1", "a"], ["2", "c"], ["3", "b"]]);
  assert.deepEqual(badges[1].geometry.coordinates, [6.55, 53.25]);
  assert.equal(badges[2].properties._late, 1);
});

test("height responses become labelled samples and a profile line", () => {
  const points = heightFeatures({ points: [[114.1, 22.3], [114.2, 22.3]] }, { result: { heights: [12.34, null] } }).features;
  assert.deepEqual(points.map((f) => f.properties._label), ["12.3 m", "no data"]);
  assert.deepEqual(points[0].geometry.coordinates, [114.1, 22.3, 12.34]);
  const profile = heightFeatures(
    { points: [[114.1, 22.3], [114.2, 22.3]], range: true, resample_distance_m: 5000 },
    { result: { range_heights: [[0, 5], [5000, 7.5], [10000, 9]], shape: [[114.1, 22.3], [114.15, 22.3], [114.2, 22.3]] } },
  ).features;
  assert.equal(profile[0].properties._kind, "height_line");
  assert.deepEqual(profile.filter((f) => f.properties._kind === "heights").map((f) => f.properties.distance_m), [0, 5000, 10000]);
});

test("network access labels cover the new travel modes", () => {
  const motor = { access_car: true, access_taxi: true, access_bus: true, access_hgv: true, access_motorcycle: true, access_moped: true, access_bicycle: true, access_foot: false };
  assert.equal(accessLabel(motor), "motor+bike");
  assert.equal(accessLabel({ ...motor, access_hgv: false }), "car+taxi+bus+motorcycle+moped+bike");
  assert.equal(accessLabel({ access_car: true, access_bicycle: false, access_foot: true }), "car+foot");
  assert.equal(accessLabel({ access_bus: true, access_taxi: true }), "taxi+bus");
});

test("axis ticks are clean numbers", () => {
  assert.deepEqual(niceTicks(0, 10, 4), [0, 2.5, 5, 7.5, 10]);
  assert.deepEqual(niceTicks(12, 87, 3), [25, 50, 75]);
  assert.deepEqual(niceTicks(5, 5), [5]);
});
