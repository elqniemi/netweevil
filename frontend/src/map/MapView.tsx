import { useEffect, useRef } from "react";
import maplibregl, { type Map as MapLibreMap, type MapMouseEvent } from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
import { basemapStyle } from "./basemaps";
import { addPoint, getPoints, getState, nextPointId, setState, updatePoint, useStore, type ToolId } from "../state/store";
import { TOOL_BY_ID } from "../tools/registry";
import { EMPTY_COLLECTION, SLOT_COLORS, featureBounds, type Feature, type FeatureCollection } from "../geo/features";
import type { ServiceInfo } from "../api/types";

const RESULT_SOURCE = "results";
const INPUT_SOURCE = "inputs";
const SIM_AGENTS_SOURCE = "sim-agents";
const SIM_EDGES_SOURCE = "sim-edges";

interface Props {
  service: ServiceInfo | null;
}

/** Builds the input-point feature collection for the active tool. */
function inputFeatures(tool: ToolId): FeatureCollection {
  const def = TOOL_BY_ID[tool];
  const features: Feature[] = [];
  let id = 0;
  for (const slot of def.slots) {
    const points = getPoints(tool, slot.id);
    points.forEach((p, index) => {
      const color = SLOT_COLORS[slot.id] ?? "#7B3FB8";
      const isOd = slot.id === "od";
      const label = isOd ? `${Math.floor(index / 2) + 1}${index % 2 === 0 ? "A" : "B"}` : slot.max === 1 ? slot.label[0] : String(index + 1);
      features.push({
        type: "Feature",
        id: id++,
        geometry: { type: "Point", coordinates: [p.lon, p.lat] },
        properties: {
          slot: slot.id,
          index,
          label,
          color: isOd ? (index % 2 === 0 ? "#C8102E" : "#1F5FBF") : color,
          kind: p.kind ?? "",
        },
      });
    });
    // Connect OD pairs and waypoint sequences with thin guide lines.
    if (slot.id === "od") {
      for (let i = 0; i + 1 < points.length; i += 2) {
        features.push({
          type: "Feature",
          id: id++,
          geometry: {
            type: "LineString",
            coordinates: [
              [points[i].lon, points[i].lat],
              [points[i + 1].lon, points[i + 1].lat],
            ],
          },
          properties: { slot: slot.id, guide: true, color: "#6B7A87" },
        });
      }
    }
    if (slot.id === "zone" && points.length >= 3) {
      const ring = points.map((p) => [p.lon, p.lat]);
      ring.push(ring[0]);
      features.push({ type: "Feature", id: id++, geometry: { type: "Polygon", coordinates: [ring] }, properties: { slot: slot.id, zone: true, color: "#0E8A6A" } });
    }
  }
  return { type: "FeatureCollection", features };
}

