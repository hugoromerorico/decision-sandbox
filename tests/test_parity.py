"""Differential tests: the TypeScript and Rust Workers must answer exactly like the Python one.

Runs only when PARITY_URLS lists running Workers, reference (Python) first:

    PARITY_URLS=http://localhost:8797,http://localhost:8798,http://localhost:8799 uv run pytest tests/test_parity.py

`task parity` starts the three dev servers with the variables below and runs this file.
Every case is sent to every Worker; status, headers and body bytes must be identical.
Each request uses its own CF-Connecting-IP (honoured by `wrangler dev`) and the keys
rotate, so the local rate limiters never trip except where a test wants them to.
"""

import itertools
import json
import os
import random
import re
import string
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from urllib.parse import urlsplit

import httpx2 as httpx
import pytest
from jsonschema import Draft202012Validator
from referencing import Registry
from referencing.jsonschema import DRAFT202012
from pathlib import Path

URLS = [u.rstrip("/") for u in os.environ.get("PARITY_URLS", "").split(",") if u.strip()]
pytestmark = pytest.mark.skipif(len(URLS) < 2, reason="set PARITY_URLS to two or more running Workers")

# Must match the --var flags in Taskfile.yml (task parity).
KEYS = [f"parity-{i}" for i in range(32)]
ADMIN_TOKEN = "parity-admin"
FUZZ_SEED = int(os.environ.get("PARITY_SEED", "20261105"))
FUZZ_CASES = int(os.environ.get("PARITY_CASES", "400"))

SPEC = json.loads((Path(__file__).parent / "typesafe_openapi.json").read_text())
REGISTRY = Registry().with_resource("typesafe", DRAFT202012.create_resource(SPEC))
RESPONSE = Draft202012Validator({"$ref": "typesafe#/components/schemas/SystemOneResponse"}, registry=REGISTRY)
VALIDATION_ERROR = Draft202012Validator({"$ref": "typesafe#/components/schemas/HTTPValidationError"}, registry=REGISTRY)

# Transport headers that legitimately differ between runtimes and dev servers.
IGNORED_HEADERS = {"date", "content-length", "transfer-encoding", "connection", "keep-alive", "server", "x-request-id"}

_ip = itertools.count(1)
_key = itertools.cycle(KEYS)


def fresh_ip() -> str:
    n = next(_ip)
    return f"10.{(n >> 16) & 255}.{(n >> 8) & 255}.{n & 255}"


@dataclass
class Case:
    name: str
    method: str = "POST"
    path: str = "/v1/systemone"
    body: bytes | None = None
    headers: dict[str, str] = field(default_factory=dict)
    params: list[tuple[str, str]] = field(default_factory=list)
    key: str | None = "rotate"  # "rotate", a specific key, or None for no Authorization header
    # The Python dev server answers a crash with a traceback; only the status is compared.
    status_only: bool = False


@dataclass
class Result:
    status: int
    headers: dict[str, str]
    body: bytes


def send(client: httpx.Client, base: str, case: Case, ip: str, key: str | None) -> Result:
    headers = {"CF-Connecting-IP": ip, **case.headers}
    if key is not None and "Authorization" not in headers:
        headers["Authorization"] = f"Bearer {key}"
    if case.body is not None and "Content-Type" not in headers:
        headers["Content-Type"] = "application/json"
    res = client.request(case.method, base + case.path, content=case.body, headers=headers, params=case.params)
    return Result(res.status_code, {k.lower(): v for k, v in res.headers.items()}, res.content)


def normalise(result: Result, base: str) -> Result:
    headers = {k: v for k, v in result.headers.items() if k not in IGNORED_HEADERS}
    if "location" in headers:
        headers["location"] = headers["location"].replace(urlsplit(base).netloc, "HOST")
    return Result(result.status, headers, result.body)


def run(case: Case) -> list[Result]:
    """Sends the case to every Worker (same IP and key for each) and checks invariants."""
    ip = fresh_ip()
    key = next(_key) if case.key == "rotate" else case.key
    with httpx.Client(timeout=30) as client:
        results = [send(client, base, case, ip, key) for base in URLS]
    for base, res in zip(URLS, results):
        if res.status != 500 or not case.status_only:
            assert re.fullmatch(r"req_[0-9a-f]{24}", res.headers.get("x-request-id", "req_" + "0" * 24)), base
    return results


