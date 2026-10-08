"""Contract tests: every response must validate against TypeSafe's published OpenAPI spec."""

import json
from pathlib import Path

import pytest
from fastapi.testclient import TestClient
from jsonschema import Draft202012Validator
from referencing import Registry
from referencing.jsonschema import DRAFT202012

from app import app
from sandbox.limits import IP_REQUESTS_PER_MINUTE
from sandbox.ratelimit import local_limiter

SPEC = json.loads((Path(__file__).parent / "typesafe_openapi.json").read_text())
REGISTRY = Registry().with_resource("typesafe", DRAFT202012.create_resource(SPEC))


def schema_validator(name: str) -> Draft202012Validator:
    return Draft202012Validator({"$ref": f"typesafe#/components/schemas/{name}"}, registry=REGISTRY)


RESPONSE = schema_validator("SystemOneResponse")
MODELS = schema_validator("ModelMetadataList")
VALIDATION_ERROR = schema_validator("HTTPValidationError")

REQUEST = {
    "model": "jev-latest",
    "state": {"subject": "Duplicate charge", "message": "I was charged twice. Please help."},
    "questions": {
        "billing": {"type": "noul", "instructions": "Is this message about billing?"},
        "spam": {
            "type": "noul",
            "instructions": "Is this message spam?",
            "criteria": {"true": "Unsolicited advertising", "false": "A legitimate conversation"},
        },
        "tone": {
            "type": "choice",
            "instructions": "What is the tone of this message?",
            "criteria": {"angry": "An upset or hostile message", "calm": None, "excited": {"hint": "eager"}},
        },
        "urgency": {
            "type": "score",
            "instructions": "How urgent is this message?",
            "criteria": ["Can wait", "Needs attention this week", "Needs attention today"],
        },
    },
}


@pytest.fixture
def client():
    local_limiter.reset()
    return TestClient(app, headers={"Authorization": "Bearer github-2026"})


def post(client, body=REQUEST, **kwargs):
    return client.post("/v1/systemone", json=body, **kwargs)


def test_request_example_is_valid_typesafe_request():
    assert not list(schema_validator("SystemOneRequest").iter_errors(REQUEST))


def test_response_matches_typesafe_schema(client):
    res = post(client)
    assert res.status_code == 200
    RESPONSE.validate(res.json())
    assert list(res.json()["answers"]) == list(REQUEST["questions"])
    assert res.headers["x-sandbox-synthetic"] == "true"
    assert res.headers["x-request-id"].startswith("req_")
    assert res.json()["model"] == "decision-sandbox-v1"


@pytest.mark.parametrize("scenario", ["random", "high-confidence", "low-confidence", "always-true", "always-false",
                                      "first-choice", "last-choice", "min-score", "max-score", "choice:calm", "score:1"])
def test_every_answer_scenario_is_schema_valid(client, scenario):
    for seed in range(25):
        res = post(client, params={"scenario": scenario, "seed": str(seed)})
        assert res.status_code == 200, res.text
        RESPONSE.validate(res.json())
        for answer in res.json()["answers"].values():
            if "probabilities" in answer:
                assert abs(sum(answer["probabilities"].values()) - 1) < 0.01
                assert answer["confidence"] == max(answer["probabilities"].values())


def test_choice_is_highest_probability(client):
    for seed in range(50):
        tone = post(client, params={"seed": str(seed)}).json()["answers"]["tone"]
        assert tone["probabilities"][tone["choice"]] == max(tone["probabilities"].values())


def test_score_is_expected_value_and_legend_matches(client):
    urgency = post(client, params={"seed": "x"}).json()["answers"]["urgency"]
    assert urgency["legend"] == {"0": "Can wait", "1": "Needs attention this week", "2": "Needs attention today"}
    expected = sum(int(k) * p for k, p in urgency["probabilities"].items())
    assert urgency["score"] == pytest.approx(expected, abs=1e-3)


def test_seeded_responses_are_reproducible(client):
    a = post(client, headers={"X-Sandbox-Seed": "ci-42"})
    b = post(client, params={"seed": "ci-42"})
    assert a.json() == b.json()
    assert a.headers["x-sandbox-seed"] == "ci-42"


def test_random_response_returns_replayable_seed(client):
    first = post(client)
    replay = post(client, headers={"X-Sandbox-Seed": first.headers["x-sandbox-seed"]})
    assert first.json() == replay.json()


def test_unseeded_responses_vary(client):
    bodies = {json.dumps(post(client).json(), sort_keys=True) for _ in range(5)}
    assert len(bodies) > 1


