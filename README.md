# Decision Sandbox

**Test typed AI decisions without a model.**

Decision Sandbox is a free, open-source **synthetic decision API** for testing Jev-compatible integrations without model costs or inference infrastructure. It implements the public [TypeSafe API contract](https://api.typesafe.ai/openapi.json) (`POST /v1/systemone`, `GET /v1/models`). Send a structured decision request and you get back a correctly typed response: yes/no probabilities, choices and scores, each with confidence values and probabilities.

> [!IMPORTANT]
> **All answers are synthetic.** They are generated randomly, or derived from a seed or scenario. No model runs, and nothing here reproduces Jev or any other model. Probabilities are not calibrated predictions, so do not use them for real decisions. Decision Sandbox is an independent project and is **not affiliated with TypeSafe**. It is compatible with TypeSafe's public API contract, but its answers are not equivalent to what the model would return.

Use random answers for demos, seeded answers for reproducible tests and scenarios to simulate failures.

**Public instance:** [https://decision-sandbox.gutan.dev](https://decision-sandbox.gutan.dev)

## Quick start

Change your base URL from `https://api.typesafe.ai` to `https://decision-sandbox.gutan.dev`. An API key is required, as with TypeSafe. **The free key is `github-2026`.** Other keys are issued per channel so traffic sources can be told apart.

```sh
curl -s https://decision-sandbox.gutan.dev/v1/systemone \
  -H 'Authorization: Bearer github-2026' \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "jev-latest",
    "state": "I was charged twice. Please help.",
    "questions": {
      "billing": {"type": "noul", "instructions": "Is this message about billing?"},
      "tone": {"type": "choice", "criteria": {"angry": "Upset or hostile", "calm": "Neutral or polite"}},
      "urgency": {"type": "score", "criteria": ["Can wait", "Needs attention this week", "Needs attention today"]}
    }
  }'
```

```json
{
  "model": "decision-sandbox-v1",
  "answers": {
    "billing": {"type": "noul", "noul": 0.9312},
    "tone": {"type": "choice", "choice": "angry", "confidence": 0.7741,
             "probabilities": {"angry": 0.7741, "calm": 0.2259}},
    "urgency": {"type": "score", "score": 1.8383, "confidence": 0.8405,
                "legend": {"0": "Can wait", "1": "Needs attention this week", "2": "Needs attention today"},
                "probabilities": {"0": 0.0022, "1": 0.1573, "2": 0.8405}}
  },
  "usage": {"input_tokens": 77, "output_tokens": 6}
}
```

Interactive docs are at [https://decision-sandbox.gutan.dev/docs](https://decision-sandbox.gutan.dev/docs), and the OpenAPI schema is at [https://decision-sandbox.gutan.dev/openapi.json](https://decision-sandbox.gutan.dev/openapi.json).

## Compatibility

| | TypeSafe | Decision Sandbox |
|---|---|---|
| `POST /v1/systemone` | ✅ | ✅ Same request and response schema |
| `GET /v1/models` | ✅ | ✅ Lists `decision-sandbox-v1` and the `jev-latest` alias |
| Question types `noul`, `choice`, `score` | ✅ | ✅ |
| `422` validation errors | ✅ | ✅ Same `HTTPValidationError` shape |
| `Authorization: Bearer <key>` | Required | Required (allowlisted keys; free key `github-2026`) |
| `model` | Must be a listed model | Any string is accepted |
| Answers | Model output | **Synthetic** |
| `usage` | Billed tokens | A deterministic estimate (about 4 characters per token). Nothing is billed. |

Response bodies contain exactly the fields defined in the TypeSafe schema, so clients that reject unknown fields still work. The response's `model` field is always `decision-sandbox-v1`; the spec allows it to differ from the alias you requested. Sandbox metadata travels in response headers:

| Header | Meaning |
|---|---|
| `X-Sandbox-Synthetic: true` | The response was not produced by a model. Present on every response. |
| `X-Sandbox-Model` | Always `decision-sandbox-v1`. |
| `X-Request-Id` | Unique per request, useful in bug reports. |
| `X-Sandbox-Seed` | The seed that produced the answers. Send it back to replay the response. |
| `X-Sandbox-Scenario` | The scenario that was applied. |

The contract is enforced in CI: [tests/test_contract.py](tests/test_contract.py) validates every response against a vendored copy of TypeSafe's OpenAPI spec.

## Controlling answers

Sandbox controls go in headers, so request bodies remain valid TypeSafe requests. Each control also has a query parameter, which takes precedence over the header.

| Header | Query | Effect |
|---|---|---|
| `X-Sandbox-Seed` | `seed` | Makes answers reproducible. Any string of 1–128 characters. |
| `X-Sandbox-Scenario` | `scenario` | Applies a predefined behaviour (see [Scenarios](#scenarios)). |
| `X-Sandbox-Delay-Ms` | `delay_ms` | Adds latency of 0–5000 ms. |

### Random mode (default)

Every request gets fresh answers. The seed used is returned in `X-Sandbox-Seed`, so you can replay any response you want to investigate.

### Seeded mode

```text
Same request + same seed = same response
```

```sh
curl -s https://decision-sandbox.gutan.dev/v1/systemone \
  -H 'Authorization: Bearer github-2026' \
  -H 'X-Sandbox-Seed: ci-42' \
  -H 'Content-Type: application/json' \
  -d @request.json
```

Details:

- Each answer is derived from the seed, `state`, the question name and the question body. Adding or removing a question does not change the answers to the other questions.
- The `model` field does not affect answers, so `jev-latest` and `decision-sandbox-v1` produce the same results.
- Seeded results are stable within a major sandbox version (`decision-sandbox-v1`). If a future change would alter seeded output, it will ship under a new model version.

### Scenarios

`GET /sandbox/scenarios` returns the list below.

| Scenario | Behaviour |
|---|---|
| `random` | Varied answers (default) |
| `high-confidence` | `noul` near 0 or 1; one dominant choice or score level |
| `low-confidence` | `noul` near 0.5; near-uniform probabilities |
| `always-true` | Every `noul` is close to 1 |
| `always-false` | Every `noul` is close to 0 |
| `first-choice` / `last-choice` | Choice questions select their first or last option |
| `min-score` / `max-score` | Score questions favour the lowest or highest level |
| `choice:<name>` | Choice questions that offer `<name>` select it; others stay random |
| `score:<level>` | Score questions favour that level, clamped to the rubric |
| `invalid-response` | HTTP 200 with a body that violates the response schema |
| `validation-error` | HTTP 422 with an `HTTPValidationError` body |
| `unauthorized` | HTTP 401 |
| `rate-limit` | HTTP 429 with `Retry-After: 1` |
| `server-error` | HTTP 500 |
| `unavailable` | HTTP 503 with `Retry-After: 1` |
| `timeout` | Waits 10 s, then returns HTTP 504 |

Answer-shaping scenarios combine with seeds. For example, `scenario=low-confidence&seed=1` always returns the same uncertain answers.

## Client examples

**Python (httpx)**

```python
import httpx

client = httpx.Client(
    base_url="https://decision-sandbox.gutan.dev",  # was https://api.typesafe.ai
    headers={
        "Authorization": "Bearer github-2026",
        "X-Sandbox-Seed": "test-suite",  # optional: reproducible answers
    },
)
res = client.post(
    "/v1/systemone",
    json={
        "model": "jev-latest",
        "state": "I was charged twice.",
        "questions": {
            "billing": {"type": "noul", "instructions": "Is this about billing?"}
        },
    },
)
print(res.json()["answers"]["billing"]["noul"])
```

**JavaScript (fetch)**

```js
const res = await fetch("https://decision-sandbox.gutan.dev/v1/systemone?scenario=always-true", {
  method: "POST",
  headers: { "Content-Type": "application/json", Authorization: "Bearer github-2026" },
  body: JSON.stringify({
    model: "jev-latest",
    state: "I was charged twice.",
    questions: { billing: { type: "noul", instructions: "Is this about billing?" } },
  }),
});
const { answers } = await res.json();
```

CORS is open, so you can call the sandbox directly from browser code.

## Errors

| Status | When | Body |
|---|---|---|
| `400` | Unknown scenario or invalid seed | `{"detail": "..."}` |
| `401` | Missing or unknown API key | `{"detail": "Not authenticated"}` with `WWW-Authenticate: Bearer` |
| `413` | Body larger than 64 KiB | `{"detail": "..."}` |
| `422` | Request violates the schema or a sandbox limit | `{"detail": [{"loc", "msg", "type", ...}]}` |
| `429` | Rate limit exceeded | `{"detail": "..."}` with `Retry-After` |

## Limits

The sandbox is a free shared service, so every request is bounded.

| Limit | Value |
|---|---|
| Request body | 64 KiB |
| Questions per request | 32 |
| Question name length | 128 characters |
| Options per choice question | 64 |
| Choice name length | 256 characters |
| Levels per score question | 32 |
| Simulated delay | 5000 ms (the `timeout` scenario waits a fixed 10 s) |
| Requests per IP | 60 per minute |
| Requests per API key | 300 per minute (the per-IP limit also applies) |

Request bodies are never stored. API keys are checked against an allowlist. The service keeps per-key request counts in memory only (no database) and logs the key label and endpoint of each request. Keys identify a traffic source, not a person. Counts are per Worker instance and reset on restart or deploy, so they are approximate.

## Development

The service is a [FastAPI](https://fastapi.tiangolo.com/) app that runs on [Cloudflare Python Workers](https://developers.cloudflare.com/workers/languages/python/). It needs no database, model or external calls.

```
src/
├── entry.py              # Cloudflare Worker entrypoint (wraps the app as ASGI)
├── app.py                # Routes
└── sandbox/
    ├── schemas.py        # Pydantic models mirroring the TypeSafe contract
    ├── generator.py      # Synthetic answer generation (seeded RNG per question)
    ├── scenarios.py      # Named scenarios
    ├── ratelimit.py      # Per-IP / per-key limits (Cloudflare rate-limit bindings)
    ├── middleware.py     # Body size limit, synthetic headers
    └── limits.py         # All hard limits in one place
tests/
├── test_contract.py      # Contract tests against TypeSafe's OpenAPI spec
└── typesafe_openapi.json # Vendored copy of https://api.typesafe.ai/openapi.json
```

Requirements: [uv](https://docs.astral.sh/uv/) ≥ 0.12.3, [Node.js](https://nodejs.org/) (pywrangler uses it to run `wrangler`), and optionally [Task](https://taskfile.dev/).

```sh
task sync     # uv sync && uv run pywrangler sync
task test     # run the contract tests
task dev      # http://localhost:8787
task bench    # latency of dev and prod (bench:dev starts a dev server if none is running)
task deploy   # deploy to Cloudflare (run `uv run pywrangler login` the first time)
```

The Workers runtime uses **Python 3.14**, selected by `compatibility_date` in `wrangler.jsonc` (any date on or after `2026-09-08`). `.python-version` and `requires-python` are pinned to match.

Rate limits are enforced by the `ratelimits` bindings in `wrangler.jsonc`. Keep them in sync with [src/sandbox/limits.py](src/sandbox/limits.py). When the bindings are absent, as in unit tests, an in-memory limiter takes over.

API keys: `github-2026` is built in. Add more with the `SANDBOX_API_KEYS` secret (comma-separated, e.g. `linkedin-2026,community-2026`). Set `SANDBOX_ADMIN_TOKEN` to enable `GET /sandbox/stats` (send it as a Bearer token) for per-key counts; without it the endpoint returns 404.

```sh
uv run pywrangler secret put SANDBOX_API_KEYS
uv run pywrangler secret put SANDBOX_ADMIN_TOKEN
```

To refresh the vendored spec and check that the sandbox is still compatible:

```sh
curl -s https://api.typesafe.ai/openapi.json | python -m json.tool > tests/typesafe_openapi.json
task test
```
