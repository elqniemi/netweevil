import type { Map, RasterDEMSourceSpecification } from "maplibre-gl";
import { joinUrl } from "../api/client";
import type { TerrainSourceInfo } from "../api/types";

export const TERRAIN_SOURCE = "netweevil-terrain-dem";
export const TERRAIN_HILLSHADE = "netweevil-terrain-hillshade";

export function terrainTileError(error: Error & { status?: number }): string {
  return error.status === 404 || /\b404\b/.test(error.message)
    ? "Some visible tiles have no DEM coverage. Covered terrain remains visible."
    : `Terrain tiles could not load: ${error.message}`;
}

export function selectedTerrain(sources: TerrainSourceInfo[], id: string | null): TerrainSourceInfo | null {
  return sources.find(source => source.id === id) ?? sources[0] ?? null;
}

export function terrainSourceSpec(source: TerrainSourceInfo, apiBase: string): RasterDEMSourceSpecification {
  return {
    type: "raster-dem", encoding: source.encoding, tileSize: source.tile_size,
    tiles: source.tiles.map(tile => /^https?:\/\//.test(tile) ? tile : joinUrl(apiBase, tile)),
    minzoom: source.minzoom, maxzoom: source.maxzoom, bounds: source.bounds,
    attribution: source.attribution,
  };
}

interface TerrainOptions {
  view3d: boolean;
  enabled: boolean;
  exaggeration: number;
  source: TerrainSourceInfo | null;
  apiBase: string;
}

type TerrainMap = Pick<Map, "getSource" | "getLayer" | "getTerrain" | "getStyle" | "addSource" | "removeSource" | "addLayer" | "removeLayer" | "setTerrain">;

/** Terrain scales source metres exactly like the separate XYZ overlay.
 * It never samples or adds ground height to a surveyed route coordinate. */
export function applyTerrain(map: TerrainMap, options: TerrainOptions): void {
  const desired = options.enabled && options.view3d && options.source ? terrainSourceSpec(options.source, options.apiBase) : null;
  const existing = map.getSource(TERRAIN_SOURCE);
  const changed = existing && JSON.stringify(existing.serialize()) !== JSON.stringify(desired);
  if (!desired || changed) {
    if (map.getTerrain()?.source === TERRAIN_SOURCE) map.setTerrain(null);
    if (map.getLayer(TERRAIN_HILLSHADE)) map.removeLayer(TERRAIN_HILLSHADE);
    if (existing) map.removeSource(TERRAIN_SOURCE);
  }
  if (!desired) return;
  if (!map.getSource(TERRAIN_SOURCE)) map.addSource(TERRAIN_SOURCE, desired);
  if (!map.getLayer(TERRAIN_HILLSHADE)) {
    const before = map.getStyle().layers.find(layer => layer.type === "symbol" || layer.id === "results-fill")?.id;
    map.addLayer({
      id: TERRAIN_HILLSHADE, type: "hillshade", source: TERRAIN_SOURCE,
      paint: { "hillshade-exaggeration": 0.25, "hillshade-shadow-color": "#475569", "hillshade-highlight-color": "#ffffff" },
    }, before);
  }
  const terrain = map.getTerrain();
  if (terrain?.source !== TERRAIN_SOURCE || terrain.exaggeration !== options.exaggeration) {
    map.setTerrain({ source: TERRAIN_SOURCE, exaggeration: options.exaggeration });
  }
}
