"""Named behaviours that let developers deliberately exercise application branches."""

from dataclasses import dataclass
from typing import Literal

Spread = Literal["natural", "peaked", "flat"]


@dataclass(frozen=True)
class Scenario:
    name: str
    description: str
    # Shapes successful answers.
    spread: Spread = "natural"
    noul_range: tuple[float, float] | None = None
    choice_pick: str | None = None  # "first", "last" or a choice name
    score_pick: str | int | None = None  # "min", "max" or a level index
    # Replaces the successful response entirely.
    error: str | None = None

    @property
    def is_error(self) -> bool:
        return self.error is not None


_SCENARIOS = [
    Scenario("random", "Varied synthetic answers (default)."),
    Scenario("high-confidence", "Decisive answers: noul near 0 or 1, one dominant choice or score level.", spread="peaked"),
    Scenario("low-confidence", "Uncertain answers: noul near 0.5, near-uniform probabilities.", spread="flat"),
    Scenario("always-true", "Every noul answer is close to 1.", noul_range=(0.95, 0.995)),
    Scenario("always-false", "Every noul answer is close to 0.", noul_range=(0.005, 0.05)),
    Scenario("first-choice", "Choice questions select their first option.", choice_pick="first"),
    Scenario("last-choice", "Choice questions select their last option.", choice_pick="last"),
    Scenario("min-score", "Score questions favour the lowest level.", score_pick="min"),
    Scenario("max-score", "Score questions favour the highest level.", score_pick="max"),
    Scenario("invalid-response", "HTTP 200 with a body that violates the response schema.", error="invalid-response"),
    Scenario("validation-error", "HTTP 422 with a validation error body.", error="validation-error"),
    Scenario("unauthorized", "HTTP 401, as if the API key were missing or invalid.", error="unauthorized"),
    Scenario("rate-limit", "HTTP 429 with a Retry-After header.", error="rate-limit"),
    Scenario("server-error", "HTTP 500.", error="server-error"),
    Scenario("unavailable", "HTTP 503 with a Retry-After header.", error="unavailable"),
    Scenario("timeout", "Waits 10 seconds, then returns HTTP 504.", error="timeout"),
]

SCENARIOS: dict[str, Scenario] = {s.name: s for s in _SCENARIOS}
DEFAULT_SCENARIO = SCENARIOS["random"]

PARAMETERISED = {
    "choice:<name>": "Choice questions that offer <name> select it; others stay random.",
    "score:<level>": "Score questions favour level <level> (clamped to the rubric).",
}


class UnknownScenario(ValueError):
    pass


def parse_scenario(value: str | None) -> Scenario:
    if not value:
        return DEFAULT_SCENARIO
    value = value.strip()
    if value in SCENARIOS:
        return SCENARIOS[value]

    kind, sep, arg = value.partition(":")
    if sep and arg:
        if kind == "choice":
            return Scenario(value, PARAMETERISED["choice:<name>"], choice_pick=arg)
        if kind == "score":
            try:
                level = int(arg)
            except ValueError:
                pass
            else:
                return Scenario(value, PARAMETERISED["score:<level>"], score_pick=level)

    raise UnknownScenario(value)


def catalogue() -> list[dict[str, str]]:
    return [{"name": s.name, "description": s.description} for s in _SCENARIOS] + [
        {"name": name, "description": description} for name, description in PARAMETERISED.items()
    ]
