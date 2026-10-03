"""Per-IP and per-API-key rate limiting.

On Cloudflare this uses the Workers rate-limit bindings declared in wrangler.jsonc,
so no database is needed. Outside the Workers runtime (unit tests, plain uvicorn)
it falls back to a small in-memory fixed window.
"""

import hashlib
import logging
import time

from fastapi import HTTPException, Request

from .limits import IP_REQUESTS_PER_MINUTE, KEY_REQUESTS_PER_MINUTE

logger = logging.getLogger(__name__)

WINDOW_SECONDS = 60


class LocalLimiter:
    def __init__(self) -> None:
        self._windows: dict[str, tuple[int, int]] = {}

    def allow(self, key: str, limit: int) -> bool:
        window = int(time.time()) // WINDOW_SECONDS
        start, count = self._windows.get(key, (window, 0))
        if start != window:
            if len(self._windows) > 10_000:
                self._windows.clear()
            count = 0
        self._windows[key] = (window, count + 1)
        return count < limit

    def reset(self) -> None:
        self._windows.clear()


local_limiter = LocalLimiter()


async def _allow(env, binding_name: str, key: str, fallback_limit: int) -> bool:
    binding = getattr(env, binding_name, None) if env is not None else None
    if binding is None:
        return local_limiter.allow(key, fallback_limit)
    try:
        import js
        from pyodide.ffi import to_js

        outcome = await binding.limit(to_js({"key": key}, dict_converter=js.Object.fromEntries))
        return bool(outcome.success)
    except Exception:
        # Fail open: an unavailable limiter should not take the sandbox down.
        logger.exception("Rate limiter %s failed", binding_name)
        return True


def client_ip(request: Request) -> str:
    return (
        request.headers.get("cf-connecting-ip")
        or (request.client.host if request.client else None)
        or "unknown"
    )


def api_key(request: Request) -> str | None:
    scheme, _, token = request.headers.get("authorization", "").partition(" ")
    if scheme.lower() != "bearer":
        return None
    return token.strip() or None


async def enforce_rate_limits(request: Request) -> None:
    env = request.scope.get("env")
    checks = [("IP_RATE_LIMITER", f"ip:{client_ip(request)}", IP_REQUESTS_PER_MINUTE)]
    if key := api_key(request):
        # Only a hash of the key is used; keys are never stored or logged.
        digest = hashlib.sha256(key.encode()).hexdigest()[:32]
        checks.append(("KEY_RATE_LIMITER", f"key:{digest}", KEY_REQUESTS_PER_MINUTE))

    for binding_name, key, limit in checks:
        if not await _allow(env, binding_name, key, limit):
            raise HTTPException(
                status_code=429,
                detail="Rate limit exceeded. Decision Sandbox is a free shared service; please slow down.",
                headers={"Retry-After": str(WINDOW_SECONDS)},
            )
