import assert from "node:assert/strict";
import test from "node:test";
import {
  cellValueColor,
  clampResolution,
  csvHeader,
  distanceMetres,
  drawnCellAt,
  h3EdgeMetres,
  importBody,
  importColumns,
  isScratchLayer,
  isValidLayerId,
  numericCsvColumns,
  planStroke,
  pointsCsv,
  sampleStroke,
  splitCsvLine,
  strokePreview,
  styleSurgeCells,
  styleValueCells,
  surgeColor,
  valueLegend,
} from "../src/geo/cells.ts";
import { TOOL_BY_ID } from "../src/tools/registry.ts";

const settings = { column: "demand", op: "set", value: 2, resolution: 8, mode: "brush" };

function hexagon(cell, resolution, [lon, lat], r = 0.004, props = {}) {
  const ring = Array.from({ length: 7 }, (_, i) => {
    const a = ((i % 6) * Math.PI) / 3;
    return [lon + r * Math.cos(a), lat + r * Math.sin(a)];
  });
  return { type: "Feature", geometry: { type: "Polygon", coordinates: [ring] }, properties: { cell, resolution, ...props } };
}

test("the Cells tool is registered with its own input kind", () => {
  const tool = TOOL_BY_ID.cells;
  assert.equal(tool.input, "cells");
  assert.equal(tool.group, "cells");
  assert.deepEqual(tool.slots, []);
});

test("value colours run from light at the minimum to dark at the maximum", () => {
  const low = cellValueColor(0, 0, 10);
  const high = cellValueColor(10, 0, 10);
  const luminance = (hex) => parseInt(hex.slice(1, 3), 16) + parseInt(hex.slice(3, 5), 16) + parseInt(hex.slice(5, 7), 16);
  assert.ok(luminance(low) > luminance(cellValueColor(5, 0, 10)));
  assert.ok(luminance(cellValueColor(5, 0, 10)) > luminance(high));
  assert.equal(cellValueColor(null, 0, 10), null);
  assert.equal(cellValueColor(undefined, 0, 10), null);
  assert.equal(cellValueColor(Number.NaN, 0, 10), null);
  // A column with one distinct value (or no stats) still colours its cells.
  assert.equal(cellValueColor(4, 4, 4), high);
  assert.equal(cellValueColor(4, null, null), high);
  assert.equal(cellValueColor(-5, 0, 10), low, "values below the range clamp");
});

test("surge colours are neutral at multiplier 1 and strongest at the maximum", () => {
  assert.equal(surgeColor(1), surgeColor(0.5));
  assert.notEqual(surgeColor(1), surgeColor(2));
  assert.equal(surgeColor(3), surgeColor(5));
  assert.equal(surgeColor(null), null);
});

test("cells without a value in the column are drawn as outline only", () => {
  const collection = {
    type: "FeatureCollection",
    features: [hexagon("a", 8, [0, 0], 0.004, { demand: 3 }), hexagon("b", 8, [1, 1], 0.004, { supply: 1 })],
  };
  const styled = styleValueCells(collection, "demand", { name: "demand", count: 1, sum: 3, min: 3, max: 3 });
  assert.ok(styled.features[0].properties._fill > 0);
  assert.equal(styled.features[1].properties._fill, 0);
  assert.equal(styled.features[0].properties.demand, 3, "original properties stay for hover");
  assert.equal(collection.features[0].properties._fill, undefined, "input is not mutated");
  const noColumn = styleValueCells(collection, "", null);
  assert.ok(noColumn.features.every((f) => f.properties._fill === 0));
  const surge = styleSurgeCells({ type: "FeatureCollection", features: [hexagon("a", 8, [0, 0], 0.004, { multiplier: 1 }), hexagon("b", 8, [1, 1], 0.004, { multiplier: 2.5 })] });
  assert.ok(surge.features[1].properties._fill > surge.features[0].properties._fill);
});

test("legend needs a column with a min and max", () => {
  assert.equal(valueLegend("demand", null), null);
  assert.equal(valueLegend("demand", { name: "demand", count: 0, sum: 0, min: null, max: null }), null);
  const legend = valueLegend("demand", { name: "demand", count: 2, sum: 12, min: 2, max: 10 });
  assert.deepEqual(legend.labels, ["2", "6", "10"]);
  assert.equal(legend.colors.length, 5);
});

test("a click paints the single cell under the pointer with a point bbox", () => {
  const plan = planStroke([[6.5, 53.2]], false, settings);
  assert.deepEqual(plan, { kind: "request", body: { column: "demand", op: "set", value: 2, bbox: [6.5, 53.2, 6.5, 53.2], resolution: 8 } });
  const clear = planStroke([[6.5, 53.2], [6.5001, 53.2]], false, { ...settings, op: "clear" });
  assert.equal(clear.kind, "request");
  assert.equal("value" in clear.body, false, "clear sends no value");
  assert.deepEqual(clear.body.bbox, [6.5001, 53.2, 6.5001, 53.2]);
});

