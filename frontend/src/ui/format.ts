import type { TransitTimeContext } from "../api/time";

export function fmtMs(ms: number): string {
  if (!Number.isFinite(ms)) return "–";
  if (ms < 1) return `${ms.toFixed(2)} ms`;
  if (ms < 100) return `${ms.toFixed(1)} ms`;
  if (ms < 10_000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

export function fmtBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
}

export function fmtNumber(value: number): string {
  if (Number.isInteger(value)) return value.toLocaleString("en-US");
  if (Math.abs(value) >= 1000) return value.toLocaleString("en-US", { maximumFractionDigits: 0 });
  if (Math.abs(value) >= 10) return value.toLocaleString("en-US", { maximumFractionDigits: 1 });
  return value.toLocaleString("en-US", { maximumFractionDigits: 3 });
}

export function fmtDuration(seconds: number): string {
  if (!Number.isFinite(seconds)) return "–";
  const s = Math.round(seconds);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const rest = s % 60;
  if (h > 0) return m ? `${h} h ${m} min` : `${h} h`;
  if (m > 0) return rest ? `${m} min ${rest} s` : `${m} min`;
  return `${rest} s`;
}

export function fmtDistance(metres: number): string {
  if (!Number.isFinite(metres)) return "–";
  if (metres >= 10_000) return `${(metres / 1000).toFixed(1)} km`;
  if (metres >= 1000) return `${(metres / 1000).toFixed(2)} km`;
  return `${Math.round(metres)} m`;
}

export function fmtTransitTime(seconds: number, context?: TransitTimeContext): string {
  if (!Number.isFinite(seconds)) return "–";
  if (context) {
    const instant = new Date((context.time_origin_unix_s + seconds) * 1000);
    if (!Number.isFinite(instant.getTime())) return "–";
    return new Intl.DateTimeFormat("en-GB", {
      timeZone: context.agency_timezone,
      year: "numeric", month: "2-digit", day: "2-digit",
      hour: "2-digit", minute: "2-digit", second: "2-digit",
      hourCycle: "h23", timeZoneName: "shortOffset",
    }).format(instant);
  }
  const s = Math.round(seconds);
  const days = Math.floor(s / 86400);
  const clock = [Math.floor(s / 3600) % 24, Math.floor(s / 60) % 60, s % 60]
    .map((part) => String(part).padStart(2, "0")).join(":");
  return days ? `${clock} (+${days} ${days === 1 ? "day" : "days"})` : clock;
}

export function fmtValue(key: string, value: unknown, context?: TransitTimeContext): string {
  if (value === null || value === undefined) return "–";
  if (typeof value === "number") {
    if (!Number.isFinite(value)) return "–";
    const field = key.split(".").at(-1) ?? key;
    if (field === "departure_s" || field === "arrival_s" || (context && (field === "start_s" || field === "end_s"))) return fmtTransitTime(value, context);
    if (field.endsWith("_unix_s")) return new Date(value * 1000).toISOString();
    if (/_time_s$|^time_s$|_s$/.test(key) && !/^(id|z)$/.test(key)) return `${fmtDuration(value)} (${fmtNumber(value)})`;
    if (/_m$/.test(key)) return fmtDistance(value);
    return fmtNumber(value);
  }
  if (typeof value === "boolean") return value ? "yes" : "no";
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}

export function percentile(values: number[], p: number): number {
  if (!values.length) return NaN;
  const sorted = values.slice().sort((a, b) => a - b);
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1));
  return sorted[index];
}
