import type { StyleSpecification } from "maplibre-gl";
import type { Basemap } from "../state/store";

// Key-free basemaps. Positron is a vector style from OpenFreeMap; the others
// are raster fallbacks that work without any tokens.
export function basemapStyle(basemap: Basemap): string | StyleSpecification {
  switch (basemap) {
    case "positron":
      return "https://tiles.openfreemap.org/styles/positron";
    case "dark":
      return rasterStyle(
        ["https://a.basemaps.cartocdn.com/dark_all/{z}/{x}/{y}.png", "https://b.basemaps.cartocdn.com/dark_all/{z}/{x}/{y}.png"],
        "© OpenStreetMap contributors © CARTO",
      );
    case "osm":
      return rasterStyle(["https://tile.openstreetmap.org/{z}/{x}/{y}.png"], "© OpenStreetMap contributors");
  }
}

function rasterStyle(tiles: string[], attribution: string): StyleSpecification {
  return {
    version: 8,
    sources: { base: { type: "raster", tiles, tileSize: 256, attribution, maxzoom: 19 } },
    layers: [{ id: "base", type: "raster", source: "base" }],
  };
}

export const BASEMAP_LABELS: Record<Basemap, string> = {
  positron: "Positron",
  dark: "Dark",
  osm: "OSM",
};
