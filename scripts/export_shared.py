"""Export the Python app's OpenAPI spec and docs pages for the TypeScript and Rust ports.

The ports serve these files verbatim, so /openapi.json, /docs and /redoc stay identical
across the three implementations. Re-run after changing routes or schemas: task shared
"""

import sys
from pathlib import Path

from fastapi.testclient import TestClient

sys.path.insert(0, str(Path(__file__).parent.parent / "src"))
from app import app  # noqa: E402

OUT = Path(__file__).parent.parent / "shared"
PAGES = {
    "/openapi.json": "openapi.json",
    "/docs": "docs.html",
    "/docs/oauth2-redirect": "oauth2-redirect.html",
    "/redoc": "redoc.html",
}

client = TestClient(app)
for path, name in PAGES.items():
    res = client.get(path)
    res.raise_for_status()
    (OUT / name).write_bytes(res.content)
    print(f"{path:<24} -> shared/{name} ({len(res.content)} bytes, {res.headers['content-type']})")