def assert_same(case: Case, results: list[Result]) -> None:
    reference = normalise(results[0], URLS[0])
    for base, res in zip(URLS[1:], results[1:]):
        got = normalise(res, base)
        where = f"{case.name}: {URLS[0]} vs {base}"
        assert got.status == reference.status, f"{where}\n{reference.body[:500]!r}\n{got.body[:500]!r}"
        if case.status_only and reference.status == 500:
            continue
        assert got.headers == reference.headers, where
        assert got.body == reference.body, f"{where}\n{reference.body[:2000]!r}\n{got.body[:2000]!r}"


def check(case: Case) -> list[Result]:
    results = run(case)
    assert_same(case, results)
    ref = results[0]
    if case.path == "/v1/systemone" and case.method == "POST" and ref.headers.get("x-sandbox-scenario") != "invalid-response":
        if ref.status == 200:
            RESPONSE.validate(json.loads(ref.body))
        elif ref.status == 422:
            VALIDATION_ERROR.validate(json.loads(ref.body))
    return results


def body(value) -> bytes:
    return json.dumps(value).encode()


BASE = {
    "model": "jev-latest",
    "state": {"subject": "Duplicate charge", "message": "I was charged twice. Please help."},
    "questions": {
        "billing": {"type": "noul", "instructions": "Is this message about billing?"},
        "spam": {"type": "noul", "criteria": {"true": "Unsolicited advertising", "false": "A legitimate conversation"}},
        "tone": {"type": "choice", "criteria": {"angry": "An upset message", "calm": None, "excited": {"hint": "eager"}}},
        "urgency": {"type": "score", "criteria": ["Can wait", "Needs attention this week", "Needs attention today"]},
    },
}
SMALL = {"model": "m", "state": "s", "questions": {"q": {"type": "noul"}}}

ANSWER_SCENARIOS = ["random", "high-confidence", "low-confidence", "always-true", "always-false", "first-choice",
                    "last-choice", "min-score", "max-score", "choice:calm", "score:1", "score:-3", "score:99"]
ERROR_SCENARIOS = ["invalid-response", "validation-error", "unauthorized", "rate-limit", "server-error", "unavailable"]


