import type { ServiceInfo } from "../api/types";

export const TRANSIT_ANALYSIS_TOOLS = ["transit_route", "transit_directions", "transit_service_area"] as const;

/** Hong Kong examples must use platform-bound pedestrian access by default. */
export function hongKongTransitDefaults(service: ServiceInfo): Record<string, unknown> | null {
  const feed = service.loaded_transit_feeds.find((f) => f.agency_timezone === "Asia/Hong_Kong");
  const foot = service.loaded_profiles.find((p) => p.mode === "foot" && p.profile_id === service.default_profile_id)
    ?? service.loaded_profiles.find((p) => p.mode === "foot");
  if (!feed || !foot) return null;
  return {
    street_access: "network",
    walking_geometry: "network",
    include_stop_segments: true,
    pedestrian_profile_id: foot.profile_id,
    ...(feed.transfer_profile_ids.includes(foot.profile_id) ? { transfer_profile_id: foot.profile_id } : {}),
    ...(feed.service_start_date ? { datetime: `${feed.service_start_date}T08:30:00+08:00` } : {}),
  };
}
