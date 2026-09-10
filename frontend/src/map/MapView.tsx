import { useEffect, useRef } from "react";
import maplibregl, { type Map as MapLibreMap, type MapMouseEvent } from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
import { basemapStyle } from "./basemaps";
import {
  addPoint,
  appendEditorStop,
  getPoints,
  getState,
  moveEditorStop,
  moveRouteEnd,
  nextEditorStopId,
  nextPointId,
  placeRoutePoint,
  removeEditorStop,
  removePoint,
  removeVia,
  setActiveRoute,
  setEditor,
  setRouteEnd,
  setState,
  updatePoint,
  updateVia,
  useStore,
  type EditorState,
  type RouteEnd,
  type RouteInput,
  type ToolId,
} from "../state/store";
import { TOOL_BY_ID } from "../tools/registry";
import { EMPTY_COLLECTION, PALETTE, SLOT_COLORS, featureBounds, routeColor, type Feature, type FeatureCollection } from "../geo/features";
import type { FeedStop, ServiceInfo } from "../api/types";
import { apiRequest } from "../api/client";
import { lineColor } from "../ui/editorShared";

const RESULT_SOURCE = "results";
const INPUT_SOURCE = "inputs";
const SIM_AGENTS_SOURCE = "sim-agents";
const SIM_EDGES_SOURCE = "sim-edges";
const EDITOR_SOURCE = "editor";
const EDITOR_LAYERS = ["editor-base-stop", "editor-stop"];

/** Map features of the GTFS editor: base feed stops, scenario stops and lines. */
function editorFeatures(editor: EditorState, active: boolean): FeatureCollection {
  const features: Feature[] = [];
  if (!active || !editor.scenario) return { type: "FeatureCollection", features };
  const scenario = editor.scenario;
  const usedByActive = new Set<string>(editor.activeLine !== null ? (scenario.lines[editor.activeLine]?.stops.map((s) => s.stop_id) ?? []) : []);
  const scenarioIds = new Set(scenario.stops.map((s) => s.stop_id));
  const lookup = new Map<string, { lon: number; lat: number; name: string }>();
  for (const s of editor.baseStops) lookup.set(s.stop_id, s);
  for (const s of scenario.stops) lookup.set(s.stop_id, s);
  let id = 0;
  for (const s of editor.baseStops) {
    if (scenarioIds.has(s.stop_id)) continue;
    features.push({ type: "Feature", id: id++, geometry: { type: "Point", coordinates: [s.lon, s.lat] }, properties: { kind: "base_stop", stop_id: s.stop_id, name: s.name, used: usedByActive.has(s.stop_id) ? 1 : 0, selected: editor.selectedStop === s.stop_id ? 1 : 0 } });
  }
  scenario.lines.forEach((line, index) => {
    const coords = line.stops.map((s) => lookup.get(s.stop_id)).filter((p): p is { lon: number; lat: number; name: string } => !!p).map((p) => [p.lon, p.lat]);
    if (coords.length >= 2) {
      features.push({ type: "Feature", id: id++, geometry: { type: "LineString", coordinates: coords }, properties: { kind: "line", index, color: lineColor(line, index), active: index === editor.activeLine ? 1 : 0, name: line.short_name || line.line_id } });
    }
  });
  for (const s of scenario.stops) {
    features.push({ type: "Feature", id: id++, geometry: { type: "Point", coordinates: [s.lon, s.lat] }, properties: { kind: "scn_stop", stop_id: s.stop_id, name: s.name, used: usedByActive.has(s.stop_id) ? 1 : 0, selected: editor.selectedStop === s.stop_id ? 1 : 0 } });
  }
  // Sequence labels along the active line.
  if (editor.activeLine !== null && scenario.lines[editor.activeLine]) {
    scenario.lines[editor.activeLine].stops.forEach((s, position) => {
      const p = lookup.get(s.stop_id);
      if (p) features.push({ type: "Feature", id: id++, geometry: { type: "Point", coordinates: [p.lon, p.lat] }, properties: { kind: "seq", label: String(position + 1), stop_id: s.stop_id } });
    });
  }
  return { type: "FeatureCollection", features };
}

