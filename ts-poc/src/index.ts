/**
 * Minimal TypeScript Worker for measuring cold start against the Python and Rust Workers.
 *
 * Mirrors rust-poc/src/lib.rs route for route: GET /health, POST /v1/systemone
 * with a Bearer key, a fixed answer per question. No framework and no
 * dependencies, so the bundle is as small as the runtime allows.
 */

const PUBLIC_KEY = "github-2026";

const json = (body: unknown, status = 200, headers: Record<string, string> = {}): Response =>
  Response.json(body, { status, headers });

async function systemone(req: Request): Promise<Response> {
  if (req.headers.get("Authorization") !== `Bearer ${PUBLIC_KEY}`) {
    return json({ detail: "Not authenticated" }, 401);
  }

  // Parse the body so the request does comparable work to the real endpoint.
  let body: { questions?: unknown };
  try {
    body = await req.json();
  } catch {
    return json({ detail: "Invalid JSON body" }, 422);
  }

  const questions = body?.questions;
  const names = questions && typeof questions === "object" && !Array.isArray(questions)
    ? Object.keys(questions)
    : [];
  const answers = Object.fromEntries(names.map((name) => [name, { type: "noul", noul: 0.9312 }]));

  return json(
    {
      model: "decision-sandbox-v1",
      answers,
      usage: { input_tokens: 20, output_tokens: 2 * names.length },
    },
    200,
    { "X-Sandbox-Synthetic": "true" },
  );
}

export default {
  async fetch(req: Request): Promise<Response> {
    const { pathname } = new URL(req.url);
    if (pathname === "/health" && req.method === "GET") return json({ status: "ok" });
    if (pathname === "/v1/systemone" && req.method === "POST") return systemone(req);
    return new Response("Not Found", { status: 404 });
  },
};
