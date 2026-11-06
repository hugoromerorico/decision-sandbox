# TypeScript port

A full TypeScript port of the Python Worker ([src/](../src/)), with no framework and no runtime dependencies. It serves the same routes and returns the same status codes, headers and bodies. **For a given seed, the answers are byte-identical to the Python Worker's**, which `task parity` checks against all three implementations.

## What "identical" takes

The Python app's behaviour comes partly from CPython, FastAPI, Starlette and pydantic. Matching it byte for byte means reproducing that behaviour too:

| Python behaviour | Where |
|---|---|
| `random.Random`: Mersenne Twister seeded from a 256-bit int, `random()`, `getrandbits`, `gammavariate`, `betavariate` | [src/pyrandom.ts](src/pyrandom.ts) |
| `sum()` of floats (Neumaier compensated, CPython 3.12+), `round(x, 4)` (ties to even) | [src/pyrandom.ts](src/pyrandom.ts) |
| `json.loads`: dict key order (JS objects reorder integer-like keys), duplicate keys, ints vs floats, big ints, `NaN`/`Infinity`, the decoder's error messages and positions | [src/json.ts](src/json.ts) |
| `json.dumps(sort_keys=True)` for seed hashing (code-point key order, `repr()` floats), Starlette's JSON responses, the float format of the Workers runtime's pydantic-core | [src/json.ts](src/json.ts) |
| pydantic validation errors: type, loc, msg, input, ctx and order, including lax int parsing of `delay_ms` | [src/schemas.ts](src/schemas.ts) |
| FastAPI's order of work (body parse, API key, rate limits, parameter validation), strict content type, 405/307/404 handling | [src/index.ts](src/index.ts) |
| CORS and sandbox middleware, request size limit, `X-Request-Id` | [src/index.ts](src/index.ts) |

The OpenAPI spec and the `/docs` and `/redoc` pages are exported from the Python app (`task shared`) and served verbatim.

## Run, test, deploy

```sh
task ts:install                          # once: wrangler + typescript
task ts:dev                              # local, http://localhost:8789 (uses ../.dev.vars)
task ts:check                            # typecheck
task parity                              # Python vs Rust vs TypeScript, byte for byte
task secrets:ts                          # upload SANDBOX_API_KEYS / SANDBOX_ADMIN_TOKEN
task ts:deploy                           # decision-sandbox-ts.<subdomain>.workers.dev
task bench:ts URL=https://decision-sandbox-ts.<subdomain>.workers.dev
```

The Worker uses its own rate-limit namespaces (2001/2002), so its counters are separate from the Python and Rust Workers'.

## Known differences

- JSON nested more than 512 levels deep is rejected with a 400. Python's limit depends on the runtime's stack.
- Python's `repr()` escapes unassigned Unicode code points in the "unknown scenario" message. This port uses the JS engine's Unicode tables, which can be a version apart.
- `exp`, `log` and `pow` come from V8, not musl. They can differ from the Python Worker in the last bit, which could only change an answer if a value sits within about 1e-16 of a rounding boundary. No such case has turned up in the fuzz runs.
