import assert from "node:assert/strict";
import test from "node:test";
import { fmtTransitTime, fmtValue } from "../src/ui/format.ts";
import { transitTimeContext } from "../src/api/time.ts";

const context = {
  agency_timezone: "Europe/Amsterdam",
  time_origin_unix_s: Date.parse("2026-05-10T22:00:00Z") / 1000,
};

test("timetable timestamps show the date and clock time after midnight", () => {
  assert.equal(fmtValue("departure_s", 32 * 3600, context), "12/05/2026, 08:00:00 GMT+2");
  assert.equal(fmtValue("summary.arrival_s", 32 * 3600 + 900, context), "12/05/2026, 08:15:00 GMT+2");
  assert.equal(fmtValue("start_s", 32 * 3600, context), "12/05/2026, 08:00:00 GMT+2");
  assert.equal(fmtValue("travel_time_s", 900, context), "15 min (900)");
  assert.equal(fmtValue("start_s", 900), "15 min (900)");
});

test("overnight timestamps retain the day offset without a feed context", () => {
  assert.equal(fmtTransitTime(32 * 3600), "08:00:00 (+1 day)");
  assert.equal(fmtTransitTime(48 * 3600), "00:00:00 (+2 days)");
  assert.equal(fmtTransitTime(NaN, context), "–");
});

test("repeated clock times at the DST change show different offsets", () => {
  const autumn = { ...context, time_origin_unix_s: Date.parse("2026-10-24T22:00:00Z") / 1000 };
  assert.equal(fmtTransitTime(9000, autumn), "25/10/2026, 02:30:00 GMT+2");
  assert.equal(fmtTransitTime(12600, autumn), "25/10/2026, 02:30:00 GMT+1");
  const spring = { ...context, time_origin_unix_s: Date.parse("2026-03-28T23:00:00Z") / 1000 };
  assert.equal(fmtTransitTime(9000, spring), "29/03/2026, 03:30:00 GMT+2");
});

test("JSON responses and exported GeoJSON retain their feed's time context", () => {
  for (const value of [
    { result: { time_context: context } },
    { service: context },
    { type: "FeatureCollection", metadata: context },
    { type: "Feature", properties: { ...context, departure_s: 115200 } },
  ]) assert.deepEqual(transitTimeContext(value), context);
  assert.equal(transitTimeContext({ result: { summary: { total_travel_time_s: 900 } } }), undefined);
});