test("an area drag sends one bbox request spanning the drag", () => {
  const plan = planStroke([[6.6, 53.1], [6.55, 53.15], [6.5, 53.2]], true, { ...settings, mode: "area" });
  assert.deepEqual(plan.body.bbox, [6.5, 53.1, 6.6, 53.2]);
  assert.equal(plan.body.resolution, 8);
});

test("a brush drag resolves drawn hexagons locally and leaves the rest to the API", () => {
  const drawn = [hexagon("881f1d4887fffff", 8, [6.5, 53.2]), hexagon("other-res", 9, [6.53, 53.2])];
  const plan = planStroke([[6.5, 53.2], [6.53, 53.2]], true, settings, drawn);
  assert.equal(plan.kind, "brush");
  assert.deepEqual(plan.cells, ["881f1d4887fffff"], "each drawn cell once, only of the paint resolution");
  assert.ok(plan.unresolved.length > 0);
  assert.equal(plan.resolution, 8);
  assert.deepEqual(plan.base, { column: "demand", op: "set", value: 2 });
  for (const p of plan.unresolved) assert.equal(drawnCellAt(p, drawn, 8), null);
});

test("stroke sampling keeps points no further apart than the spacing", () => {
  const stroke = [[6.5, 53.2], [6.52, 53.2], [6.52, 53.21]];
  const spacing = h3EdgeMetres(9) / 3;
  const samples = sampleStroke(stroke, spacing);
  assert.deepEqual(samples[0], stroke[0]);
  assert.deepEqual(samples.at(-1), stroke.at(-1));
  for (let i = 1; i < samples.length; i++) assert.ok(distanceMetres(samples[i - 1], samples[i]) <= spacing + 1e-6);
  assert.equal(sampleStroke(stroke, 0.01, 100).length, 100, "long strokes are capped");
  assert.deepEqual(sampleStroke([], 10), []);
  assert.ok(h3EdgeMetres(5) > h3EdgeMetres(11));
});

test("stroke preview draws a path for the brush and a rectangle for an area", () => {
  assert.equal(strokePreview([[0, 0], [1, 1]], "brush").features[0].geometry.type, "LineString");
  assert.equal(strokePreview([[0, 0], [1, 1]], "area").features[0].geometry.type, "Polygon");
  assert.equal(strokePreview([[0, 0]], "area").features[0].geometry.type, "Point");
  assert.equal(strokePreview([], "brush").features.length, 0);
  assert.equal(pointsCsv([[1, 2]]), "lon,lat,n\n1.0000000,2.0000000,1");
});

test("CSV headers honour quotes, a BOM and CRLF line ends", () => {
  assert.deepEqual(splitCsvLine('a, "b,c" ,"say ""hi"""'), ["a", "b,c", 'say "hi"']);
  assert.deepEqual(csvHeader("﻿cell,demand,supply\r\n8a,1,2\r\n"), ["cell", "demand", "supply"]);
  assert.deepEqual(csvHeader(""), []);
  assert.deepEqual(numericCsvColumns("cell,name,demand,supply\n8a,x,1,\n8b,y,,2.5\n"), ["demand", "supply"]);
});

test("imports send column specs only for a non-sum aggregation", () => {
  const csv = "id,lon,lat,trips,name,fare\n1,6.5,53.2,3,a,2.5\n2,6.6,53.3,4,b,3\n";
  const base = { layerId: "trips", points: true, resolution: 9, lonColumn: "lon", latColumn: "lat", aggregation: "sum" };
  assert.equal(importColumns(csv, base), undefined);
  assert.deepEqual(importBody(csv, base), { layer_id: "trips", merge: true, csv, resolution: 9 });
  const mean = importBody(csv, { ...base, aggregation: "mean" });
  assert.deepEqual(mean.columns, [
    { source: "id", aggregation: "mean" },
    { source: "trips", aggregation: "mean" },
    { source: "fare", aggregation: "mean" },
  ]);
  const custom = "x,y,cell,v\n6.5,53.2,8a,1\n";
  const renamed = importBody(custom, { ...base, lonColumn: "x", latColumn: "y", aggregation: "max" });
  assert.equal(renamed.lon_column, "x");
  assert.equal(renamed.lat_column, "y");
  assert.deepEqual(renamed.columns, [{ source: "v", aggregation: "max" }]);
  const cellTable = importBody("cell,v\n8a,1\n", { ...base, points: false });
  assert.equal("resolution" in cellTable, false);
  assert.equal(importColumns("cell\n8a\n", { ...base, aggregation: "count" }), undefined, "no numeric columns falls back to the API default");
});

test("layer ids, scratch layers and resolutions", () => {
  assert.ok(isValidLayerId("demand_2026-01"));
  assert.ok(!isValidLayerId("demand 2026"));
  assert.ok(!isValidLayerId(""));
  assert.ok(isScratchLayer("console-brush-abc-1"));
  assert.ok(!isScratchLayer("demand"));
  assert.equal(clampResolution(3), 5);
  assert.equal(clampResolution(14), 11);
  assert.equal(clampResolution("9.4"), 9);
  assert.equal(clampResolution("x", 7), 7);
});
