import type { ApiErrorBody } from "./types";

/** One timed request, as shown in the performance panel and its exports. */
export interface Timing {
  id: number;
  at: string;
  tool: string;
  method: string;
  path: string;
  status: number;
  ok: boolean;
  /** Time until response headers arrived. */
  ttfbMs: number;
  /** Time until the body was fully downloaded. */
  downloadMs: number;
  /** JSON.parse time on the client. */
  parseMs: number;
  totalMs: number;
  requestBytes: number;
  responseBytes: number;
  featureCount: number | null;
  /** Server hints such as profile cache status and prepare time. */
  serverHints: Record<string, string>;
  /** Free-form tag, e.g. "bench" for repeated benchmark runs. */
  tag?: string;
  error?: string;
}

export class ApiError extends Error {
  status: number;
  body: ApiErrorBody | null;
  timing: Timing;
  constructor(message: string, status: number, body: ApiErrorBody | null, timing: Timing) {
    super(message);
    this.status = status;
    this.body = body;
    this.timing = timing;
  }
}

export interface ApiResponse<T = unknown> {
  data: T;
  headers: Headers;
  timing: Timing;
  rawText: string;
}

let nextTimingId = 1;

const HINT_HEADERS = ["x-netweevil-profile-cache", "x-netweevil-profile-prepare-ms"];

export interface RequestOptions {
  tool: string;
  method?: "GET" | "POST" | "DELETE";
  body?: unknown;
  tag?: string;
  signal?: AbortSignal;
}

/**
 * Fetches an API path with timing instrumentation. The timing record is
 * returned with the response (or attached to the thrown error) so callers can
 * push it into the performance log.
 */
export async function apiRequest<T = unknown>(base: string, path: string, options: RequestOptions): Promise<ApiResponse<T>> {
  const method = options.method ?? (options.body === undefined ? "GET" : "POST");
  const url = joinUrl(base, path);
  const bodyText = options.body === undefined ? undefined : JSON.stringify(options.body);
  const t0 = performance.now();
  const timing: Timing = {
    id: nextTimingId++,
    at: new Date().toISOString(),
    tool: options.tool,
    method,
    path,
    status: 0,
    ok: false,
    ttfbMs: 0,
    downloadMs: 0,
    parseMs: 0,
    totalMs: 0,
    requestBytes: bodyText ? utf8Length(bodyText) : 0,
    responseBytes: 0,
    featureCount: null,
    serverHints: {},
    tag: options.tag,
  };
  let response: Response;
  try {
    response = await fetch(url, {
      method,
      headers: bodyText ? { "content-type": "application/json" } : undefined,
      body: bodyText,
      signal: options.signal,
    });
  } catch (error) {
    timing.totalMs = performance.now() - t0;
    timing.error = error instanceof Error ? error.message : String(error);
    throw new ApiError(`Network error: ${timing.error}`, 0, null, timing);
  }
  timing.ttfbMs = performance.now() - t0;
  timing.status = response.status;
  for (const header of HINT_HEADERS) {
    const value = response.headers.get(header);
    if (value !== null) timing.serverHints[header.replace("x-netweevil-", "")] = value;
  }
  const text = await response.text();
  timing.downloadMs = performance.now() - t0 - timing.ttfbMs;
  const contentLength = response.headers.get("content-length");
  timing.responseBytes = contentLength ? Number(contentLength) : utf8Length(text);
  const p0 = performance.now();
  let data: unknown = null;
  if (text.length > 0) {
    try {
      data = JSON.parse(text);
    } catch {
      data = text;
    }
  }
  timing.parseMs = performance.now() - p0;
  timing.totalMs = performance.now() - t0;
  timing.ok = response.ok;
  if (!response.ok) {
    const body = isErrorBody(data) ? data : null;
    const message = body?.error ?? (typeof data === "string" && data ? data : `HTTP ${response.status}`);
    timing.error = message;
    throw new ApiError(message, response.status, body, timing);
  }
  return { data: data as T, headers: response.headers, timing, rawText: text };
}

function isErrorBody(value: unknown): value is ApiErrorBody {
  return typeof value === "object" && value !== null && typeof (value as ApiErrorBody).error === "string";
}

export function joinUrl(base: string, path: string): string {
  if (!base) return path;
  return base.replace(/\/+$/, "") + (path.startsWith("/") ? path : `/${path}`);
}

function utf8Length(text: string): number {
  // Fast path for ASCII payloads; fall back to a real byte count otherwise.
  let extra = 0;
  for (let i = 0; i < text.length; i++) {
    const code = text.charCodeAt(i);
    if (code > 0x7f) {
      extra = -1;
      break;
    }
  }
  return extra === 0 ? text.length : new TextEncoder().encode(text).length;
}

/** Builds the curl equivalent of a console request, for reproducing it in a shell. */
export function toCurl(base: string, path: string, body: unknown, method = "POST"): string {
  const url = joinUrl(base || "http://127.0.0.1:8080", path);
  if (body === undefined) return `curl -s ${method !== "GET" ? `-X ${method} ` : ""}'${url}'`;
  const json = JSON.stringify(body, null, 2).replace(/'/g, "'\\''");
  return `curl -s -X ${method} '${url}' \\\n  -H 'content-type: application/json' \\\n  --data '${json}'`;
}
