export interface TransitTimeContext {
  agency_timezone: string;
  time_origin_unix_s: number;
}

/** Find the time origin in a transit response or a standalone exported feature. */
export function transitTimeContext(value: unknown): TransitTimeContext | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const obj = value as Record<string, unknown>;
  if (typeof obj.agency_timezone === "string" && typeof obj.time_origin_unix_s === "number") {
    return { agency_timezone: obj.agency_timezone, time_origin_unix_s: obj.time_origin_unix_s };
  }
  for (const key of ["time_context", "result", "service", "metadata", "properties"]) {
    const context = transitTimeContext(obj[key]);
    if (context) return context;
  }
  return undefined;
}