interface Props {
  service: ServiceInfo | null;
}

/** What an input marker on the map refers to, carried in feature properties. */
interface MarkerRef {
  kind: "slot" | "route";
  slot?: string;
  index: number;
  route?: number;
  end?: RouteEnd | "via";
}

function markerRef(props: Record<string, unknown> | null | undefined): MarkerRef | null {
  if (!props || (props.kind !== "slot" && props.kind !== "route")) return null;
  return {
    kind: props.kind as MarkerRef["kind"],
    slot: typeof props.slot === "string" ? props.slot : undefined,
    index: Number(props.index ?? 0),
    route: typeof props.route === "number" ? props.route : undefined,
    end: typeof props.end === "string" ? (props.end as MarkerRef["end"]) : undefined,
  };
}

/** Builds the input-point feature collection: routes for route tools, slots otherwise. */
function inputFeatures(tool: ToolId, routes: RouteInput[]): FeatureCollection {
  const def = TOOL_BY_ID[tool];
  const features: Feature[] = [];
  let id = 0;
  if (def.input === "routes") {
    const multi = routes.length > 1;
    routes.forEach((route, r) => {
      const color = multi ? routeColor(r) : PALETTE.crimson;
      const suffix = multi ? String(r + 1) : "";
      if (def.usesVias) {
        route.vias.forEach((via, v) => {
          features.push({
            type: "Feature",
            id: id++,
            geometry: { type: "Point", coordinates: [via.lon, via.lat] },
            properties: { kind: "route", route: r, end: "via", index: v, label: String(v + 1), color, small: 1 },
          });
        });
      }
      if (route.origin) {
        features.push({
          type: "Feature",
          id: id++,
          geometry: { type: "Point", coordinates: [route.origin.lon, route.origin.lat] },
          properties: { kind: "route", route: r, end: "origin", index: 0, label: `A${suffix}`, color, small: 0 },
        });
      }
      if (route.destination) {
        features.push({
          type: "Feature",
          id: id++,
          geometry: { type: "Point", coordinates: [route.destination.lon, route.destination.lat] },
          properties: { kind: "route", route: r, end: "destination", index: 0, label: `B${suffix}`, color: multi ? color : PALETTE.blue, small: 0 },
        });
      }
    });
  }
  for (const slot of def.slots) {
    const points = getPoints(slot.id);
    points.forEach((p, index) => {
      const color = SLOT_COLORS[slot.id] ?? "#7B3FB8";
      const label = slot.max === 1 ? slot.label[0] : String(index + 1);
      features.push({
        type: "Feature",
        id: id++,
        geometry: { type: "Point", coordinates: [p.lon, p.lat] },
        properties: { kind: "slot", slot: slot.id, index, label, color, small: 0 },
      });
    });
    if (slot.id === "zone" && points.length >= 3) {
      const ring = points.map((p) => [p.lon, p.lat]);
      ring.push(ring[0]);
      features.push({ type: "Feature", id: id++, geometry: { type: "Polygon", coordinates: [ring] }, properties: { slot: slot.id, zone: true, color: "#0E8A6A" } });
    }
  }
  return { type: "FeatureCollection", features };
}

/** Opacity multiplier that dims every run except the selected one. */
function selectionExpression(selected: number | null): maplibregl.ExpressionSpecification | number {
  if (selected === null) return 1;
  return ["case", ["==", ["get", "_run"], selected], 1, 0.18];
}