def fixed_cases() -> list[Case]:
    cases = [
        Case("seeded", body=body(BASE), params=[("seed", "ci-42")]),
        Case("seed header", body=body(BASE), headers={"X-Sandbox-Seed": "ci-42"}),
        Case("query beats header", body=body(BASE), params=[("seed", "a")], headers={"X-Sandbox-Seed": "b"}),
        Case("repeated query", body=body(BASE), params=[("seed", "a"), ("seed", "b")]),
        Case("unicode seed in range", body=body(BASE), params=[("seed", "🎉" * 128)], status_only=True),
        Case("non-ascii seed (Python 500)", body=body(BASE), params=[("seed", "é")], status_only=True),
        Case("seed too long", body=body(BASE), params=[("seed", "🎉" * 129)]),
        Case("seed empty", body=body(BASE), params=[("seed", "")]),
        Case("scenario empty", body=body(SMALL), params=[("seed", "1"), ("scenario", "")]),
        Case("scenario spaces", body=body(SMALL), params=[("scenario", "  ")]),
        Case("scenario stripped", body=body(SMALL), params=[("seed", "1"), ("scenario", "\t random \n")]),
        Case("scenario score: 2 ", body=body(BASE), params=[("seed", "1"), ("scenario", " score: 2 ")]),
        Case("scenario score:1_0", body=body(BASE), params=[("seed", "1"), ("scenario", "score:1_0")]),
        Case("scenario score:x", body=body(BASE), params=[("scenario", "score:x")]),
        Case("scenario choice:", body=body(BASE), params=[("scenario", "choice:")]),
        Case("scenario choice:a:b", body=body(BASE), params=[("seed", "1"), ("scenario", "choice:a:b")]),
        Case("scenario repr", body=body(BASE), params=[("scenario", "a\\b\x01\"'é​\U0001f389")]),
        Case("scenario repr dq", body=body(BASE), params=[("scenario", "it's")]),
        Case("scenario header", body=body(BASE), headers={"X-Sandbox-Scenario": "max-score", "X-Sandbox-Seed": "z"}),
        *[Case(f"delay {d!r}", body=body(SMALL), params=[("seed", "1"), ("delay_ms", d)]) for d in
          ["0", "1.0", " 2 ", "1_0", "+3", "-0", "00012", "1.00", "1e3", "0x10", "١", "1.", "", ".0", " ",
           "1 2", "5001", "5000.0", "1_", "_1", "+-1", "1__0", "1.0_0", "-1", "99999999999999999999", "\xa05", "\x1c5"]],
        Case("delay header", body=body(SMALL), headers={"X-Sandbox-Delay-Ms": "6000"}),
        Case("delay all errors", body=b"{}", params=[("delay_ms", "x")], headers={"X-Sandbox-Delay-Ms": "-5"}),
        *[Case(f"error scenario {s}", body=body(BASE), params=[("seed", "e"), ("scenario", s)]) for s in ERROR_SCENARIOS],
        Case("no key", body=body(SMALL), key=None),
        Case("wrong key", body=body(SMALL), key="nope"),
        Case("lowercase scheme", body=body(SMALL), params=[("seed", "1")], headers={"Authorization": f"bearer   {KEYS[1]}"}),
        Case("tab after scheme", body=body(SMALL), headers={"Authorization": f"Bearer\t{KEYS[1]}"}),
        Case("public key", body=body(SMALL), params=[("seed", "1")], key="github-2026"),
        Case("bad json before auth", body=b"{", key=None),
        Case("413 by length", body=body({**SMALL, "state": "x" * 70_000})),
        Case("413 on GET", method="GET", path="/v1/models", body=b"x" * 70_000),
        Case("no content type", body=body(SMALL), headers={"Content-Type": ""}),
        Case("text/plain", body=body(SMALL), headers={"Content-Type": "text/plain"}),
        Case("text/plain bad utf8", body=b"\xff", headers={"Content-Type": "text/plain"}),
        Case("+json", body=body(SMALL), params=[("seed", "1")], headers={"Content-Type": "application/vnd.api+json; charset=utf-8"}),
        Case("JSON upper", body=body(SMALL), params=[("seed", "1")], headers={"Content-Type": "Application/JSON"}),
        Case("content type json", body=body(SMALL), headers={"Content-Type": "json"}),
        Case("empty body", body=b""),
        Case("null body", body=b"null"),
        Case("list body", body=b"[1]"),
        Case("bad utf8", body=b'{"a":"\xff"}'),
        Case("bom", body=b"\xef\xbb\xbf" + body(SMALL), params=[("seed", "1")]),
        Case("double bom", body=b"\xef\xbb\xbf\xef\xbb\xbf{}"),
        Case("deep nesting", body=b"[" * 400 + b"]" * 400),
        Case("huge int", body=b'{"model":"m","state":[' + b"1" * 4301 + b'],"questions":{"q":{"type":"noul"}}}'),
        Case("nan state", body=b'{"model":"m","state":[NaN,Infinity,-Infinity],"questions":{"q":{"type":"noul"}}}', params=[("seed", "1")]),
        Case("nan in error echo (Python 500)", body=b'{"model":"m","state":NaN,"questions":{"q":{"type":"noul"}}}'),
        Case("nan in legend", body=b'{"model":"m","state":"s","questions":{"q":{"type":"score","criteria":[[NaN,1e400]]}}}', params=[("seed", "1")]),
        Case("nan in instructions", body=b'{"model":"m","state":"s","questions":{"q":{"type":"noul","instructions":[NaN]}}}', params=[("seed", "1")]),
        Case("lone surrogate in model", body=b'{"model":"\\ud800","state":"s","questions":{"q":{"type":"noul"}}}', params=[("seed", "1")]),
        Case("lone surrogate in state", body=b'{"model":"m","state":"\\ud800","questions":{"q":{"type":"noul"}}}', params=[("seed", "1")]),
        Case("lone surrogate in error", body=b'{"model":1,"state":"\\udc00x","questions":{"q":{"type":"noul"}}}'),
        Case("surrogate pair", body=b'{"model":"m","state":"\\ud83c\\udf89\\u00e9","questions":{"\\ud83c\\udf89":{"type":"noul"}}}', params=[("seed", "1")]),
        Case("legend floats", body=b'{"model":"m","state":"s","questions":{"q":{"type":"score","criteria":[[1e15,1e16,1.5e16,1e-4,1e-5,1.5e-5,1e-6,123456789012345680000.0,0.1,5e-324,-0.0,-0,1E2,12345678901234567890123]]}}}', params=[("seed", "1")]),
        Case("error echo floats", body=b'{"model":[1e-5,1e16,1E2,-0.0,1.0],"state":"s","questions":{"q":{"type":"noul"}}}'),
        Case("duplicate keys", body=b'{"model":"m","state":"s","questions":{"b":{"type":"noul"},"a":{"type":"noul"},"b":{"type":"score","criteria":["x"]}}}', params=[("seed", "1")]),
        Case("integer-like keys", body=b'{"model":"m","state":{"z":1.0,"10":2,"2":3},"questions":{"2":{"type":"noul"},"1":{"type":"choice","criteria":{"9":null,"3":null,"10":"x"}}}}', params=[("seed", "1"), ("scenario", "last-choice")]),
        Case("extra fields", body=body({**SMALL, "extra": 1, "questions": {"q": {"type": "noul", "x": 1}}}), params=[("seed", "1")]),
        Case("40 duplicate questions", body=('{"model":"m","state":"s","questions":{' + ",".join(['"a":{"type":"noul"}'] * 40) + "}}").encode(), params=[("seed", "1")]),
        Case("unicode keys sort", body=body({"model": "m", "state": {"é": 1, "z": 2, "\U0001f389": 3, "～": 4, "A": 5}, "questions": {"q": {"type": "noul"}}}), params=[("seed", "1")]),
        Case("control chars", body=body({"model": "m", "state": "a\x00\x1f\x7f \"\\", "questions": {"q\n": {"type": "choice", "criteria": {"\t": None}}}}), params=[("seed", "1")]),
    ]
    # Validation errors.
    q = lambda question: body({**SMALL, "questions": {"q": question}})  # noqa: E731
    cases += [
        Case("missing state", body=body({"model": "m", "questions": {"q": {"type": "noul"}}})),
        Case("missing everything", body=b"{}"),
        Case("state null", body=body({**SMALL, "state": None})),
        Case("state number", body=body({**SMALL, "state": 5})),
        Case("model number", body=body({**SMALL, "model": 5})),
        Case("questions empty", body=body({**SMALL, "questions": {}})),
        Case("questions list", body=body({**SMALL, "questions": []})),
        Case("33 questions", body=body({**SMALL, "questions": {f"q{i}": {"type": "noul"} for i in range(33)}})),
        Case("33 questions one bad", body=body({**SMALL, "questions": {**{f"q{i}": {"type": "noul"} for i in range(32)}, "z": {"type": "x"}}})),
        Case("long question name", body=body({**SMALL, "questions": {"x" * 129: {"type": "x"}}})),
        Case("question number", body=body({**SMALL, "questions": {"q": 5}})),
        Case("question list", body=body({**SMALL, "questions": {"q": []}})),
        Case("no type", body=q({})),
        *[Case(f"type {t}", body=f'{{"model":"m","state":"s","questions":{{"q":{{"type":{t}}}}}}}'.encode())
          for t in ["null", "true", "false", "1", "1.0", "-0", "1E2", "{}", '{"a":[1,"b",null]}', "[1]", '""', '"Noul"', '"x\'y"']],
        Case("instructions number", body=q({"type": "noul", "instructions": 5})),
        Case("instructions bool", body=q({"type": "noul", "instructions": True})),
        Case("criteria number", body=q({"type": "noul", "criteria": 5})),
        Case("criteria list", body=q({"type": "noul", "criteria": []})),
        Case("criteria true int", body=q({"type": "noul", "criteria": {"true": 1, "false": 2.5}})),
        Case("score empty", body=q({"type": "score", "criteria": []})),
        Case("score missing", body=q({"type": "score"})),
        Case("score dict", body=q({"type": "score", "criteria": {"a": 1}})),
        Case("score null items", body=q({"type": "score", "criteria": [None, 3, "ok"]})),
        Case("score 33", body=q({"type": "score", "criteria": ["a"] * 33})),
        Case("score 33 bad", body=q({"type": "score", "criteria": [None] * 33})),
        Case("choice empty", body=q({"type": "choice", "criteria": {}})),
        Case("choice list", body=q({"type": "choice", "criteria": ["a"]})),
        Case("choice missing", body=q({"type": "choice", "instructions": 1})),
        Case("choice 65", body=q({"type": "choice", "criteria": {str(i): None for i in range(65)}})),
        Case("choice 65 bad", body=q({"type": "choice", "criteria": {str(i): 5 for i in range(65)}})),
        Case("choice long name", body=q({"type": "choice", "criteria": {"x" * 257: 5}})),
        Case("choice instructions + criteria bad", body=q({"type": "choice", "instructions": 1, "criteria": {"a": 1}})),
    ]
    # JSON decode errors.
    for raw in [b"[1,]", b'{"a":1,}', b'{"a":"\\x"}', b'{"a":"\\u12"}', b'{"a":"\\u12g4"}', b'{"a":"\\ud800\\u12g4"}',
                b'"\\u1234', b'{"a":"b\nc"}', b'"abc', b'{"a":1} x', b"-", b"01", b"[1.]", b"[1e]", b"[1e+]", b"[-]",
                b"tru", b"nul", b"Infinit", b'{"a"', b"  ", b'{"a":1 "b":2}', b"[1 2]", b"{1:2}", b"[,1]", b'{"\xc3\xa9":"x",}',
                b'["\xf0\x9f\x8e\x89", x]', b"\t\n\r [", b'{"a":', b'"\\']:
        cases.append(Case(f"json {raw[:30]!r}", body=raw))
    # Other routes.
    cases += [
        Case("root", method="GET", path="/", key=None),
        Case("health", method="GET", path="/health", key=None),
        Case("scenarios", method="GET", path="/sandbox/scenarios", key=None),
        Case("models", method="GET", path="/v1/models"),
        Case("models no key", method="GET", path="/v1/models", key=None),
        Case("openapi", method="GET", path="/openapi.json", key=None),
        Case("docs", method="GET", path="/docs", key=None),
        Case("redoc", method="GET", path="/redoc", key=None),
        Case("oauth2 redirect", method="GET", path="/docs/oauth2-redirect", key=None),
        Case("stats no token", method="GET", path="/sandbox/stats", key=None),
        Case("stats wrong token", method="GET", path="/sandbox/stats", key="wrong"),
        Case("404", method="GET", path="/nope"),
        Case("405 GET systemone", method="GET", path="/v1/systemone"),
        Case("405 POST health", method="POST", path="/health"),
        Case("405 HEAD", method="HEAD", path="/health"),
        Case("405 OPTIONS", method="OPTIONS", path="/health"),
        Case("redirect slash", method="GET", path="/health/", params=[("a", "1")]),
        Case("redirect slashes", method="GET", path="/v1/models///"),
        Case("redirect add slash", method="GET", path="/docs/oauth2-redirect/"),
        Case("percent path", method="GET", path="/heal%74h", key=None),
        Case("preflight", method="OPTIONS", headers={"Origin": "https://x.dev", "Access-Control-Request-Method": "POST", "Access-Control-Request-Headers": "authorization, content-type"}),
        Case("preflight bad method", method="OPTIONS", headers={"Origin": "https://x.dev", "Access-Control-Request-Method": "PUT"}),
        Case("preflight private network", method="OPTIONS", headers={"Origin": "https://x.dev", "Access-Control-Request-Method": "GET", "Access-Control-Request-Private-Network": "true"}),
        Case("cors simple", method="GET", path="/health", headers={"Origin": "https://x.dev"}),
        Case("cors on error", body=b"{", headers={"Origin": "null"}),
    ]
    return cases