function addLayers(map: MapLibreMap) {
  if (map.getSource(RESULT_SOURCE)) return;
  map.addSource(RESULT_SOURCE, { type: "geojson", data: EMPTY_COLLECTION, promoteId: undefined });
  map.addSource(INPUT_SOURCE, { type: "geojson", data: EMPTY_COLLECTION });
  map.addSource(SIM_EDGES_SOURCE, { type: "geojson", data: EMPTY_COLLECTION });
  map.addSource(SIM_AGENTS_SOURCE, { type: "geojson", data: EMPTY_COLLECTION });

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
    paint: { "line-color": ["get", "_color"], "line-width": ["get", "_width"], "line-opacity": ["get", "_opacity"] },
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
    paint: { "circle-radius": 11, "circle-color": "#ffffff", "circle-opacity": 0.95 },
  });
  map.addLayer({
    id: "inputs-point",
    type: "circle",
    source: INPUT_SOURCE,
    filter: ["==", ["geometry-type"], "Point"],
    paint: { "circle-radius": 9, "circle-color": ["get", "color"] },
  });
  map.addLayer({
    id: "inputs-label",
    type: "symbol",
    source: INPUT_SOURCE,
    filter: ["==", ["geometry-type"], "Point"],
    layout: {
      "text-field": ["get", "label"],
      "text-size": 10,
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

function currentBbox(map: MapLibreMap): [number, number, number, number] {
  const b = map.getBounds();
  return [b.getWest(), b.getSouth(), b.getEast(), b.getNorth()];
}

export function MapView({ service }: Props) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const mapRef = useRef<MapLibreMap | null>(null);
  const styleReady = useRef(false);
  const basemap = useStore((s) => s.basemap);
  const tool = useStore((s) => s.tool);
  const points = useStore((s) => s.points);
  const result = useStore((s) => s.result);
  const fitRequest = useStore((s) => s.fitRequest);
  const agentFeatures = useStore((s) => s.simulation.agentFeatures);
  const edgeFeatures = useStore((s) => s.simulation.edgeFeatures);
  const showEdges = useStore((s) => s.simulation.showEdges);
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

    const syncViewport = () => setState({ viewport: { bbox: currentBbox(map) } });
    map.on("moveend", syncViewport);

    map.on("style.load", () => {
      addLayers(map);
      styleReady.current = true;
      setSource(map, INPUT_SOURCE, inputFeatures(getState().tool));
      setSource(map, RESULT_SOURCE, getState().result?.features ?? EMPTY_COLLECTION);
      setSource(map, SIM_AGENTS_SOURCE, getState().simulation.agentFeatures ?? EMPTY_COLLECTION);
      setSource(map, SIM_EDGES_SOURCE, getState().simulation.edgeFeatures ?? EMPTY_COLLECTION);
      syncViewport();
    });

    // --- Click to add points, drag to move them. ---
    let dragging: { slot: string; index: number } | null = null;

    map.on("mousedown", "inputs-point", (e) => {
      const feature = e.features?.[0];
      if (!feature) return;
      e.preventDefault();
      dragging = { slot: String(feature.properties?.slot), index: Number(feature.properties?.index) };
      map.getCanvas().style.cursor = "grabbing";
      map.dragPan.disable();
    });
    map.on("mousemove", (e: MapMouseEvent) => {
      if (!dragging) return;
      updatePoint(getState().tool, dragging.slot, dragging.index, { lon: e.lngLat.lng, lat: e.lngLat.lat });
    });
    const endDrag = () => {
      if (!dragging) return;
      dragging = null;
      map.getCanvas().style.cursor = "";
      map.dragPan.enable();
    };
    map.on("mouseup", endDrag);
    map.on("mouseout", endDrag);

    map.on("contextmenu", "inputs-point", (e) => {
      const feature = e.features?.[0];
      if (!feature) return;
      e.preventDefault();
      const state = getState();
      const slot = String(feature.properties?.slot);
      const index = Number(feature.properties?.index);
      const current = getPoints(state.tool, slot);
      setState((prev) => ({ points: { ...prev.points, [`${state.tool}:${slot}`]: current.filter((_, i) => i !== index) } }));
    });

    map.on("click", (e) => {
      if (dragging) return;
      const hits = map.queryRenderedFeatures(e.point, { layers: ["inputs-point"] });
      if (hits.length) return;
      const state = getState();
      const def = TOOL_BY_ID[state.tool];
      if (!def.slots.length) return;
      const activeSlot = state.activeSlot[state.tool] ?? def.slots[0].id;
      const slot = def.slots.find((s) => s.id === activeSlot) ?? def.slots[0];
      const point = { id: nextPointId(slot.id), lon: e.lngLat.lng, lat: e.lngLat.lat, kind: slot.kinds ? "break" : undefined };
      addPoint(state.tool, slot.id, point, slot.max);
      // Auto-advance origin -> destination for two-slot tools.
      if (slot.max === 1) {
        const next = def.slots.find((s) => s.id !== slot.id && s.max === 1 && getPoints(state.tool, s.id).length === 0);
        if (next) setState((prev) => ({ activeSlot: { ...prev.activeSlot, [state.tool]: next.id } }));
      }
    });

    map.on("mouseenter", "inputs-point", () => (map.getCanvas().style.cursor = "grab"));
    map.on("mouseleave", "inputs-point", () => (map.getCanvas().style.cursor = ""));

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
    const hasPoints = Object.values(getState().points).some((p) => p.length > 0);
    if (!hasPoints) map.fitBounds([bounds.min_lon, bounds.min_lat, bounds.max_lon, bounds.max_lat], { padding: 40, duration: 0 });
  }, [service]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !styleReady.current) return;
    setSource(map, INPUT_SOURCE, inputFeatures(tool));
  }, [tool, points]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !styleReady.current) return;
    setSource(map, RESULT_SOURCE, result?.features ?? EMPTY_COLLECTION);
  }, [result]);

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
    const collection = getState().result?.features ?? getState().simulation.edgeFeatures;
    if (!collection) return;
    const bounds = featureBounds(collection);
    if (!bounds) return;
    map.fitBounds(bounds, { padding: { top: 60, bottom: 120, left: 60, right: 60 }, duration: 500, maxZoom: 16 });
  }, [fitRequest]);

  return <div ref={containerRef} className="map" />;
}
