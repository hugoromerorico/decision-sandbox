# Rust port

A full Rust port of the Python Worker ([src/](../src/)), built with [workers-rs](https://github.com/cloudflare/workers-rs). It serves the same routes and returns the same status codes, headers and bodies. **For a given seed, the answers are byte-identical to the Python Worker's**, which `task parity` checks against all three implementations.

It mirrors the [TypeScript port](../ts-poc/README.md) module for module. See that README for what byte-for-byte parity with CPython, FastAPI and pydantic involves. The modules:

- [src/pyrandom.rs](src/pyrandom.rs): CPython's Mersenne Twister and distributions, Neumaier `sum()`, `round()`
- [src/json.rs](src/json.rs): a JSON decoder and encoders with Python semantics (no serde_json)
- [src/schemas.rs](src/schemas.rs): pydantic-identical validation errors
- [src/generator.rs](src/generator.rs), [src/scenarios.rs](src/scenarios.rs), [src/auth.rs](src/auth.rs): the sandbox logic
- [src/lib.rs](src/lib.rs): routing and the middleware stack

## Setup (one-time)

```sh
brew install rustup                      # keg-only: add /opt/homebrew/opt/rustup/bin to PATH
rustup default stable
rustup target add wasm32-unknown-unknown
```

The first build installs `worker-build` through the build command in `wrangler.jsonc`, using `~/.cargo/bin`, so that directory must be on `PATH` too.

## Run, test, deploy

```sh
task rust:dev                            # local, http://localhost:8788 (uses ../.dev.vars)
task parity                              # Python vs Rust vs TypeScript, byte for byte
task secrets:rust                        # upload SANDBOX_API_KEYS / SANDBOX_ADMIN_TOKEN
task rust:deploy                         # decision-sandbox-rs.<subdomain>.workers.dev
task bench:rust URL=https://decision-sandbox-rs.<subdomain>.workers.dev
```

The Worker uses its own rate-limit namespaces (3001/3002).

## Measuring cold start fairly

- Each run gives only **one** cold sample (the `first POST` line). Collect 3–5 per Worker. A fresh deploy, or 10+ minutes idle, usually makes the next request cold.
- Cloudflare starts warming a Worker during the TLS handshake, so compare `connect + first POST`, not `first POST` alone.
- Benchmark the three Workers the same way and at the same time.
- Local numbers (`wrangler dev`) say nothing about cold start. Use the deployed Worker.

## Known differences

- JSON nested more than 512 levels deep is rejected with a 400. Python's limit depends on the runtime's stack.
- In the "unknown scenario" message, Python's `repr()` escapes non-printable characters using full Unicode tables. This port covers controls, format characters, separators, surrogates and private use, but treats unassigned code points as printable.