@pytest.mark.parametrize("case", fixed_cases(), ids=lambda c: c.name)
def test_fixed_case(case):
    check(case)


# --- Fuzzing ---------------------------------------------------------------------------


class JsonFuzzer:
    """Random JSON text with the details that matter: number spellings, escapes, unicode,
    integer-like and duplicate keys, whitespace."""

    ALPHABET = string.ascii_letters + string.digits + " _-:'\"\\/\t\n" + "éß漢🎉  \x00\x1f\x7f"

    def __init__(self, rng: random.Random):
        self.r = rng

    def ws(self) -> str:
        return self.r.choice(["", "", "", " ", "\n  ", "\t"])

    def text(self, max_len: int = 12) -> str:
        return "".join(self.r.choice(self.ALPHABET) for _ in range(self.r.randint(0, max_len)))

    def string(self, max_len: int = 12) -> str:
        s = json.dumps(self.text(max_len), ensure_ascii=self.r.random() < 0.3)
        if self.r.random() < 0.1:
            s = s[:-1] + self.r.choice(["\\/", "\\b", "\\u00e9", "\\ud83c\\udf89", "\\u0000"]) + '"'
        return s

    def number(self) -> str:
        return self.r.choice([
            str(self.r.randint(-1000, 1000)), "0", "-0", "1.0", "-0.0", "1E2", "1e-7", "2.5e+3", "0.1", "1e16",
            "1e400", "123456789012345678901234567890", repr(self.r.uniform(-1e6, 1e6)), repr(self.r.random() * 10 ** self.r.randint(-30, 30)),
        ])

    def scalar(self) -> str:
        return self.r.choice([self.string, self.number, lambda: "null", lambda: "true", lambda: "false", lambda: "NaN"])()

    def value(self, depth: int = 0) -> str:
        kind = self.r.random()
        if depth > 3 or kind < 0.55:
            return self.scalar()
        if kind < 0.8:
            return self.array(depth + 1)
        return self.obj(depth + 1)

    def array(self, depth: int = 0, items=None) -> str:
        items = items if items is not None else [self.value(depth) for _ in range(self.r.randint(0, 4))]
        return "[" + self.ws() + ("," + self.ws()).join(items) + self.ws() + "]"

    def key(self) -> str:
        return self.r.choice([self.string(), self.string(), f'"{self.r.randint(0, 20)}"'])

    def obj(self, depth: int = 0, pairs=None) -> str:
        if pairs is None:
            pairs = [(self.key(), self.value(depth)) for _ in range(self.r.randint(0, 4))]
            if pairs and self.r.random() < 0.15:
                pairs.append((pairs[0][0], self.value(depth)))  # duplicate key
        return "{" + self.ws() + ("," + self.ws()).join(f"{k}{self.ws()}:{self.ws()}{v}" for k, v in pairs) + self.ws() + "}"

    def free_form(self, nullable: bool = True) -> str:
        options = [self.string, lambda: self.obj(1), lambda: self.array(1)]
        if nullable:
            options.append(lambda: "null")
        return self.r.choice(options)()

    def question(self) -> str:
        kind = self.r.choice(["noul", "choice", "score"])
        pairs = [('"type"', f'"{kind}"')]
        if self.r.random() < 0.7:
            pairs.append(('"instructions"', self.free_form()))
        if kind == "noul" and self.r.random() < 0.5:
            criteria = [(f'"{k}"', self.free_form()) for k in ("true", "false") if self.r.random() < 0.8]
            pairs.append(('"criteria"', self.r.choice([self.obj(1, criteria), "null"])))
        elif kind == "choice":
            names = [self.key() for _ in range(self.r.randint(1, 6))]
            pairs.append(('"criteria"', self.obj(1, [(n, self.free_form()) for n in names])))
        elif kind == "score":
            pairs.append(('"criteria"', self.array(1, [self.free_form(nullable=False) for _ in range(self.r.randint(1, 6))])))
        self.r.shuffle(pairs)
        return self.obj(1, pairs)

    def request(self) -> str:
        questions = [(self.key(), self.question()) for _ in range(self.r.randint(1, 6))]
        pairs = [
            ('"model"', self.string()),
            ('"state"', self.free_form(nullable=False)),
            ('"questions"', self.obj(1, questions)),
        ]
        self.r.shuffle(pairs)
        return self.obj(0, pairs)

    def mutate(self, doc: str) -> str:
        op = self.r.choice(["delete", "insert", "truncate", "replace_value"])
        i = self.r.randrange(len(doc))
        if op == "delete":
            return doc[:i] + doc[i + 1:]
        if op == "insert":
            return doc[:i] + self.r.choice(list(',:{}[]"\\ 0-eE.') + ["null", "NaN"]) + doc[i:]
        if op == "truncate":
            return doc[:i]
        # Swap a JSON value for one of another type, so validation (not decoding) fails.
        return re.sub(r'"type"\s*:\s*"\w+"', lambda _: f'"type":{self.scalar()}', doc, count=1) if self.r.random() < 0.3 \
            else doc.replace(self.r.choice(['"state"', '"model"', '"questions"', '"criteria"', '"instructions"']), self.r.choice(['"state"', '"x"']), 1)


