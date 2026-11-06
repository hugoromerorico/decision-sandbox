/**
 * Decision Sandbox, TypeScript port of the Python Worker (src/app.py).
 *
 * Same routes, status codes, headers and bodies; seeded answers are byte-identical.
 * The request pipeline reproduces the Python app's middleware stack, outermost first:
 * Starlette's ServerErrorMiddleware (plain 500), CORSMiddleware, SandboxMiddleware
 * (size limit, X-Request-Id and synthetic headers), then FastAPI routing and
 * dependency order: body parsing, API key, rate limits, parameter validation.
 */

import docsHtml from "../../shared/docs.html";
import oauthRedirectHtml from "../../shared/oauth2-redirect.html";
import openapiJson from "../../shared/openapi.json";
import redocHtml from "../../shared/redoc.html";
import { type Env, HttpError, enforceRateLimits, metrics, requireAdmin, requireApiKey } from "./auth";
import { MAX_BODY_BYTES, MAX_DELAY_MS, MAX_SEED_LENGTH, SANDBOX_MODEL, SANDBOX_VERSION, TIMEOUT_SCENARIO_MS } from "./config";
import { generate, newSeed, randomHex } from "./generator";
import { BodyParseError, EncodeError, type FloatStyle, type Json, JsonDecodeError, dumps, loadsBytes, pyLen, pyReprStr } from "./json";
import { type Scenario, UnknownScenario, catalogue, parseScenario } from "./scenarios";
import { type Body, type SystemOneRequest, type ValidationError, errorsToJson, validateIntParam, validateRequest } from "./schemas";

const TOO_LARGE = `Request body exceeds the sandbox limit of ${MAX_BODY_BYTES} bytes.`;
const EXPOSE_HEADERS = "X-Request-Id, X-Sandbox-Synthetic, X-Sandbox-Model, X-Sandbox-Seed, X-Sandbox-Scenario, Retry-After";
const RELEASE_DATE = "2026-10-04";

type Headers_ = Record<string, string>;

/** A response before it is turned into a Fetch Response, so middleware can add headers. */
interface Reply {
  status: number;
  body: string;
  headers: Headers_;
}

const obj = (entries: [string, Json][]): Json => new Map(entries);

function json(status: number, body: Json, headers: Headers_ = {}, floats: FloatStyle = "python"): Reply {
  return { status, body: dumps(body, { floats }), headers: { "content-type": "application/json", ...headers } };
}

const detail = (status: number, message: string, headers: Headers_ = {}) => json(status, obj([["detail", message]]), headers);

const text = (status: number, body: string, contentType: string, headers: Headers_ = {}): Reply => ({
  status,
  body,
  headers: { "content-type": contentType, ...headers },
});

const internalError = () =>
  new Response("Internal Server Error", { status: 500, headers: { "content-type": "text/plain; charset=utf-8" } });

// --- Routes ------------------------------------------------------------------

const MODELS = obj([
  ["models", [
    obj([
      ["name", SANDBOX_MODEL],
      ["description", "Synthetic decision generator. Returns random, seeded or scenario-driven answers. Not an AI model."],
      ["release_date", RELEASE_DATE],
    ]),
    obj([
      ["name", "jev-latest"],
      ["description", `Compatibility alias for ${SANDBOX_MODEL} so existing integrations work unchanged. Answers are synthetic; this is not TypeSafe's Jev model.`],
      ["release_date", RELEASE_DATE],
    ]),
  ]],
]);

type Handler = (request: Request, env: Env, url: URL) => Promise<Reply> | Reply;

const ROUTES: Record<string, { method: "GET" | "POST"; handler: Handler }> = {
  "/": {
    method: "GET",
    handler: () =>
      json(200, obj([
        ["name", "Decision Sandbox"],
        ["description", "A free synthetic decision API for testing Jev-compatible integrations."],
        ["synthetic", true],
        ["affiliated_with_typesafe", false],
        ["model", SANDBOX_MODEL],
        ["version", SANDBOX_VERSION],
        ["docs", "/docs"],
        ["openapi", "/openapi.json"],
        ["endpoints", ["POST /v1/systemone", "GET /v1/models", "GET /sandbox/scenarios", "GET /health"]],
      ])),
  },
  "/health": {
    method: "GET",
    handler: () => json(200, obj([["status", "ok"], ["model", SANDBOX_MODEL], ["version", SANDBOX_VERSION], ["synthetic", true]])),
  },
  "/sandbox/scenarios": {
    method: "GET",
    handler: () => json(200, obj([["scenarios", catalogue().map((s) => obj([["name", s.name], ["description", s.description]]))]])),
  },
  "/sandbox/stats": {
    method: "GET",
    handler: (request, env) => {
      requireAdmin(request, env);
      return json(200, metrics.snapshot());
    },
  },
  "/v1/models": {
    method: "GET",
    handler: async (request, env, url) => {
      requireApiKey(request, env, url.pathname);
      await enforceRateLimits(request, env);
      return json(200, MODELS, {}, "pydantic");
    },
  },
  "/v1/systemone": { method: "POST", handler: systemone },
  "/openapi.json": { method: "GET", handler: () => text(200, openapiJson, "application/json") },
  "/docs": { method: "GET", handler: () => text(200, docsHtml, "text/html; charset=utf-8") },
  "/docs/oauth2-redirect": { method: "GET", handler: () => text(200, oauthRedirectHtml, "text/html; charset=utf-8") },
  "/redoc": { method: "GET", handler: () => text(200, redocHtml, "text/html; charset=utf-8") },
};

