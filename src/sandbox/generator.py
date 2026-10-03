"""Synthetic answer generation.

Each question gets its own RNG derived from (seed, state, question name, question
body), so answers are reproducible for a given seed and adding or removing one
question never changes the answers to the others.
"""

import hashlib
import json
import math
import random
import secrets
from typing import Any

from . import SANDBOX_MODEL
from .scenarios import Scenario, Spread
from .schemas import ChoiceQuestion, NoulQuestion, ScoreQuestion, SystemOneRequest

PRECISION = 4


def new_seed() -> str:
    return secrets.token_hex(8)


def canonical_json(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def question_rng(seed: str, state: Any, name: str, question: dict[str, Any]) -> random.Random:
    material = "\x1f".join((seed, canonical_json(state), name, canonical_json(question)))
    digest = hashlib.sha256(material.encode()).digest()
    return random.Random(int.from_bytes(digest, "big"))


def generate(request: SystemOneRequest, seed: str, scenario: Scenario) -> dict[str, Any]:
    answers = {}
    for name, question in request.questions.items():
        rng = question_rng(seed, request.state, name, question.model_dump(mode="json"))
        match question:
            case NoulQuestion():
                answers[name] = _noul(rng, scenario)
            case ChoiceQuestion():
                answers[name] = _choice(rng, question, scenario)
            case ScoreQuestion():
                answers[name] = _score(rng, question, scenario)

    return {
        "model": SANDBOX_MODEL,
        "answers": answers,
        "usage": _usage(request),
    }


# --- Answer types ------------------------------------------------------------


def _noul(rng: random.Random, scenario: Scenario) -> dict[str, Any]:
    if scenario.noul_range:
        value = rng.uniform(*scenario.noul_range)
    elif scenario.spread == "peaked":
        value = rng.uniform(0.93, 0.995)
        value = value if rng.random() < 0.5 else 1 - value
    elif scenario.spread == "flat":
        value = rng.uniform(0.4, 0.6)
    else:
        # U-shaped: most answers are fairly decisive, some are uncertain.
        value = rng.betavariate(0.55, 0.55)
    return {"type": "noul", "noul": round(value, PRECISION)}


def _choice(rng: random.Random, question: ChoiceQuestion, scenario: Scenario) -> dict[str, Any]:
    names = list(question.criteria)
    winner = None
    if scenario.choice_pick == "first":
        winner = 0
    elif scenario.choice_pick == "last":
        winner = len(names) - 1
    elif scenario.choice_pick in question.criteria:
        winner = names.index(scenario.choice_pick)

    probs = _categorical(rng, len(names), scenario.spread, winner)
    best = max(range(len(names)), key=probs.__getitem__)
    return {
        "type": "choice",
        "choice": names[best],
        "confidence": probs[best],
        "probabilities": dict(zip(names, probs)),
    }


def _score(rng: random.Random, question: ScoreQuestion, scenario: Scenario) -> dict[str, Any]:
    levels = len(question.criteria)
    pick = scenario.score_pick
    if pick == "min":
        winner = 0
    elif pick == "max":
        winner = levels - 1
    elif isinstance(pick, int):
        winner = min(max(pick, 0), levels - 1)
    else:
        winner = None

    if scenario.spread == "flat" and winner is None:
        probs = _categorical(rng, levels, "flat", None)
    else:
        probs = _unimodal(rng, levels, scenario.spread, winner)

    best = max(range(levels), key=probs.__getitem__)
    keys = [str(i) for i in range(levels)]
    return {
        "type": "score",
        "score": round(sum(i * p for i, p in enumerate(probs)), PRECISION),
        "confidence": probs[best],
        "legend": dict(zip(keys, question.criteria)),
        "probabilities": dict(zip(keys, probs)),
    }


# --- Distributions -----------------------------------------------------------


def _categorical(rng: random.Random, n: int, spread: Spread, winner: int | None) -> list[float]:
    if n == 1:
        return [1.0]
    if winner is None and spread == "peaked":
        winner = rng.randrange(n)
    if winner is not None:
        top = rng.uniform(0.82, 0.97)
        rest = _dirichlet(rng, n - 1, 1.0)
        probs = [p * (1 - top) for p in rest]
        probs.insert(winner, top)
        return _round(probs)
    return _round(_dirichlet(rng, n, 25.0 if spread == "flat" else 0.8))


def _unimodal(rng: random.Random, n: int, spread: Spread, winner: int | None) -> list[float]:
    """Probabilities that peak at one level and fall off with distance, like a real rubric."""
    if n == 1:
        return [1.0]
    centre = winner if winner is not None else rng.randrange(n)
    sigma = rng.uniform(0.25, 0.45) if spread == "peaked" or winner is not None else rng.uniform(0.4, 1.4)
    weights = [math.exp(-((i - centre) ** 2) / (2 * sigma**2)) * rng.uniform(0.85, 1.15) for i in range(n)]
    total = sum(weights)
    return _round([w / total for w in weights])


def _dirichlet(rng: random.Random, n: int, alpha: float) -> list[float]:
    draws = [rng.gammavariate(alpha, 1.0) for _ in range(n)]
    total = sum(draws) or 1.0
    return [d / total for d in draws]


def _round(probs: list[float]) -> list[float]:
    return [round(p, PRECISION) for p in probs]


def _usage(request: SystemOneRequest) -> dict[str, int]:
    """Plausible, deterministic token counts (roughly four characters per token)."""
    text = canonical_json(request.state) + canonical_json(
        {name: q.model_dump(mode="json") for name, q in request.questions.items()}
    )
    return {
        "input_tokens": max(1, math.ceil(len(text) / 4)),
        "output_tokens": 2 * len(request.questions),
    }
