# TypeScript cold-start POC

A minimal TypeScript Worker for comparing cold start with the Python Worker and the [Rust POC](../rust-poc/README.md). It is **not** a port of the sandbox.

It mirrors the Rust POC route for route:

- `GET /health`
- `POST /v1/systemone`, which requires `Authorization: Bearer github-2026`, parses the JSON body and returns a fixed `noul` answer per question

No framework and no dependencies: Wrangler bundles `src/index.ts` with esbuild into a ~1.4 KiB script. There is no build step and no WASM to instantiate, so V8 only has to compile a tiny JS module on a cold start.

## Run, deploy, benchmark

```sh
task ts:dev                              # local, http://localhost:8789
task ts:deploy                           # separate Worker: decision-sandbox-ts.<subdomain>.workers.dev
task bench:ts URL=https://decision-sandbox-ts.<subdomain>.workers.dev
```

Measure cold start the same way as for the Rust POC; see [Measuring cold start fairly](../rust-poc/README.md#measuring-cold-start-fairly).
