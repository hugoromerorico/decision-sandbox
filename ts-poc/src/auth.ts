/**
 * API key allowlist, admin token, per-IP/per-key rate limits and in-memory request
 * counters. Mirrors src/sandbox/auth.py and src/sandbox/ratelimit.py.
 *
 * Counters live in memory only: they are per isolate and reset on eviction or deploy.
 */

import { IP_REQUESTS_PER_MINUTE, KEY_REQUESTS_PER_MINUTE, PUBLIC_KEYS, WINDOW_SECONDS } from "./config";
import { type Json, type JsonObject, pyStrip } from "./json";

interface RateLimiter {
  limit(options: { key: string }): Promise<{ success: boolean }>;
}

export interface Env {
  SANDBOX_API_KEYS?: string;
  SANDBOX_ADMIN_TOKEN?: string;
  IP_RATE_LIMITER?: RateLimiter;
  KEY_RATE_LIMITER?: RateLimiter;
}

export class HttpError extends Error {
  constructor(
    readonly status: number,
    readonly detail: string,
    readonly headers: Record<string, string> = {},
  ) {
    super(detail);
  }
}

const unauthorized = () => new HttpError(401, "Not authenticated", { "WWW-Authenticate": "Bearer" });
const encoder = new TextEncoder();
const now = () => Math.floor(Date.now() / 1000);

// --- Keys ----------------------------------------------------------------------

/** `Authorization: Bearer <key>`: the scheme is case-insensitive, the key is stripped. */
export function apiKey(request: Request): string | null {
  const header = request.headers.get("authorization") ?? "";
  const space = header.indexOf(" ");
  const scheme = space < 0 ? header : header.slice(0, space);
  const token = space < 0 ? "" : header.slice(space + 1);
  if (scheme.toLowerCase() !== "bearer") return null;
  return pyStrip(token) || null;
}

function allowlist(env: Env): string[] {
  const extra = (env.SANDBOX_API_KEYS ? String(env.SANDBOX_API_KEYS) : "").split(",").map(pyStrip).filter(Boolean);
  return [...new Set([...PUBLIC_KEYS, ...extra])];
}

/** `hmac.compare_digest`: time depends on the length only, not on the content. */
function compareDigest(a: string, b: string): boolean {
  const x = encoder.encode(a);
  const y = encoder.encode(b);
  const other = x.length === y.length ? y : x;
  let diff = x.length ^ y.length;
  for (let i = 0; i < x.length; i++) diff |= (x[i] as number) ^ (other[i] as number);
  return diff === 0;
}

function match(candidate: string, keys: string[]): string | null {
  let found: string | null = null;
  for (const key of keys) if (compareDigest(candidate, key)) found = key; // no early exit
  return found;
}

// --- Metrics -------------------------------------------------------------------

interface KeyStats {
  requests: number;
  byEndpoint: Map<string, number>;
  lastSeen: number;
}

class Metrics {
  startedAt = now();
  unauthorized = 0;
  keys = new Map<string, KeyStats>();

  record(key: string, endpoint: string): void {
    let entry = this.keys.get(key);
    if (!entry) this.keys.set(key, (entry = { requests: 0, byEndpoint: new Map(), lastSeen: 0 }));
    entry.requests += 1;
    entry.byEndpoint.set(endpoint, (entry.byEndpoint.get(endpoint) ?? 0) + 1);
    entry.lastSeen = now();
  }

  snapshot(): JsonObject {
    const keys: JsonObject = new Map();
    let total = 0;
    for (const [key, e] of this.keys) {
      total += e.requests;
      keys.set(key, new Map<string, Json>([
        ["requests", BigInt(e.requests)],
        ["by_endpoint", new Map([...e.byEndpoint].map(([k, v]) => [k, BigInt(v)]))],
        ["last_seen", BigInt(e.lastSeen)],
      ]));
    }
    return new Map<string, Json>([
      ["started_at", BigInt(this.startedAt)],
      ["total", BigInt(total)],
      ["unauthorized", BigInt(this.unauthorized)],
      ["keys", keys],
      ["note", "In-memory, per isolate; resets on eviction or deploy."],
    ]);
  }
}

export const metrics = new Metrics();

export function requireApiKey(request: Request, env: Env, path: string): string {
  const candidate = apiKey(request);
  const key = candidate ? match(candidate, allowlist(env)) : null;
  if (key === null) {
    metrics.unauthorized += 1;
    throw unauthorized();
  }
  metrics.record(key, path);
  console.log(JSON.stringify({ event: "request", key_label: key, endpoint: path }));
  return key;
}

export function requireAdmin(request: Request, env: Env): void {
  const token = env.SANDBOX_ADMIN_TOKEN ? String(env.SANDBOX_ADMIN_TOKEN) : "";
  if (!token) throw new HttpError(404, "Not found");
  const candidate = apiKey(request);
  if (!candidate || !compareDigest(candidate, token)) throw unauthorized();
}

// --- Rate limits ---------------------------------------------------------------

/** In-memory fixed window, used only when a rate-limit binding is missing. */
class LocalLimiter {
  private windows = new Map<string, [number, number]>();

  allow(key: string, limit: number): boolean {
    const window = Math.floor(now() / WINDOW_SECONDS);
    let [start, count] = this.windows.get(key) ?? [window, 0];
    if (start !== window) {
      if (this.windows.size > 10_000) this.windows.clear();
      count = 0;
      start = window;
    }
    this.windows.set(key, [window, count + 1]);
    return count < limit;
  }
}

const localLimiter = new LocalLimiter();

async function allow(binding: RateLimiter | undefined, name: string, key: string, fallbackLimit: number): Promise<boolean> {
  if (!binding) return localLimiter.allow(key, fallbackLimit);
  try {
    return (await binding.limit({ key })).success;
  } catch (err) {
    // Fail open: an unavailable limiter should not take the sandbox down.
    console.error(`Rate limiter ${name} failed`, err);
    return true;
  }
}

const clientIp = (request: Request) => request.headers.get("cf-connecting-ip") || "unknown";

async function sha256Hex(text: string): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", encoder.encode(text)));
  return [...digest].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export async function enforceRateLimits(request: Request, env: Env): Promise<void> {
  const checks: [RateLimiter | undefined, string, string, number][] = [
    [env.IP_RATE_LIMITER, "IP_RATE_LIMITER", `ip:${clientIp(request)}`, IP_REQUESTS_PER_MINUTE],
  ];
  const key = apiKey(request);
  if (key) {
    // Only a hash of the key is used; keys are never stored or logged.
    const digest = (await sha256Hex(key)).slice(0, 32);
    checks.push([env.KEY_RATE_LIMITER, "KEY_RATE_LIMITER", `key:${digest}`, KEY_REQUESTS_PER_MINUTE]);
  }
  for (const [binding, name, limitKey, limit] of checks) {
    if (!(await allow(binding, name, limitKey, limit))) {
      throw new HttpError(429, "Rate limit exceeded. Decision Sandbox is a free shared service; please slow down.", {
        "Retry-After": String(WINDOW_SECONDS),
      });
    }
  }
}