// --- POST /v1/systemone --------------------------------------------------------

function isJsonContentType(value: string): boolean {
  // email.message's get_content_type(): anything but exactly one "/" means text/plain.
  const type = (value.split(";")[0] ?? "").trim().toLowerCase();
  const parts = type.split("/");
  if (parts.length !== 2) return false;
  const [main, sub] = parts as [string, string];
  return main.trim() === "application" && (sub.trim() === "json" || sub.trim().endsWith("+json"));
}

/** FastAPI reads and decodes the body before resolving any dependency. */
function readBody(raw: Uint8Array, contentType: string | null): Body | Reply {
  if (raw.length === 0) return { kind: "none" };
  if (contentType && isJsonContentType(contentType)) {
    try {
      return { kind: "json", value: loadsBytes(raw) };
    } catch (err) {
      if (err instanceof JsonDecodeError) {
        const error = obj([
          ["type", "json_invalid"],
          ["loc", ["body", BigInt(err.pos)]],
          ["msg", "JSON decode error"],
          ["input", new Map()],
          ["ctx", obj([["error", err.msg]])],
        ]);
        return json(422, obj([["detail", [error]]]));
      }
      if (err instanceof BodyParseError) return detail(400, "There was an error parsing the body");
      throw err;
    }
  }
  // Without a JSON content type the body stays bytes; echoing undecodable bytes in a
  // validation error fails in Python, so they become a lone surrogate that fails the same way.
  let value: string;
  try {
    value = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(raw);
  } catch {
    value = "\ud800";
  }
  return { kind: "text", value };
}

const isReply = (value: Body | Reply): value is Reply => "status" in value;
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
const lastQuery = (url: URL, name: string) => url.searchParams.getAll(name).at(-1) ?? null;

async function systemone(request: Request, env: Env, url: URL): Promise<Reply> {
  const raw = new Uint8Array(await request.arrayBuffer());
  if (raw.length > MAX_BODY_BYTES) return detail(413, TOO_LARGE);
  const body = readBody(raw, request.headers.get("content-type"));
  if (isReply(body)) return body;

  requireApiKey(request, env, url.pathname);
  await enforceRateLimits(request, env);

  const errors: ValidationError[] = [];
  const delayQuery = lastQuery(url, "delay_ms");
  const delayHeader = request.headers.get("x-sandbox-delay-ms");
  const delayFromQuery = delayQuery === null ? null : validateIntParam(delayQuery, ["query", "delay_ms"], MAX_DELAY_MS, errors);
  const delayFromHeader = delayHeader === null ? null : validateIntParam(delayHeader, ["header", "X-Sandbox-Delay-Ms"], MAX_DELAY_MS, errors);
  const validated = validateRequest(body);
  errors.push(...validated.errors);
  if (errors.length) return json(422, obj([["detail", errorsToJson(errors)]]));
  const req = validated.request as SystemOneRequest;

  let seed = lastQuery(url, "seed") ?? request.headers.get("x-sandbox-seed");
  if (seed !== null && !(pyLen(seed) > 0 && pyLen(seed) <= MAX_SEED_LENGTH)) {
    return detail(400, `Seed must be 1 to ${MAX_SEED_LENGTH} characters.`);
  }
  seed = seed || newSeed();

  let chosen: Scenario;
  try {
    chosen = parseScenario(lastQuery(url, "scenario") ?? request.headers.get("x-sandbox-scenario"));
  } catch (err) {
    if (!(err instanceof UnknownScenario)) throw err;
    return detail(400, `Unknown sandbox scenario ${pyReprStr(err.value)}. See GET /sandbox/scenarios for the list.`);
  }

  const sandboxHeaders = { "X-Sandbox-Seed": seed, "X-Sandbox-Scenario": chosen.name };
  const delay = delayFromQuery ?? delayFromHeader;
  if (delay) await sleep(delay);

  if (chosen.error) return simulatedError(chosen, req, sandboxHeaders);
  return json(200, await generate(req, seed, chosen), sandboxHeaders, "pydantic");
}

