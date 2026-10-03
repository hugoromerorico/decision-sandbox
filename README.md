# decision-sandbox

A minimal [FastAPI](https://fastapi.tiangolo.com/) backend that runs on [Cloudflare Python Workers](https://developers.cloudflare.com/workers/languages/python/).

## Project layout

```
.
├── src/
│   ├── app.py          # FastAPI application (routes live here)
│   └── entry.py        # Cloudflare Worker entrypoint (wraps the app as ASGI)
├── pyproject.toml      # Project metadata and dependencies
├── wrangler.jsonc      # Cloudflare Worker configuration
├── Taskfile.yml        # Common dev commands
├── .python-version     # Python version used locally (matches the Workers runtime)
├── uv.lock             # Lockfile for the local environment
└── pylock.toml         # Lockfile for the Worker (Pyodide) environment, managed by pywrangler
```

## Requirements

- [uv](https://docs.astral.sh/uv/) ≥ 0.12.3
- [Node.js](https://nodejs.org/), which pywrangler needs to run `wrangler`
- [Task](https://taskfile.dev/) (optional; you can also run the commands below directly)

## Python version

The Workers runtime uses **Python 3.14**. It is selected by `compatibility_date` in `wrangler.jsonc`, since any date on or after `2026-09-08` targets 3.14. To keep the local environment on the same version, `.python-version` and `requires-python` in `pyproject.toml` are both pinned to 3.14. If you change one of these, update the others to match.

## Getting started

```sh
task sync     # uv sync && uv run pywrangler sync
task dev      # http://localhost:8787
```

The interactive API docs are served at `/docs`.

## Deploying

```sh
uv run pywrangler login   # first time only
task deploy
```

## Adding dependencies

```sh
uv add <package>
task sync
```

Packages must be pure Python or have a [Pyodide](https://pyodide.org/) build available. `pywrangler sync` resolves them for the Worker and updates `pylock.toml`.
