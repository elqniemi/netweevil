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
  if (h > 0) return `${h} h ${m} min`;
  if (m > 0) return `${m} min ${rest} s`;
  return `${rest} s`;
}

export function fmtDistance(metres: number): string {
  if (!Number.isFinite(metres)) return "–";
  if (metres >= 10_000) return `${(metres / 1000).toFixed(1)} km`;
  if (metres >= 1000) return `${(metres / 1000).toFixed(2)} km`;
  return `${Math.round(metres)} m`;
}

export function fmtValue(key: string, value: unknown): string {
  if (value === null || value === undefined) return "–";
  if (typeof value === "number") {
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
