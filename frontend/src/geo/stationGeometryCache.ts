import type { FeatureCollection } from "./features";
import { stationGeometryFeatures, type StationGeometryResponse } from "./stationGeometry";

interface StationDisplay {
  stationGeometry: FeatureCollection | null;
  stationGeometryStatus: string | null;
}

/** Keep the last successful viewport visible while its replacement loads.
 * Request tickets prevent late responses from restoring an old feed or view. */
export class StationGeometryCache {
  private scope: string | null = null;
  private sequence = 0;
  private signature: string | null = null;
  private geometry: FeatureCollection | null = null;
  private completeFeed = false;
  private viewportOnly = false;

  coversFeed(scope: string): boolean { return this.scope === scope && this.completeFeed; }
  needsViewport(scope: string): boolean { return this.scope === scope && this.viewportOnly; }

  markTruncated(ticket: number) {
    if (ticket === this.sequence) this.viewportOnly = true;
  }

  begin(scope: string | null): { ticket: number; display: StationDisplay } {
    if (scope !== this.scope || scope === null) {
      this.geometry = null;
      this.signature = null;
      this.completeFeed = false;
      this.viewportOnly = false;
    }
    this.scope = scope;
    return { ticket: ++this.sequence, display: {
      stationGeometry: this.geometry,
      stationGeometryStatus: scope === null ? null : this.geometry ? "Updating station platforms…" : "Loading station platforms…",
    } };
  }

  cancel(ticket: number) {
    if (ticket === this.sequence) this.sequence++;
  }

  complete(ticket: number, response: StationGeometryResponse, wholeFeed = false): StationDisplay | null {
    if (ticket !== this.sequence || this.scope === null) return null;
    this.completeFeed = wholeFeed && !response.metadata.truncated;
    // Repeated views often return identical station features. Preserve their
    // object identity so camera movements do not rebuild GPU buffers/labels.
    const signature = JSON.stringify(response.features);
    if (signature !== this.signature) {
      this.geometry = stationGeometryFeatures(response);
      this.signature = signature;
    }
    const surfaces = response.features.filter(f => f.properties.kind === "platform_surface").length;
    const boarding = response.features.filter(f => f.properties.kind === "boarding_point").length;
    const suffix = response.metadata.truncated ? " Zoom in to see all matches." : !response.metadata.surface_source_available ? " Surveyed platform surfaces are unavailable for this feed." : "";
    return { stationGeometry: this.geometry, stationGeometryStatus: `${surfaces} platform surfaces · ${boarding} boarding points.${suffix}` };
  }

  fail(ticket: number, error: unknown): StationDisplay | null {
    if (ticket !== this.sequence || this.scope === null) return null;
    const retained = this.geometry ? " Showing the previous viewport's station geometry." : "";
    return { stationGeometry: this.geometry, stationGeometryStatus: `Station platforms could not load: ${error instanceof Error ? error.message : String(error)}.${retained}` };
  }
}
