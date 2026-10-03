import asyncio
from typing import Annotated

from fastapi import Depends, FastAPI, Header, HTTPException, Query, Response
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse
from fastapi.security import HTTPAuthorizationCredentials, HTTPBearer

from sandbox import SANDBOX_MODEL, SANDBOX_VERSION
from sandbox.generator import generate, new_seed
from sandbox.limits import MAX_DELAY_MS, MAX_SEED_LENGTH, TIMEOUT_SCENARIO_MS
from sandbox.middleware import SandboxMiddleware
from sandbox.ratelimit import enforce_rate_limits
from sandbox.scenarios import Scenario, UnknownScenario, catalogue, parse_scenario
from sandbox.schemas import ModelMetadataList, SystemOneRequest, SystemOneResponse

DESCRIPTION = f"""
**A free synthetic decision API for testing Jev-compatible integrations.**

Decision Sandbox implements the public TypeSafe API contract (`POST /v1/systemone`,
`GET /v1/models`) and returns correctly typed **synthetic** answers. They are
randomly generated: no model runs, and nothing here reproduces Jev or any other
model. Probabilities are not calibrated predictions. Do not use them for real
decisions. This project is independent and not affiliated with TypeSafe.

Point your integration at this base URL instead of `https://api.typesafe.ai`.
An API key is optional.

Control the answers with headers (or query parameters):

| Header | Query | Effect |
|---|---|---|
| `X-Sandbox-Seed` | `seed` | Same request + same seed = same response |
| `X-Sandbox-Scenario` | `scenario` | Predefined behaviour. See `GET /sandbox/scenarios` |
| `X-Sandbox-Delay-Ms` | `delay_ms` | Adds latency (max {MAX_DELAY_MS} ms) |

Every response carries `X-Sandbox-Synthetic: true`, `X-Request-Id` and, on decisions,
the `X-Sandbox-Seed` that produced it, so any random answer can be replayed.
"""

app = FastAPI(
    title="Decision Sandbox",
    version=SANDBOX_VERSION,
    description=DESCRIPTION,
    license_info={"name": "MIT"},
)
app.add_middleware(SandboxMiddleware)
app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_methods=["GET", "POST", "OPTIONS"],
    allow_headers=["*"],
    expose_headers=["X-Request-Id", "X-Sandbox-Synthetic", "X-Sandbox-Model", "X-Sandbox-Seed", "X-Sandbox-Scenario", "Retry-After"],
)

# Declares the same security scheme as TypeSafe, but a key is optional here.
bearer = HTTPBearer(auto_error=False)
OptionalAuth = Annotated[HTTPAuthorizationCredentials | None, Depends(bearer)]

RELEASE_DATE = "2026-10-04"
MODELS = ModelMetadataList(
    models=[
        {
            "name": SANDBOX_MODEL,
            "description": "Synthetic decision generator. Returns random, seeded or scenario-driven answers. Not an AI model.",
            "release_date": RELEASE_DATE,
        },
        {
            "name": "jev-latest",
            "description": f"Compatibility alias for {SANDBOX_MODEL} so existing integrations work unchanged. "
            "Answers are synthetic; this is not TypeSafe's Jev model.",
            "release_date": RELEASE_DATE,
        },
    ]
)


@app.get("/", include_in_schema=False)
def index():
    return {
        "name": "Decision Sandbox",
        "description": "A free synthetic decision API for testing Jev-compatible integrations.",
        "synthetic": True,
        "affiliated_with_typesafe": False,
        "model": SANDBOX_MODEL,
        "version": SANDBOX_VERSION,
        "docs": "/docs",
        "openapi": "/openapi.json",
        "endpoints": ["POST /v1/systemone", "GET /v1/models", "GET /sandbox/scenarios", "GET /health"],
    }


@app.get("/health", tags=["sandbox"])
def health():
    return {"status": "ok", "model": SANDBOX_MODEL, "version": SANDBOX_VERSION, "synthetic": True}


@app.get("/sandbox/scenarios", tags=["sandbox"])
def scenarios():
    """List the scenarios accepted by `X-Sandbox-Scenario` / `?scenario=`."""
    return {"scenarios": catalogue()}