function addLayers(map: MapLibreMap) {
  if (map.getSource(RESULT_SOURCE)) return;
  map.addSource(RESULT_SOURCE, { type: "geojson", data: EMPTY_COLLECTION });
  map.addSource(INPUT_SOURCE, { type: "geojson", data: EMPTY_COLLECTION });
  map.addSource(SIM_EDGES_SOURCE, { type: "geojson", data: EMPTY_COLLECTION });
  map.addSource(SIM_AGENTS_SOURCE, { type: "geojson", data: EMPTY_COLLECTION });
  map.addSource(EDITOR_SOURCE, { type: "geojson", data: EMPTY_COLLECTION });

  map.addLayer({
    id: "results-fill",
    type: "fill",
    source: RESULT_SOURCE,
    filter: ["any", ["==", ["geometry-type"], "Polygon"], ["==", ["geometry-type"], "MultiPolygon"]],
    paint: { "fill-color": ["get", "_color"], "fill-opacity": ["get", "_fill"] },
  });
  map.addLayer({
    id: "results-fill-outline",
    type: "line",
    source: RESULT_SOURCE,
    filter: ["any", ["==", ["geometry-type"], "Polygon"], ["==", ["geometry-type"], "MultiPolygon"]],
    paint: { "line-color": ["get", "_color"], "line-width": ["get", "_width"], "line-opacity": 0.9 },
  });
  map.addLayer({
    id: "results-line-casing",
    type: "line",
    source: RESULT_SOURCE,
    filter: ["all", ["any", ["==", ["geometry-type"], "LineString"], ["==", ["geometry-type"], "MultiLineString"]], ["==", ["get", "_dash"], 0]],
    layout: { "line-cap": "round", "line-join": "round", "line-sort-key": ["get", "_sort"] },
    paint: { "line-color": "#ffffff", "line-width": ["+", ["get", "_width"], 2.5], "line-opacity": 0.85 },
  });
  map.addLayer({
    id: "results-line",
    type: "line",
    source: RESULT_SOURCE,
    filter: ["all", ["any", ["==", ["geometry-type"], "LineString"], ["==", ["geometry-type"], "MultiLineString"]], ["==", ["get", "_dash"], 0]],
    layout: { "line-cap": "round", "line-join": "round", "line-sort-key": ["get", "_sort"] },
    paint: { "line-color": ["get", "_color"], "line-width": ["get", "_width"], "line-opacity": ["get", "_opacity"], "line-offset": ["coalesce", ["get", "_offset"], 0] },
  });
  map.addLayer({
    id: "results-line-dashed",
    type: "line",
    source: RESULT_SOURCE,
    filter: ["all", ["any", ["==", ["geometry-type"], "LineString"], ["==", ["geometry-type"], "MultiLineString"]], ["==", ["get", "_dash"], 1]],
    layout: { "line-cap": "round", "line-join": "round" },
    paint: { "line-color": ["get", "_color"], "line-width": ["get", "_width"], "line-opacity": ["get", "_opacity"], "line-dasharray": [1.5, 2] },
  });
  map.addLayer({
    id: "results-point",
    type: "circle",
    source: RESULT_SOURCE,
    filter: ["==", ["geometry-type"], "Point"],
    paint: {
      "circle-radius": ["get", "_radius"],
      "circle-color": ["get", "_color"],
      "circle-opacity": ["get", "_opacity"],
      "circle-stroke-color": "#ffffff",
      "circle-stroke-width": 1.5,
    },
  });

  map.addLayer({
    id: "sim-edges",
    type: "line",
    source: SIM_EDGES_SOURCE,
    layout: { "line-cap": "round" },
    paint: { "line-color": ["get", "_color"], "line-width": ["get", "_width"], "line-opacity": 0.85 },
  });
  map.addLayer({
    id: "sim-agents",
    type: "circle",
    source: SIM_AGENTS_SOURCE,
    paint: {
      "circle-radius": ["interpolate", ["linear"], ["zoom"], 8, 1.5, 12, 3, 16, 6],
      "circle-color": ["get", "_color"],
      "circle-opacity": 0.9,
    },
  });

  // GTFS editor: base stops (grey), lines, scenario stops (accent) and sequence labels.
  map.addLayer({
    id: "editor-line-casing",
    type: "line",
    source: EDITOR_SOURCE,
    filter: ["==", ["get", "kind"], "line"],
    layout: { "line-cap": "round", "line-join": "round" },
    paint: { "line-color": "#ffffff", "line-width": ["case", ["==", ["get", "active"], 1], 8, 5], "line-opacity": 0.9 },
  });
  map.addLayer({
    id: "editor-line",
    type: "line",
    source: EDITOR_SOURCE,
    filter: ["==", ["get", "kind"], "line"],
    layout: { "line-cap": "round", "line-join": "round" },
    paint: { "line-color": ["get", "color"], "line-width": ["case", ["==", ["get", "active"], 1], 5, 3], "line-opacity": ["case", ["==", ["get", "active"], 1], 1, 0.7] },
  });
  map.addLayer({
    id: "editor-base-stop",
    type: "circle",
    source: EDITOR_SOURCE,
    filter: ["==", ["get", "kind"], "base_stop"],
    paint: {
      "circle-radius": ["case", ["==", ["get", "used"], 1], 6, ["interpolate", ["linear"], ["zoom"], 10, 2, 14, 4.5]],
      "circle-color": ["case", ["==", ["get", "used"], 1], "#0e7c86", "#ffffff"],
      "circle-stroke-color": ["case", ["==", ["get", "selected"], 1], "#16232e", "#6b7a87"],
      "circle-stroke-width": ["case", ["==", ["get", "selected"], 1], 2.5, 1.2],
      "circle-opacity": 0.95,
    },
  });
  map.addLayer({
    id: "editor-stop",
    type: "circle",
    source: EDITOR_SOURCE,
    filter: ["==", ["get", "kind"], "scn_stop"],
    paint: {
      "circle-radius": 7,
      "circle-color": ["case", ["==", ["get", "used"], 1], "#0e7c86", "#7B3FB8"],
      "circle-stroke-color": ["case", ["==", ["get", "selected"], 1], "#16232e", "#ffffff"],
      "circle-stroke-width": ["case", ["==", ["get", "selected"], 1], 3, 2],
    },
  });
  map.addLayer({
    id: "editor-seq",
    type: "symbol",
    source: EDITOR_SOURCE,
    filter: ["==", ["get", "kind"], "seq"],
    layout: { "text-field": ["get", "label"], "text-size": 10, "text-font": ["Noto Sans Bold"], "text-offset": [0, -1.4], "text-allow-overlap": true },
    paint: { "text-color": "#16232e", "text-halo-color": "#ffffff", "text-halo-width": 1.5 },
  });
  map.addLayer({
    id: "editor-stop-label",
    type: "symbol",
    source: EDITOR_SOURCE,
    filter: ["any", ["==", ["get", "kind"], "scn_stop"], ["all", ["==", ["get", "kind"], "base_stop"], ["==", ["get", "used"], 1]]],
    minzoom: 12,
    layout: { "text-field": ["get", "name"], "text-size": 10.5, "text-font": ["Noto Sans Regular"], "text-offset": [0, 1.1], "text-anchor": "top", "text-optional": true },
    paint: { "text-color": "#16232e", "text-halo-color": "#ffffff", "text-halo-width": 1.2 },
  });

  map.addLayer({
    id: "inputs-zone",
    type: "fill",
    source: INPUT_SOURCE,
    filter: ["==", ["geometry-type"], "Polygon"],
    paint: { "fill-color": ["get", "color"], "fill-opacity": 0.12 },
  });
  map.addLayer({
    id: "inputs-guide",
    type: "line",
    source: INPUT_SOURCE,
    filter: ["any", ["==", ["geometry-type"], "LineString"], ["==", ["geometry-type"], "Polygon"]],
    paint: { "line-color": ["get", "color"], "line-width": 1.5, "line-dasharray": [2, 2], "line-opacity": 0.8 },
  });
  map.addLayer({
    id: "inputs-halo",
    type: "circle",
    source: INPUT_SOURCE,
    filter: ["==", ["geometry-type"], "Point"],
    paint: { "circle-radius": ["case", ["==", ["get", "small"], 1], 8, 11], "circle-color": "#ffffff", "circle-opacity": 0.95 },
  });
  map.addLayer({
    id: "inputs-point",
    type: "circle",
    source: INPUT_SOURCE,
    filter: ["==", ["geometry-type"], "Point"],
    paint: { "circle-radius": ["case", ["==", ["get", "small"], 1], 6, 9], "circle-color": ["get", "color"] },
  });
  map.addLayer({
    id: "inputs-label",
    type: "symbol",
    source: INPUT_SOURCE,
    filter: ["==", ["geometry-type"], "Point"],
    layout: {
      "text-field": ["get", "label"],
      "text-size": ["case", ["==", ["get", "small"], 1], 9, 10],
      "text-font": ["Noto Sans Bold"],
      "text-allow-overlap": true,
      "text-ignore-placement": true,
    },
    paint: { "text-color": "#ffffff" },
  });
}

