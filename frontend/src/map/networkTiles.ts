// Routing-network debug overlay: the selected profile's routable edges as
// vector tiles from /v1/tiles, coloured by road class.

import type { ExpressionSpecification, Map } from "maplibre-gl";
import { joinUrl } from "../api/client";
import { ROAD_CLASS_COLORS } from "../geo/network";

export const NETWORK_TILE_SOURCE = "netweevil-network-tiles";
const NETWORK_TILE_LAYER = "netweevil-network-tiles-line";

export function networkTileUrl(apiBase: string, profileId: string | null): string {
  const query = profileId ? `?profile_id=${encodeURIComponent(profileId)}` : "";
  return joinUrl(apiBase, `/v1/tiles/{z}/{x}/{y}.mvt${query}`);
}

/** Adds, replaces or removes the overlay; it sits below result layers. */
export function applyNetworkTiles(map: Map, url: string | null, beforeLayer?: string) {
  const source = map.getSource(NETWORK_TILE_SOURCE) as { tiles?: string[] } | undefined;
  if ((source?.tiles?.[0] ?? null) === url) return;
  if (map.getLayer(NETWORK_TILE_LAYER)) map.removeLayer(NETWORK_TILE_LAYER);
  if (source) map.removeSource(NETWORK_TILE_SOURCE);
  if (!url) return;
  map.addSource(NETWORK_TILE_SOURCE, { type: "vector", tiles: [url], minzoom: 5, maxzoom: 16 });
  const color = ["match", ["get", "road_class"], ...Object.entries(ROAD_CLASS_COLORS).flat(), ROAD_CLASS_COLORS.unknown] as unknown as ExpressionSpecification;
  map.addLayer(
    {
      id: NETWORK_TILE_LAYER,
      type: "line",
      source: NETWORK_TILE_SOURCE,
      "source-layer": "network",
      paint: {
        "line-color": color,
        "line-width": ["interpolate", ["linear"], ["zoom"], 10, 0.6, 16, 2.5],
        "line-opacity": 0.75,
      },
    },
    beforeLayer && map.getLayer(beforeLayer) ? beforeLayer : undefined,
  );
}