def fuzz_cases() -> list[Case]:
    rng = random.Random(FUZZ_SEED)
    fuzz = JsonFuzzer(rng)
    scenarios = ANSWER_SCENARIOS + ["choice:" + fuzz.text(4), "score:" + str(rng.randint(-5, 40))]
    cases = []
    for i in range(FUZZ_CASES):
        doc = fuzz.request()
        seed = fuzz.text(20)
        if rng.random() < 0.9:  # a non-ASCII seed crashes the Python Worker; keep most cases comparable
            seed = "".join(c for c in seed if " " <= c < "\x7f")
        params = [("seed", seed or "s"), ("scenario", rng.choice(scenarios))]
        if i % 2:
            doc = fuzz.mutate(doc)
        cases.append(Case(f"fuzz-{FUZZ_SEED}-{i}", body=doc.encode("utf-8", "surrogatepass"), params=params, status_only=True))
    return cases


def test_fuzz():
    """Random valid and mutated requests: every Worker must answer byte-for-byte alike."""
    cases = fuzz_cases()
    with ThreadPoolExecutor(8) as pool:
        all_results = list(pool.map(run, cases))
    statuses: dict[int, int] = {}
    failures = []
    for case, results in zip(cases, all_results):
        # Fuzz bodies may hold seeds that crash the Python Worker (non-ASCII), so a 500 is
        # compared by status only; everything else must match exactly.
        try:
            assert_same(case, results)
        except AssertionError as exc:
            failures.append(str(exc))
        statuses[results[0].status] = statuses.get(results[0].status, 0) + 1
    print(f"fuzz statuses: {dict(sorted(statuses.items()))}")
    assert not failures, f"{len(failures)} of {len(cases)} cases differ; first:\n" + "\n\n".join(failures[:3])
    assert statuses.get(200, 0) > FUZZ_CASES // 5, "too few successful cases to be meaningful"