function setSource(map: MapLibreMap, id: string, data: FeatureCollection) {
  const source = map.getSource(id) as maplibregl.GeoJSONSource | undefined;
  if (source) source.setData(data as unknown as Parameters<maplibregl.GeoJSONSource["setData"]>[0]);
}

function applySelection(map: MapLibreMap, selected: number | null) {
  const factor = selectionExpression(selected);
  const mul = (base: maplibregl.ExpressionSpecification): maplibregl.ExpressionSpecification | number =>
    typeof factor === "number" ? base : ["*", base, factor];
  if (map.getLayer("results-line")) map.setPaintProperty("results-line", "line-opacity", mul(["get", "_opacity"]));
  if (map.getLayer("results-line-dashed")) map.setPaintProperty("results-line-dashed", "line-opacity", mul(["get", "_opacity"]));
  if (map.getLayer("results-line-casing")) map.setPaintProperty("results-line-casing", "line-opacity", typeof factor === "number" ? 0.85 : ["*", 0.85, factor]);
  if (map.getLayer("results-point")) map.setPaintProperty("results-point", "circle-opacity", mul(["get", "_opacity"]));
  if (map.getLayer("results-fill")) map.setPaintProperty("results-fill", "fill-opacity", mul(["get", "_fill"]));
}

function applyMarkerScale(map: MapLibreMap, scale: number) {
  if (!map.getLayer("inputs-point")) return;
  map.setPaintProperty("inputs-halo", "circle-radius", ["*", ["case", ["==", ["get", "small"], 1], 8, 11], scale]);
  map.setPaintProperty("inputs-point", "circle-radius", ["*", ["case", ["==", ["get", "small"], 1], 6, 9], scale]);
  map.setLayoutProperty("inputs-label", "text-size", ["*", ["case", ["==", ["get", "small"], 1], 9, 10], Math.max(0.6, scale)]);
}

