"""ASGI middleware: request size limit and sandbox response headers."""

import secrets

from fastapi import HTTPException
from starlette.types import ASGIApp, Message, Receive, Scope, Send

from . import SANDBOX_MODEL
from .limits import MAX_BODY_BYTES

TOO_LARGE = f"Request body exceeds the sandbox limit of {MAX_BODY_BYTES} bytes."


class SandboxMiddleware:
    """Rejects oversized bodies and stamps every response as synthetic."""

    def __init__(self, app: ASGIApp) -> None:
        self.app = app

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        if scope["type"] != "http":
            await self.app(scope, receive, send)
            return

        request_id = "req_" + secrets.token_hex(12)
        scope.setdefault("state", {})["request_id"] = request_id

        async def send_with_headers(message: Message) -> None:
            if message["type"] == "http.response.start":
                headers = list(message.get("headers", []))
                headers += [
                    (b"x-request-id", request_id.encode()),
                    (b"x-sandbox-synthetic", b"true"),
                    (b"x-sandbox-model", SANDBOX_MODEL.encode()),
                ]
                message = {**message, "headers": headers}
            await send(message)

        content_length = dict(scope["headers"]).get(b"content-length")
        if content_length is not None and content_length.isdigit() and int(content_length) > MAX_BODY_BYTES:
            await _reject_too_large(send_with_headers)
            return

        received = 0

        async def limited_receive() -> Message:
            nonlocal received
            message = await receive()
            if message["type"] == "http.request":
                received += len(message.get("body", b""))
                if received > MAX_BODY_BYTES:
                    # FastAPI re-raises HTTPException from body parsing unchanged.
                    raise HTTPException(status_code=413, detail=TOO_LARGE)
            return message

        await self.app(scope, limited_receive, send_with_headers)


async def _reject_too_large(send: Send) -> None:
    body = b'{"detail":"' + TOO_LARGE.encode() + b'"}'
    await send(
        {
            "type": "http.response.start",
            "status": 413,
            "headers": [
                (b"content-type", b"application/json"),
                (b"content-length", str(len(body)).encode()),
            ],
        }
    )
    await send({"type": "http.response.body", "body": body})