async function simulatedError(scenario: Scenario, req: SystemOneRequest, headers: Headers_): Promise<Reply> {
  switch (scenario.error) {
    case "invalid-response": {
      // Violates the contract: wrong types, a missing field and an unknown answer type.
      const name = req.questions.keys().next().value as string;
      const answer = obj([["type", "unknown"], ["noul", "yes"], ["confidence", null]]);
      return json(200, obj([["model", SANDBOX_MODEL], ["answers", obj([[name, answer]])]]), headers);
    }
    case "validation-error":
      return json(422, obj([["detail", [obj([["loc", ["body", "state"]], ["msg", "Field required"], ["type", "missing"]])]]]), headers);
    case "unauthorized":
      return detail(401, "Not authenticated", { ...headers, "WWW-Authenticate": "Bearer" });
    case "rate-limit":
      return detail(429, "Rate limit exceeded (simulated).", { ...headers, "Retry-After": "1" });
    case "unavailable":
      return detail(503, "Service unavailable (simulated).", { ...headers, "Retry-After": "1" });
    case "timeout":
      await sleep(TIMEOUT_SCENARIO_MS);
      return detail(504, "Gateway timeout (simulated).", headers);
    default:
      return detail(500, "Internal server error (simulated).", headers);
  }
}

// --- Routing (Starlette router + FastAPI exception handlers) ---------------------

function routePath(url: URL): string {
  try {
    return decodeURIComponent(url.pathname);
  } catch {
    return url.pathname;
  }
}

async function route(request: Request, env: Env, url: URL): Promise<Reply> {
  const path = routePath(url);
  const entry = ROUTES[path];
  if (!entry) {
    // redirect_slashes: /health/ -> /health and /health -> /health/, if that route exists.
    if (path !== "/") {
      const other = path.endsWith("/") ? path.replace(/\/+$/, "") : `${path}/`;
      if (ROUTES[other]) {
        const location = new URL(url);
        location.pathname = other;
        return { status: 307, body: "", headers: { location: location.toString() } };
      }
    }
    return detail(404, "Not Found");
  }
  if (request.method !== entry.method) return detail(405, "Method Not Allowed", { Allow: entry.method });
  try {
    return await entry.handler(request, env, url);
  } catch (err) {
    if (err instanceof HttpError) return detail(err.status, err.detail, err.headers);
    throw err;
  }
}

/** SandboxMiddleware: body size limit and the headers stamped on every response. */
async function sandbox(request: Request, env: Env, url: URL): Promise<Reply> {
  const requestId = "req_" + randomHex(12);
  const length = request.headers.get("content-length");
  const reply =
    length !== null && /^\d+$/.test(length) && Number(length) > MAX_BODY_BYTES
      ? detail(413, TOO_LARGE)
      : await route(request, env, url);
  reply.headers["x-request-id"] = requestId;
  reply.headers["x-sandbox-synthetic"] = "true";
  reply.headers["x-sandbox-model"] = SANDBOX_MODEL;
  return reply;
}

/** CORSMiddleware(allow_origins=["*"], allow_methods=[GET, POST, OPTIONS], allow_headers=["*"]). */
function preflight(request: Request): Reply {
  const headers: Headers_ = {
    Vary: "Origin, Access-Control-Request-Method, Access-Control-Request-Headers, Access-Control-Request-Private-Network",
    "Access-Control-Allow-Origin": "*",
    "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
    "Access-Control-Max-Age": "600",
  };
  const failures: string[] = [];
  const method = request.headers.get("access-control-request-method") ?? "";
  if (!["GET", "POST", "OPTIONS"].includes(method)) failures.push("method");
  const requested = request.headers.get("access-control-request-headers");
  if (requested !== null) headers["Access-Control-Allow-Headers"] = requested;
  if (request.headers.has("access-control-request-private-network")) failures.push("private-network");
  return failures.length
    ? text(400, `Disallowed CORS ${failures.join(", ")}`, "text/plain; charset=utf-8", headers)
    : text(200, "OK", "text/plain; charset=utf-8", headers);
}

/**
 * The Python Worker's ASGI adapter encodes header values as latin-1 and decodes them as
 * UTF-8. Values that fail either step (e.g. a non-ASCII seed) crash the Python Worker
 * with a 500; this reproduces that, and the value it would send otherwise.
 */
function asgiHeaderValue(value: string): string {
  if (/[^\x00-\xff]/.test(value)) throw new EncodeError("latin-1");
  const bytes = Uint8Array.from(value, (c) => c.charCodeAt(0));
  return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
}

async function handle(request: Request, env: Env): Promise<Response> {
  const url = new URL(request.url);
  const origin = request.headers.get("origin");
  let reply: Reply;
  if (origin !== null && request.method === "OPTIONS" && request.headers.has("access-control-request-method")) {
    reply = preflight(request);
  } else {
    reply = await sandbox(request, env, url);
    if (origin !== null) {
      reply.headers["Access-Control-Allow-Origin"] = "*";
      reply.headers["Access-Control-Expose-Headers"] = EXPOSE_HEADERS;
    }
    reply.headers.Vary = "Origin";
  }
  const headers = new Headers();
  for (const [name, value] of Object.entries(reply.headers)) headers.set(name, asgiHeaderValue(value));
  return new Response(reply.body || null, { status: reply.status, headers });
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    try {
      return await handle(request, env);
    } catch (err) {
      // Starlette's ServerErrorMiddleware, or the Python Worker failing outright.
      if (!(err instanceof EncodeError)) console.error(err);
      return internalError();
    }
  },
} satisfies ExportedHandler<Env>;