def test_adding_a_question_does_not_change_other_answers(client):
    extended = {**REQUEST, "questions": {**REQUEST["questions"], "extra": {"type": "noul"}}}
    a = post(client, params={"seed": "s"}).json()["answers"]
    b = post(client, body=extended, params={"seed": "s"}).json()["answers"]
    assert all(a[name] == b[name] for name in a)


def test_model_name_does_not_affect_answers(client):
    a = post(client, params={"seed": "s"}).json()
    b = post(client, body={**REQUEST, "model": "decision-sandbox-v1"}, params={"seed": "s"}).json()
    assert a == b


def test_answer_scenarios_shape_values(client):
    def answers(scenario):
        return post(client, params={"scenario": scenario}).json()["answers"]

    assert answers("always-true")["billing"]["noul"] > 0.9
    assert answers("always-false")["billing"]["noul"] < 0.1
    assert 0.4 <= answers("low-confidence")["billing"]["noul"] <= 0.6
    assert answers("first-choice")["tone"]["choice"] == "angry"
    assert answers("last-choice")["tone"]["choice"] == "excited"
    assert answers("choice:calm")["tone"]["choice"] == "calm"
    assert answers("max-score")["urgency"]["probabilities"]["2"] > 0.5
    assert answers("score:99")["urgency"]["probabilities"]["2"] > 0.5
    assert answers("high-confidence")["tone"]["confidence"] > 0.8


@pytest.mark.parametrize(
    ("scenario", "status"),
    [("validation-error", 422), ("unauthorized", 401), ("rate-limit", 429), ("server-error", 500), ("unavailable", 503)],
)
def test_error_scenarios(client, scenario, status):
    res = post(client, headers={"X-Sandbox-Scenario": scenario})
    assert res.status_code == status
    assert "detail" in res.json()
    assert res.headers["x-sandbox-scenario"] == scenario
    if status == 422:
        VALIDATION_ERROR.validate(res.json())


def test_invalid_response_scenario_violates_schema(client):
    res = post(client, params={"scenario": "invalid-response"})
    assert res.status_code == 200
    assert not RESPONSE.is_valid(res.json())


def test_unknown_scenario_is_rejected(client):
    res = post(client, params={"scenario": "nope"})
    assert res.status_code == 400
    assert "/sandbox/scenarios" in res.json()["detail"]


@pytest.mark.parametrize(
    "body",
    [
        {k: v for k, v in REQUEST.items() if k != "state"},
        {**REQUEST, "questions": {}},
        {**REQUEST, "questions": {"q": {"type": "score", "criteria": []}}},
        {**REQUEST, "questions": {"q": {"type": "bogus"}}},
        {**REQUEST, "questions": {"q": {"type": "noul", "instructions": 5}}},
        {**REQUEST, "state": None},
    ],
)
def test_invalid_requests_return_typesafe_validation_errors(client, body):
    res = post(client, body=body)
    assert res.status_code == 422
    VALIDATION_ERROR.validate(res.json())


def test_missing_state_error_matches_spec_example(client):
    body = {k: v for k, v in REQUEST.items() if k != "state"}
    detail = post(client, body=body).json()["detail"][0]
    assert detail["loc"] == ["body", "state"] and detail["type"] == "missing"


def test_limits(client):
    too_many = {**REQUEST, "questions": {f"q{i}": {"type": "noul"} for i in range(33)}}
    assert post(client, body=too_many).status_code == 422
    too_many_choices = {**REQUEST, "questions": {"q": {"type": "choice", "criteria": {str(i): None for i in range(65)}}}}
    assert post(client, body=too_many_choices).status_code == 422
    huge = {**REQUEST, "state": "x" * 70_000}
    res = post(client, body=huge)
    assert res.status_code == 413
    assert post(client, params={"delay_ms": 60_000}).status_code == 422


def test_rate_limit_per_ip(client):
    limit = IP_REQUESTS_PER_MINUTE
    statuses = [client.get("/v1/models").status_code for _ in range(limit + 1)]
    assert statuses[:limit] == [200] * limit
    assert statuses[limit] == 429


def test_models(client):
    res = client.get("/v1/models")
    MODELS.validate(res.json())
    assert {m["name"] for m in res.json()["models"]} == {"decision-sandbox-v1", "jev-latest"}


def test_health_and_scenarios(client):
    assert client.get("/health").json()["status"] == "ok"
    names = {s["name"] for s in client.get("/sandbox/scenarios").json()["scenarios"]}
    assert {"random", "timeout", "choice:<name>"} <= names


def test_our_openapi_keeps_typesafe_paths_and_security(client):
    ours = client.get("/openapi.json").json()
    for path, method in [("/v1/systemone", "post"), ("/v1/models", "get")]:
        assert method in ours["paths"][path]
        assert ours["paths"][path][method]["security"] == [{"HTTPBearer": []}]
