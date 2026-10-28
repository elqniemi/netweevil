import { MapboxOverlay } from "@deck.gl/mapbox";
import { PathLayer, PolygonLayer, ScatterplotLayer, TextLayer } from "@deck.gl/layers";
import { CollisionFilterExtension, PathStyleExtension, type CollisionFilterExtensionProps, type PathStyleExtensionProps } from "@deck.gl/extensions";
import { MapView, type PickingInfo } from "@deck.gl/core";
import type { FeatureCollection } from "../geo/features";
import { elevatedData, elevationColor, type ElevatedPath, type ElevatedPoint, type ElevatedPolygon, type XYZ } from "../geo/elevation";
import type { MapStyle } from "../state/store";

type Datum = ElevatedPath | ElevatedPoint | ElevatedPolygon;

// deck's default far plane stops just beyond the visible ground and clips
// below-datum geometry at street zoom. A multiplier alone remains capped at
// the ground horizon. Override farZ on this separate overlay camera instead;
// a smaller near plane also retains elevated features close to the camera.
// The map camera and XY projection remain unchanged.
export const ELEVATION_MAP_VIEW = new MapView({
  id: "mapbox",
  viewState: { id: "mapbox", nearZ: 0.01, farZ: 10_000 },
});

export function createThreeDOverlay(onHover: (info: { x: number; y: number; props: Record<string, unknown> } | null) => void) {
  const overlay = new MapboxOverlay({
    interleaved: false,
    // deck.gl 9.4 declares DeckProps with ViewsT=null here, although the
    // public MapboxOverlay runtime accepts custom views by their mapbox ID.
    // @ts-expect-error MapboxOverlay's generic view type is fixed to null.
    views: ELEVATION_MAP_VIEW,
    layers: [],
  });
  let cachedCollection: FeatureCollection | null = null;
  let cachedOptions = "";
  let cachedData: ReturnType<typeof elevatedData> | null = null;
  let cachedLabels: Datum[] = [];
  let presented = "";
  let visible = false;
  const pathStyle = new PathStyleExtension({ dash: true, offset: true });
  const labelCollisions = new CollisionFilterExtension();
  return {
    control: overlay,
    update(collection: FeatureCollection, style: MapStyle, selected: number | null) {
      if (!style.view3d) {
        if (visible) overlay.setProps({ layers: [] });
        visible = false;
        return;
      }
      const optionsKey = [style.verticalExaggeration, style.altitudeSlice, style.minAltitude, style.maxAltitude].join(":");
      const presentationKey = `${optionsKey}:${style.elevationColor}:${selected}`;
      if (visible && cachedCollection === collection && presented === presentationKey) return;
      if (cachedCollection !== collection || cachedOptions !== optionsKey || !cachedData) {
        cachedData = elevatedData(collection, style);
        cachedCollection = collection;
        cachedOptions = optionsKey;
        cachedLabels = [...cachedData.polygons, ...cachedData.points].filter(d => d.feature.properties._label);
      }
      visible = true;
      presented = presentationKey;
      const { paths, points, polygons, min, max } = cachedData;
      // Surveyed walking legs are styled as dashed in 2D, but deck's billboard
      // dash shader is unstable when their 3D segments collapse in projection.
      // Keep dashes for planar unsurveyed connectors; render surveyed XYZ paths
      // with the ordinary path shader and without an unused offset extension.
      const useGpuDash = (d: ElevatedPath) => d.feature.properties._dash === 1 && !d.known
        && d.path.every(p => p[2] === d.path[0][2]);
      const position = (d: Datum): XYZ => {
        if ("position" in d) return d.position;
        const coordinates = "path" in d ? d.path : d.polygon[0];
        return coordinates.reduce((sum, p) => [sum[0] + p[0] / coordinates.length, sum[1] + p[1] / coordinates.length, sum[2] + p[2] / coordinates.length], [0, 0, 0]) as XYZ;
      };
      const color = (d: Datum): [number, number, number, number] => {
        const p = d.feature.properties;
        const z = position(d)[2] / style.verticalExaggeration;
        const hex = String(p._color ?? "#1f5fbf");
        const n = parseInt(hex.replace("#", ""), 16);
        const rgb: [number, number, number] = style.elevationColor && p._kind !== "off_network_connection"
          ? d.known ? elevationColor(z, min ?? 0, max ?? 1) : [148, 163, 184]
          : [(n >> 16) & 255, (n >> 8) & 255, n & 255];
        return [...rgb, Math.round(255 * Number(p._opacity ?? 0.9) * (selected !== null && p._run !== selected && !p._reference ? 0.18 : 1))];
      };
      const hover = (info: PickingInfo<Datum>) => {
        if (!info.object) { onHover(null); return; }
        const source = info.object.feature.properties.geometry_elevation_source;
        const display = source === "off_network_connection" ? "Unsurveyed connection; altitude is not a surveyed path"
          : source === "bound_gtfs_boarding_point" ? "Bound GTFS boarding elevation"
          : source === "surveyed_station_surface" ? "Original surveyed surface XYZ, metres"
          : source === "interpolated_between_stop_bindings" ? "Interpolated vehicle elevation"
          : info.object.known ? "Source XYZ, metres" : "Unknown or incomplete altitude; missing coordinates drawn at 0 m";
        onHover({ x: info.x, y: info.y, props: { ...info.object.feature.properties, display_elevation: display } });
      };
      overlay.setProps({ layers: [
        new PolygonLayer<ElevatedPolygon>({
          id: "network-3d-surfaces", data: polygons, getPolygon: d => d.polygon,
          getFillColor: color, getLineColor: color, getLineWidth: 1,
          lineWidthUnits: "pixels", filled: true, stroked: true, extruded: false,
          pickable: true, onHover: hover, parameters: { depthCompare: "less-equal" },
          updateTriggers: { getFillColor: [style.elevationColor, selected], getLineColor: [style.elevationColor, selected] },
        }),
        new PathLayer<ElevatedPath, PathStyleExtensionProps<ElevatedPath>>({
          // Solid walking paths include vertical lift/stair segments. Do not
          // compile the billboard dash-arclength shader for these: at a
          // top-down view its projected length can be zero and corner
          // clipping becomes unstable, producing long radial spikes.
          id: "network-3d-paths", data: paths.filter(d => !useGpuDash(d)), getPath: d => d.path,
          getColor: color, getWidth: d => Number(d.feature.properties._width ?? 2),
          widthUnits: "pixels", widthMinPixels: 1, capRounded: true, jointRounded: true,
          billboard: true, pickable: true, onHover: hover,
          updateTriggers: { getColor: [style.elevationColor, selected] },
          parameters: { depthCompare: "less-equal" },
        }),
        new PathLayer<ElevatedPath, PathStyleExtensionProps<ElevatedPath>>({
          id: "network-3d-dashed-paths", data: paths.filter(useGpuDash), getPath: d => d.path,
          getColor: color, getWidth: d => Number(d.feature.properties._width ?? 2),
          widthUnits: "pixels", widthMinPixels: 1, capRounded: true, jointRounded: true,
          billboard: true, pickable: true, onHover: hover,
          extensions: [pathStyle],
          getDashArray: d => d.feature.properties._dash === 1 ? [3, 4] : [0, 0],
          getOffset: d => Number(d.feature.properties._offset ?? 0) / Math.max(1, Number(d.feature.properties._width ?? 2)),
          dashGapPickable: true,
          updateTriggers: { getColor: [style.elevationColor, selected] },
          parameters: { depthCompare: "less-equal" },
        }),
        new ScatterplotLayer<ElevatedPoint>({
          id: "network-3d-points", data: points, getPosition: d => d.position,
          getFillColor: color, getRadius: d => Number(d.feature.properties._radius ?? 4),
          radiusUnits: "pixels", radiusMinPixels: 2, stroked: true, getLineColor: [255, 255, 255],
          lineWidthUnits: "pixels", getLineWidth: 1, billboard: true, pickable: true, onHover: hover,
          updateTriggers: { getFillColor: [style.elevationColor, selected] },
        }),
        new TextLayer<Datum, CollisionFilterExtensionProps<Datum>>({
          id: "station-3d-labels", data: cachedLabels,
          getPosition: position, getText: d => String(d.feature.properties._label),
          getSize: 11, getColor: [35, 25, 49], getPixelOffset: [0, -15],
          background: true, getBackgroundColor: [255, 255, 255, 220], backgroundPadding: [3, 2],
          billboard: true, pickable: false, fontFamily: "sans-serif",
          extensions: [labelCollisions], collisionEnabled: true,
          collisionGroup: "station-labels", getCollisionPriority: d => "polygon" in d ? 2 : 1,
        }),
      ] });
    },
  };
}