@app.get(
    "/v1/models",
    response_model=ModelMetadataList,
    dependencies=[Depends(enforce_rate_limits)],
    tags=["typesafe-compatible"],
    summary="Models V1",
)
def models_v1(_auth: OptionalAuth):
    """List the model names accepted by `POST /v1/systemone`.

    The sandbox accepts any model name; these are listed for discovery.
    """
    return MODELS


@app.post(
    "/v1/systemone",
    response_model=SystemOneResponse,
    dependencies=[Depends(enforce_rate_limits)],
    tags=["typesafe-compatible"],
    summary="Systemone",
    responses={
        400: {"description": "Unknown sandbox scenario or invalid seed"},
        413: {"description": "Request body too large"},
        429: {"description": "Rate limit exceeded (real or simulated)"},
    },
)
async def systemone(
    body: SystemOneRequest,
    response: Response,
    _auth: OptionalAuth,
    seed_header: Annotated[str | None, Header(alias="X-Sandbox-Seed")] = None,
    scenario_header: Annotated[str | None, Header(alias="X-Sandbox-Scenario")] = None,
    delay_header: Annotated[int | None, Header(alias="X-Sandbox-Delay-Ms", ge=0, le=MAX_DELAY_MS)] = None,
    seed: Annotated[str | None, Query(description="Seed for reproducible answers.")] = None,
    scenario: Annotated[str | None, Query(description="Scenario name. See GET /sandbox/scenarios.")] = None,
    delay_ms: Annotated[int | None, Query(ge=0, le=MAX_DELAY_MS, description="Added latency in ms.")] = None,
):
    """Answer one or more questions about the content supplied in `state`.

    Answers are **synthetic**. They are random, or derived from a seed or scenario,
    and are not produced by a model.
    """
    seed = seed if seed is not None else seed_header
    if seed is not None and not 0 < len(seed) <= MAX_SEED_LENGTH:
        raise HTTPException(400, f"Seed must be 1 to {MAX_SEED_LENGTH} characters.")
    seed = seed or new_seed()

    try:
        chosen = parse_scenario(scenario if scenario is not None else scenario_header)
    except UnknownScenario as exc:
        raise HTTPException(
            400, f"Unknown sandbox scenario {str(exc)!r}. See GET /sandbox/scenarios for the list."
        ) from None

    sandbox_headers = {"X-Sandbox-Seed": seed, "X-Sandbox-Scenario": chosen.name}

    delay = delay_ms if delay_ms is not None else delay_header
    if delay:
        await asyncio.sleep(delay / 1000)

    if chosen.is_error:
        return await _simulated_error(chosen, body, sandbox_headers)

    response.headers.update(sandbox_headers)
    return generate(body, seed, chosen)


async def _simulated_error(scenario: Scenario, body: SystemOneRequest, headers: dict[str, str]) -> JSONResponse:
    match scenario.error:
        case "invalid-response":
            # Violates the contract: wrong types, a missing field and an unknown answer type.
            name = next(iter(body.questions))
            content = {
                "model": SANDBOX_MODEL,
                "answers": {name: {"type": "unknown", "noul": "yes", "confidence": None}},
            }
            return JSONResponse(content, status_code=200, headers=headers)
        case "validation-error":
            content = {"detail": [{"loc": ["body", "state"], "msg": "Field required", "type": "missing"}]}
            return JSONResponse(content, status_code=422, headers=headers)
        case "unauthorized":
            return JSONResponse(
                {"detail": "Not authenticated"},
                status_code=401,
                headers={**headers, "WWW-Authenticate": "Bearer"},
            )
        case "rate-limit":
            return JSONResponse(
                {"detail": "Rate limit exceeded (simulated)."},
                status_code=429,
                headers={**headers, "Retry-After": "1"},
            )
        case "unavailable":
            return JSONResponse(
                {"detail": "Service unavailable (simulated)."},
                status_code=503,
                headers={**headers, "Retry-After": "1"},
            )
        case "timeout":
            await asyncio.sleep(TIMEOUT_SCENARIO_MS / 1000)
            return JSONResponse({"detail": "Gateway timeout (simulated)."}, status_code=504, headers=headers)
        case _:
            return JSONResponse({"detail": "Internal server error (simulated)."}, status_code=500, headers=headers)