# --- Behaviour that is not a single request ---------------------------------------------


def test_unseeded_seed_replays_on_every_worker():
    for base in URLS:
        with httpx.Client(timeout=30) as client:
            case = Case("unseeded", body=body(BASE))
            first = send(client, base, case, fresh_ip(), KEYS[0])
            assert re.fullmatch(r"[0-9a-f]{16}", first.headers["x-sandbox-seed"])
            replay = send(client, base, Case("replay", body=body(BASE), headers={"X-Sandbox-Seed": first.headers["x-sandbox-seed"]}), fresh_ip(), KEYS[0])
            assert replay.body == first.body


def test_rate_limit_per_ip_matches():
    for base in URLS:
        ip = fresh_ip()
        with httpx.Client(timeout=30) as client:
            statuses = [send(client, base, Case("models", method="GET", path="/v1/models"), ip, KEYS[2]).status for _ in range(61)]
        assert statuses[:60] == [200] * 60, base
        assert statuses[60] == 429, base


def test_stats_shape_matches():
    shapes = []
    for base in URLS:
        with httpx.Client(timeout=30) as client:
            res = send(client, base, Case("stats", method="GET", path="/sandbox/stats"), fresh_ip(), ADMIN_TOKEN)
        assert res.status == 200, base
        data = json.loads(res.body)
        shapes.append((sorted(data), data["note"], {k: sorted(v) for k, v in data["keys"].items()}.get(KEYS[0])))
        assert data["total"] >= data["keys"].get(KEYS[0], {}).get("requests", 0)
    assert all(s == shapes[0] for s in shapes), shapes


def test_delay_and_timeout_scenarios_wait():
    def timed(base: str, case: Case) -> tuple[int, float]:
        with httpx.Client(timeout=30) as client:
            start = time.perf_counter()
            res = send(client, base, case, fresh_ip(), KEYS[3])
            return res.status, time.perf_counter() - start

    with ThreadPoolExecutor(len(URLS) * 2) as pool:
        delays = list(pool.map(lambda b: timed(b, Case("delay", body=body(SMALL), params=[("delay_ms", "1500")])), URLS))
        timeouts = list(pool.map(lambda b: timed(b, Case("timeout", body=body(SMALL), params=[("scenario", "timeout")])), URLS))
    for base, (status, seconds) in zip(URLS, delays):
        assert status == 200 and 1.4 < seconds < 5, (base, status, seconds)
    for base, (status, seconds) in zip(URLS, timeouts):
        assert status == 504 and 9.5 < seconds < 15, (base, status, seconds)
