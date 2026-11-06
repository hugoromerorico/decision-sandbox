"""Latency benchmark for a running Decision Sandbox instance.

Sends 16 requests in total, well under the 60/min per-IP limit:
  1 cold POST /v1/systemone on a new connection (connect time reported separately)
 10 warm POST /v1/systemone on the same keep-alive connection
  5 warm GET /health, a baseline where the app does almost no work

POST minus /health approximates the app's own cost. Prod minus dev approximates
network and platform overhead.

Runs against deployed (https) targets are appended to bench/results.csv, so every
cold sample ends up in one place. Set BENCH_NOTE to annotate the row.

Usage: python bench/latency.py https://decision-sandbox.gutan.dev
The API key defaults to the free public key; override it with SANDBOX_API_KEY.
"""

import csv
import http.client
import json
import os
import statistics
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from urllib.parse import urlsplit

POSTS = 10
HEALTH_CHECKS = 5

BODY = json.dumps({
    "model": "jev-latest",
    "state": "I was charged twice. Please help.",
    "questions": {
        "billing": {"type": "noul", "instructions": "Is this message about billing?"},
    },
}).encode()
API_KEY = os.environ.get("SANDBOX_API_KEY", "github-2026")
RESULTS = Path(__file__).with_name("results.csv")
FIELDS = ["recorded_at", "language", "url", "connect_ms", "first_post_ms", "connect_plus_first_ms",
          "post_median_ms", "post_min_ms", "post_max_ms", "health_median_ms", "app_ms", "note"]
LANGUAGES = {
    "decision-sandbox.gutan.dev": "python",
    "decision-sandbox.2romerohugo.workers.dev": "python",
    "decision-sandbox-rs.2romerohugo.workers.dev": "rust",
    "decision-sandbox-ts.2romerohugo.workers.dev": "typescript",
}
HEADERS = {
    "Authorization": f"Bearer {API_KEY}",
    "Content-Type": "application/json",
    "Connection": "keep-alive",
}


def connect(url: str) -> tuple[http.client.HTTPConnection, float]:
    parts = urlsplit(url)
    cls = http.client.HTTPSConnection if parts.scheme == "https" else http.client.HTTPConnection
    conn = cls(parts.netloc, timeout=15)
    start = time.perf_counter()
    conn.connect()
    return conn, (time.perf_counter() - start) * 1000


def timed(conn: http.client.HTTPConnection, method: str, path: str) -> float:
    start = time.perf_counter()
    conn.request(method, path, body=BODY if method == "POST" else None, headers=HEADERS)
    res = conn.getresponse()
    res.read()
    elapsed = (time.perf_counter() - start) * 1000
    if res.status != 200:
        sys.exit(f"{method} {path} returned HTTP {res.status}, stopping")
    return elapsed


def summary(label: str, samples: list[float]) -> str:
    return (f"{label:<22} n={len(samples):<3} min {min(samples):7.1f}  median {statistics.median(samples):7.1f}"
            f"  mean {statistics.mean(samples):7.1f}  max {max(samples):7.1f} ms")


def record(url: str, connect_ms: float, cold_ms: float, posts: list[float], health: list[float]) -> None:
    new = not RESULTS.exists()
    with RESULTS.open("a", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=FIELDS)
        if new:
            writer.writeheader()
        writer.writerow({
            "recorded_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "language": LANGUAGES.get(urlsplit(url).hostname or "", "unknown"),
            "url": url,
            "connect_ms": f"{connect_ms:.1f}",
            "first_post_ms": f"{cold_ms:.1f}",
            "connect_plus_first_ms": f"{connect_ms + cold_ms:.1f}",
            "post_median_ms": f"{statistics.median(posts):.1f}",
            "post_min_ms": f"{min(posts):.1f}",
            "post_max_ms": f"{max(posts):.1f}",
            "health_median_ms": f"{statistics.median(health):.1f}",
            "app_ms": f"{statistics.median(posts) - statistics.median(health):.1f}",
            "note": os.environ.get("BENCH_NOTE", ""),
        })
    print(f"Recorded in {RESULTS}")


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    url = sys.argv[1].rstrip("/")

    try:
        conn, connect_ms = connect(url)
    except OSError as exc:
        sys.exit(f"Cannot reach {url}: {exc}")

    try:
        cold_ms = timed(conn, "POST", "/v1/systemone")
        posts = [timed(conn, "POST", "/v1/systemone") for _ in range(POSTS)]
        health = [timed(conn, "GET", "/health") for _ in range(HEALTH_CHECKS)]
    finally:
        conn.close()

    print(f"Target: {url}")
    print(f"{'connect (TCP, +TLS)':<22} {connect_ms:7.1f} ms")
    print(f"{'first POST (cold)':<22} {cold_ms:7.1f} ms")
    print(summary("POST /v1/systemone", posts))
    print(summary("GET /health", health))
    app_ms = statistics.median(posts) - statistics.median(health)
    print(f"{'app cost (median diff)':<22} {app_ms:7.1f} ms")
    if url.startswith("https://"):
        record(url, connect_ms, cold_ms, posts, health)


if __name__ == "__main__":
    main()
