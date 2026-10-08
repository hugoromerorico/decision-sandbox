import pytest
from fastapi.testclient import TestClient

from app import app
from sandbox import auth
from sandbox.ratelimit import local_limiter

BODY = {"model": "jev-latest", "state": "hi", "questions": {"q": {"type": "noul"}}}


@pytest.fixture
def client(monkeypatch):
    local_limiter.reset()
    auth.metrics.reset()
    env = {"SANDBOX_API_KEYS": "linkedin-2026, community-2026", "SANDBOX_ADMIN_TOKEN": "admin"}
    monkeypatch.setattr(auth, "_env_value", lambda request, name: env.get(name, ""))
    return TestClient(app)


def bearer(key):
    return {"Authorization": f"Bearer {key}"}


def test_missing_key_is_401(client):
    res = client.post("/v1/systemone", json=BODY)
    assert res.status_code == 401
    assert res.json() == {"detail": "Not authenticated"}
    assert res.headers["www-authenticate"] == "Bearer"
    assert client.get("/v1/models").status_code == 401


def test_unknown_key_is_401(client):
    assert client.post("/v1/systemone", json=BODY, headers=bearer("nope-2026")).status_code == 401


@pytest.mark.parametrize("key", ["github-2026", "linkedin-2026", "community-2026"])
def test_allowlisted_keys_work(client, key):
    assert client.post("/v1/systemone", json=BODY, headers=bearer(key)).status_code == 200


def test_open_endpoints_need_no_key(client):
    assert client.get("/health").status_code == 200
    assert client.get("/sandbox/scenarios").status_code == 200


def test_stats(client, monkeypatch):
    client.post("/v1/systemone", json=BODY, headers=bearer("linkedin-2026"))
    client.post("/v1/systemone", json=BODY, headers=bearer("linkedin-2026"))
    client.get("/v1/models", headers=bearer("github-2026"))
    client.get("/v1/models", headers=bearer("bad"))

    assert client.get("/sandbox/stats").status_code == 401
    assert client.get("/sandbox/stats", headers=bearer("wrong")).status_code == 401
    data = client.get("/sandbox/stats", headers=bearer("admin")).json()
    assert data["total"] == 3 and data["unauthorized"] == 1
    assert data["keys"]["linkedin-2026"]["requests"] == 2
    assert data["keys"]["github-2026"]["by_endpoint"] == {"/v1/models": 1}

    monkeypatch.setattr(auth, "_env_value", lambda request, name: "")
    assert client.get("/sandbox/stats", headers=bearer("admin")).status_code == 404
