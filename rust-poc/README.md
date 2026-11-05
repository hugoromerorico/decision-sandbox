# Rust cold-start POC

A minimal Rust Worker ([workers-rs](https://github.com/cloudflare/workers-rs)) for comparing cold start with the Python Worker. It is **not** a port of the sandbox.

It serves the routes [bench/latency.py](../bench/latency.py) calls:

- `GET /health`
- `POST /v1/systemone`, which requires `Authorization: Bearer github-2026`, parses the JSON body and returns a fixed `noul` answer per question

There is no generator, no seeds and no rate limiting. The goal is to measure how long the runtime takes to boot. This is where Python Workers are slow (Pyodide plus FastAPI imports, about 1.3 s cold), and where a Rust→WASM Worker should take a few milliseconds.

## Setup (one-time)

```sh
brew install rustup                      # keg-only: add /opt/homebrew/opt/rustup/bin to PATH
rustup default stable
rustup target add wasm32-unknown-unknown
```

The first build installs `worker-build` through the build command in `wrangler.jsonc`, using `~/.cargo/bin`, so that directory must be on `PATH` too.

## Run, deploy, benchmark

```sh
task rust:dev                            # local, http://localhost:8788
task rust:deploy                         # separate Worker: decision-sandbox-rs.<subdomain>.workers.dev
task bench:rust URL=https://decision-sandbox-rs.<subdomain>.workers.dev
```

## Measuring cold start fairly

- Each run gives only **one** cold sample (the `first POST` line). Collect 3–5 per Worker. A fresh deploy, or 10+ minutes idle, usually makes the next request cold.
- Cloudflare starts warming a Worker during the TLS handshake, so compare `connect + first POST`, not `first POST` alone.
- Benchmark the Python Worker the same way and at the same time (`task bench:prod`).
- Local numbers (`wrangler dev`) say nothing about cold start. Use the deployed Worker.