function currentBbox(map: MapLibreMap): [number, number, number, number] {
  const b = map.getBounds();
  return [b.getWest(), b.getSouth(), b.getEast(), b.getNorth()];
}

/** Whether a map click would place a point (crosshair cursor). */
function placing(): boolean {
  const s = getState();
  const def = TOOL_BY_ID[s.tool];
  if (def.input === "editor") return s.editor.mode === "add" && s.editor.activeLine !== null && !!s.editor.scenario;
  if (def.input === "viewport") return false;
  if (def.input === "routes") {
    const route = s.routes[Math.min(s.activeRoute, s.routes.length - 1)];
    return !route.origin || !route.destination || !!def.usesVias;
  }
  return def.slots.length > 0;
}

export function MapView({ service }: Props) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const mapRef = useRef<MapLibreMap | null>(null);
  const styleReady = useRef(false);
  const basemap = useStore((s) => s.basemap);
  const tool = useStore((s) => s.tool);
  const points = useStore((s) => s.points);
  const routes = useStore((s) => s.routes);
  const activeRoute = useStore((s) => s.activeRoute);
  const result = useStore((s) => s.result);
  const selectedRun = useStore((s) => s.selectedRun);
  const fitRequest = useStore((s) => s.fitRequest);
  const flyRequest = useStore((s) => s.flyRequest);
  const markerScale = useStore((s) => s.mapStyle.markerScale);
  const agentFeatures = useStore((s) => s.simulation.agentFeatures);
  const edgeFeatures = useStore((s) => s.simulation.edgeFeatures);
  const showEdges = useStore((s) => s.simulation.showEdges);
  const editor = useStore((s) => s.editor);
  const fittedRef = useRef(false);

  // Create the map once.
  useEffect(() => {
    if (!containerRef.current || mapRef.current) return;
    const map = new maplibregl.Map({
      container: containerRef.current,
      style: basemapStyle(getState().basemap),
      center: [6.57, 53.22],
      zoom: 11,
      attributionControl: { compact: true },
      // Smooth playback and drag handling matter more than tile fidelity here.
      fadeDuration: 0,
    });
    map.addControl(new maplibregl.NavigationControl({ visualizePitch: false }), "top-right");
    map.addControl(new maplibregl.ScaleControl({ maxWidth: 120 }), "bottom-left");
    mapRef.current = map;
    if (import.meta.env.DEV) (window as unknown as { __netweevilMap: MapLibreMap }).__netweevilMap = map;

    const syncViewport = () => setState({ viewport: { bbox: currentBbox(map), zoom: map.getZoom() } });
    map.on("moveend", syncViewport);

    map.on("style.load", () => {
      addLayers(map);
      styleReady.current = true;
      setSource(map, INPUT_SOURCE, inputFeatures(getState().tool, getState().routes));
      setSource(map, RESULT_SOURCE, getState().result?.features ?? EMPTY_COLLECTION);
      setSource(map, SIM_AGENTS_SOURCE, getState().simulation.agentFeatures ?? EMPTY_COLLECTION);
      setSource(map, SIM_EDGES_SOURCE, getState().simulation.edgeFeatures ?? EMPTY_COLLECTION);
      setSource(map, EDITOR_SOURCE, editorFeatures(getState().editor, getState().tool === "transit_editor"));
      applySelection(map, getState().selectedRun);
      applyMarkerScale(map, getState().mapStyle.markerScale);
      syncViewport();
    });

    // --- Click to add points, drag to move them (routes re-run live while dragging). ---
    let dragging: MarkerRef | null = null;
    let draggingStop: string | null = null;
    const canvas = map.getCanvas();
    const setCursor = (cursor: string) => (canvas.style.cursor = cursor);

    // --- GTFS editor: drag scenario stops, click to add, right-click to remove. ---
    map.on("mousedown", "editor-stop", (e) => {
      const stopId = e.features?.[0]?.properties?.stop_id;
      if (typeof stopId !== "string") return;
      e.preventDefault();
      draggingStop = stopId;
      setCursor("grabbing");
      map.dragPan.disable();
    });
    map.on("mousemove", (e: MapMouseEvent) => {
      if (draggingStop) moveEditorStop(draggingStop, e.lngLat.lng, e.lngLat.lat);
    });
    const endStopDrag = () => {
      if (!draggingStop) return;
      draggingStop = null;
      setCursor(placing() ? "crosshair" : "");
      map.dragPan.enable();
    };
    map.on("mouseup", endStopDrag);
    map.on("mouseout", endStopDrag);
    map.on("contextmenu", (e) => {
      if (getState().tool !== "transit_editor") return;
      const hits = map.queryRenderedFeatures(e.point, { layers: EDITOR_LAYERS.filter((id) => map.getLayer(id)) });
      const stopId = hits[0]?.properties?.stop_id;
      if (typeof stopId !== "string") return;
      e.preventDefault();
      removeEditorStop(stopId);
    });
    map.on("mouseenter", "editor-stop", () => setCursor("grab"));
    map.on("mouseleave", "editor-stop", () => setCursor(placing() ? "crosshair" : ""));
    map.on("mouseenter", "editor-base-stop", () => setCursor("pointer"));
    map.on("mouseleave", "editor-base-stop", () => setCursor(placing() ? "crosshair" : ""));

    map.on("mousedown", "inputs-point", (e) => {
      const ref = markerRef(e.features?.[0]?.properties as Record<string, unknown> | undefined);
      if (!ref) return;
      e.preventDefault();
      dragging = ref;
      if (ref.kind === "route" && ref.route !== undefined && ref.end !== "via") setActiveRoute(ref.route, ref.end);
      setCursor("grabbing");
      map.dragPan.disable();
    });
    map.on("mousemove", (e: MapMouseEvent) => {
      if (!dragging) return;
      const { lng, lat } = e.lngLat;
      if (dragging.kind === "slot" && dragging.slot) updatePoint(dragging.slot, dragging.index, { lon: lng, lat });
      else if (dragging.kind === "route" && dragging.route !== undefined) {
        if (dragging.end === "via") updateVia(dragging.route, dragging.index, { lon: lng, lat });
        else if (dragging.end) moveRouteEnd(dragging.route, dragging.end, lng, lat);
      }
    });
    const endDrag = () => {
      if (!dragging) return;
      dragging = null;
      setCursor(placing() ? "crosshair" : "");
      map.dragPan.enable();
    };
    map.on("mouseup", endDrag);
    map.on("mouseout", endDrag);

    map.on("contextmenu", "inputs-point", (e) => {
      const ref = markerRef(e.features?.[0]?.properties as Record<string, unknown> | undefined);
      if (!ref) return;
      e.preventDefault();
      if (ref.kind === "slot" && ref.slot) removePoint(ref.slot, ref.index);
      else if (ref.kind === "route" && ref.route !== undefined) {
        if (ref.end === "via") removeVia(ref.route, ref.index);
        else if (ref.end) {
          setRouteEnd(ref.route, ref.end, null);
          setActiveRoute(ref.route, ref.end);
        }
      }
    });

    map.on("click", (e) => {
      if (dragging || draggingStop) return;
      const state0 = getState();
      if (state0.tool === "transit_editor") {
        const editor = state0.editor;
        if (!editor.scenario) return;
        const hits = map.queryRenderedFeatures(e.point, { layers: EDITOR_LAYERS.filter((id) => map.getLayer(id)) });
        const stopId = hits[0]?.properties?.stop_id;
        const canAdd = editor.mode === "add" && editor.activeLine !== null;
        if (typeof stopId === "string") {
          setEditor({ selectedStop: stopId });
          if (canAdd) appendEditorStop(stopId);
          return;
        }
        if (!canAdd) return;
        const id = nextEditorStopId();
        const line = editor.scenario.lines[editor.activeLine!];
        const name = `${line?.short_name || line?.line_id || "Stop"} ${line ? line.stops.length + 1 : editor.scenario.stops.length + 1}`;
        appendEditorStop(id, { name, lon: e.lngLat.lng, lat: e.lngLat.lat });
        setEditor({ selectedStop: id });
        return;
      }
      const hits = map.queryRenderedFeatures(e.point, { layers: ["inputs-point"] });
      if (hits.length) {
        const ref = markerRef(hits[0].properties as Record<string, unknown>);
        if (ref?.kind === "route" && ref.route !== undefined && ref.end !== "via") setActiveRoute(ref.route, ref.end);
        return;
      }
      const state = getState();
      const def = TOOL_BY_ID[state.tool];
      if (def.input === "routes") {
        placeRoutePoint(e.lngLat.lng, e.lngLat.lat, !!def.usesVias);
        setCursor(placing() ? "crosshair" : "");
        return;
      }
      if (!def.slots.length) return;
      const activeSlot = state.activeSlot[state.tool] ?? def.slots[0].id;
      const slot = def.slots.find((s) => s.id === activeSlot) ?? def.slots[0];
      const point = { id: nextPointId(slot.id), lon: e.lngLat.lng, lat: e.lngLat.lat };
      addPoint(slot.id, point, slot.max);
      // Auto-advance for single-point slots.
      if (slot.max === 1) {
        const next = def.slots.find((s) => s.id !== slot.id && s.max === 1 && getPoints(s.id).length === 0);
        if (next) setState((prev) => ({ activeSlot: { ...prev.activeSlot, [state.tool]: next.id } }));
      }
    });

    map.on("mouseenter", "inputs-point", () => setCursor("grab"));
    map.on("mouseleave", "inputs-point", () => setCursor(placing() ? "crosshair" : ""));

    // --- Hover info for results. ---
    const hoverLayers = ["results-point", "results-line", "results-line-dashed", "results-fill", "sim-agents", "sim-edges"];
    map.on("mousemove", (e) => {
      if (dragging || !styleReady.current) return;
      const layers = hoverLayers.filter((id) => map.getLayer(id));
      const hits = map.queryRenderedFeatures(e.point, { layers });
      if (!hits.length) {
        if (getState().hoverInfo) setState({ hoverInfo: null });
        return;
      }
      const props = (hits[0].properties ?? {}) as Record<string, unknown>;
      setState({ hoverInfo: { x: e.point.x, y: e.point.y, props } });
    });

    return () => {
      map.remove();
      mapRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Basemap switches replace the style; layers are re-added on style.load.
  useEffect(() => {
    const map = mapRef.current;
    if (!map) return;
    styleReady.current = false;
    map.setStyle(basemapStyle(basemap));
  }, [basemap]);

  // Fit to the dataset bounds once the service is known.
  useEffect(() => {
    const map = mapRef.current;
    const bounds = service?.dataset.topology_bounds;
    if (!map || !bounds || fittedRef.current) return;
    fittedRef.current = true;
    const state = getState();
    const hasPoints = Object.values(state.points).some((p) => p.length > 0) || state.routes.some((r) => r.origin || r.destination);
    if (!hasPoints) map.fitBounds([bounds.min_lon, bounds.min_lat, bounds.max_lon, bounds.max_lat], { padding: 40, duration: 0 });
  }, [service]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !styleReady.current) return;
    setSource(map, INPUT_SOURCE, inputFeatures(tool, routes));
    map.getCanvas().style.cursor = placing() ? "crosshair" : "";
  }, [tool, points, routes, activeRoute]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !styleReady.current) return;
    setSource(map, RESULT_SOURCE, result?.features ?? EMPTY_COLLECTION);
  }, [result]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !styleReady.current) return;
    setSource(map, EDITOR_SOURCE, editorFeatures(editor, tool === "transit_editor"));
    map.getCanvas().style.cursor = placing() ? "crosshair" : "";
  }, [editor, tool]);

  // Base feed stops for the view while editing an overlay scenario.
  const baseFeedId = tool === "transit_editor" ? (editor.scenario?.base_feed_id ?? null) : null;
  const viewport = useStore((s) => s.viewport.bbox);
  const apiBase = useStore((s) => s.apiBase);
  useEffect(() => {
    const map = mapRef.current;
    if (!map || !baseFeedId || !viewport) {
      if (!baseFeedId && getState().editor.baseStops.length) setEditor({ baseStops: [] });
      return;
    }
    if (map.getZoom() < 9.5) {
      if (getState().editor.baseStops.length) setEditor({ baseStops: [] });
      return;
    }
    const controller = new AbortController();
    const [w, s, e, n] = viewport;
    const padX = (e - w) * 0.2;
    const padY = (n - s) * 0.2;
    const bbox = [w - padX, s - padY, e + padX, n + padY].map((v) => v.toFixed(5)).join(",");
    const timer = window.setTimeout(() => {
      void apiRequest<{ stops: FeedStop[] }>(apiBase, `/v1/transit-feeds/${encodeURIComponent(baseFeedId)}/stops?bbox=${bbox}&limit=6000`, { tool: "transit_editor", signal: controller.signal })
        .then((response) => setEditor({ baseStops: response.data.stops }))
        .catch(() => undefined);
    }, 150);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [baseFeedId, viewport, apiBase]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !styleReady.current) return;
    applySelection(map, selectedRun);
  }, [selectedRun]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !styleReady.current) return;
    setSource(map, SIM_AGENTS_SOURCE, agentFeatures ?? EMPTY_COLLECTION);
  }, [agentFeatures]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !styleReady.current) return;
    setSource(map, SIM_EDGES_SOURCE, showEdges ? (edgeFeatures ?? EMPTY_COLLECTION) : EMPTY_COLLECTION);
  }, [edgeFeatures, showEdges]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || fitRequest === 0) return;
    const state = getState();
    const run = state.fitRun;
    const collection = run !== null && state.result?.runs[run] ? state.result.runs[run].features : (state.result?.features ?? state.simulation.edgeFeatures);
    if (!collection) return;
    const bounds = featureBounds(collection);
    if (!bounds) return;
    map.fitBounds(bounds, { padding: { top: 60, bottom: 120, left: 60, right: 60 }, duration: 500, maxZoom: 16 });
  }, [fitRequest]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !styleReady.current) return;
    applyMarkerScale(map, markerScale);
  }, [markerScale]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !flyRequest) return;
    map.flyTo({ center: [flyRequest.lon, flyRequest.lat], zoom: Math.max(map.getZoom(), flyRequest.zoom), duration: 700 });
  }, [flyRequest]);

  return <div ref={containerRef} className="map" />;
}
