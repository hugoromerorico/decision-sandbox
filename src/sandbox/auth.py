"""API key allowlist and in-memory per-key request counters.

Keys identify the traffic source (e.g. "linkedin-2026"). The allowlist is the public
key below plus the comma-separated SANDBOX_API_KEYS secret. Counters live in memory
only: on Cloudflare they are per isolate and reset on eviction or deploy, so the
structured log line emitted per request is the durable source of truth.
"""

import hmac
import json
import logging
import time

from fastapi import HTTPException, Request

from .ratelimit import api_key

logger = logging.getLogger(__name__)

PUBLIC_KEYS = frozenset({"github-2026"})


def _env_value(request: Request, name: str) -> str:
    env = request.scope.get("env")
    value = getattr(env, name, None) if env is not None else None
    return str(value) if value else ""


def allowlist(request: Request) -> set[str]:
    extra = {k.strip() for k in _env_value(request, "SANDBOX_API_KEYS").split(",") if k.strip()}
    return set(PUBLIC_KEYS) | extra


class Metrics:
    def __init__(self) -> None:
        self.reset()

    def reset(self) -> None:
        self.started_at = int(time.time())
        self.unauthorized = 0
        self.keys: dict[str, dict] = {}

    def record(self, key: str, endpoint: str) -> None:
        entry = self.keys.setdefault(key, {"requests": 0, "by_endpoint": {}, "last_seen": 0})
        entry["requests"] += 1
        entry["by_endpoint"][endpoint] = entry["by_endpoint"].get(endpoint, 0) + 1
        entry["last_seen"] = int(time.time())

    def snapshot(self) -> dict:
        return {
            "started_at": self.started_at,
            "total": sum(e["requests"] for e in self.keys.values()),
            "unauthorized": self.unauthorized,
            "keys": self.keys,
            "note": "In-memory, per isolate; resets on eviction or deploy.",
        }


metrics = Metrics()


def _match(candidate: str, keys: set[str]) -> str | None:
    found = None
    for k in keys:  # compare against all keys to avoid early exit
        if hmac.compare_digest(candidate.encode(), k.encode()):
            found = k
    return found


def _unauthorized() -> HTTPException:
    return HTTPException(401, "Not authenticated", headers={"WWW-Authenticate": "Bearer"})


async def require_api_key(request: Request) -> str:
    candidate = api_key(request)
    key = _match(candidate, allowlist(request)) if candidate else None
    if key is None:
        metrics.unauthorized += 1
        raise _unauthorized()
    metrics.record(key, request.url.path)
    logger.info(json.dumps({"event": "request", "key_label": key, "endpoint": request.url.path}))
    return key


async def require_admin(request: Request) -> None:
    token = _env_value(request, "SANDBOX_ADMIN_TOKEN")
    if not token:
        raise HTTPException(404, "Not found")
    candidate = api_key(request)
    if not candidate or not hmac.compare_digest(candidate.encode(), token.encode()):
        raise _unauthorized()
