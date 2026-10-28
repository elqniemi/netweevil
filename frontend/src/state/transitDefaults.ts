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

/** True when a datetime's local date falls inside the feed's imported service window. */
export function datetimeInServiceWindow(datetime: unknown, feed: { service_start_date: string; service_days?: number }): boolean {
  if (typeof datetime !== "string" || !/^\d{4}-\d{2}-\d{2}/.test(datetime)) return false;
  const start = Date.parse(`${feed.service_start_date}T00:00:00Z`);
  const day = Date.parse(`${datetime.slice(0, 10)}T00:00:00Z`);
  if (Number.isNaN(start) || Number.isNaN(day)) return false;
  return day >= start && day < start + Math.max(feed.service_days ?? 1, 1) * 86_400_000;
}

/** Keeps saved transit form values, but replaces a datetime that a re-imported feed no longer covers. */
export function mergeTransitDefaults(defaults: Record<string, unknown>, saved: Record<string, unknown> | undefined, service: ServiceInfo): Record<string, unknown> {
  const merged = { ...defaults, ...saved };
  const feed = service.loaded_transit_feeds.find((f) => f.agency_timezone === "Asia/Hong_Kong");
  if (feed && "datetime" in defaults && !datetimeInServiceWindow(merged.datetime, feed)) merged.datetime = defaults.datetime;
  return merged;
}
