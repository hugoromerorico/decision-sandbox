/** Named behaviours that let developers deliberately exercise application branches. */

import { pyStrip } from "./json";

export type Spread = "natural" | "peaked" | "flat";

export interface Scenario {
  name: string;
  description: string;
  spread: Spread;
  noulRange: [number, number] | null;
  /** "first", "last" or a choice name. */
  choicePick: string | null;
  /** "min", "max" or a level index. */
  scorePick: "min" | "max" | bigint | null;
  /** Replaces the successful response entirely. */
  error: string | null;
}

const scenario = (name: string, description: string, fields: Partial<Scenario> = {}): Scenario => ({
  name,
  description,
  spread: "natural",
  noulRange: null,
  choicePick: null,
  scorePick: null,
  error: null,
  ...fields,
});

const LIST: Scenario[] = [
  scenario("random", "Varied synthetic answers (default)."),
  scenario("high-confidence", "Decisive answers: noul near 0 or 1, one dominant choice or score level.", { spread: "peaked" }),
  scenario("low-confidence", "Uncertain answers: noul near 0.5, near-uniform probabilities.", { spread: "flat" }),
  scenario("always-true", "Every noul answer is close to 1.", { noulRange: [0.95, 0.995] }),
  scenario("always-false", "Every noul answer is close to 0.", { noulRange: [0.005, 0.05] }),
  scenario("first-choice", "Choice questions select their first option.", { choicePick: "first" }),
  scenario("last-choice", "Choice questions select their last option.", { choicePick: "last" }),
  scenario("min-score", "Score questions favour the lowest level.", { scorePick: "min" }),
  scenario("max-score", "Score questions favour the highest level.", { scorePick: "max" }),
  scenario("invalid-response", "HTTP 200 with a body that violates the response schema.", { error: "invalid-response" }),
  scenario("validation-error", "HTTP 422 with a validation error body.", { error: "validation-error" }),
  scenario("unauthorized", "HTTP 401, as if the API key were missing or invalid.", { error: "unauthorized" }),
  scenario("rate-limit", "HTTP 429 with a Retry-After header.", { error: "rate-limit" }),
  scenario("server-error", "HTTP 500.", { error: "server-error" }),
  scenario("unavailable", "HTTP 503 with a Retry-After header.", { error: "unavailable" }),
  scenario("timeout", "Waits 10 seconds, then returns HTTP 504.", { error: "timeout" }),
];

const SCENARIOS = new Map(LIST.map((s) => [s.name, s]));
const DEFAULT_SCENARIO = SCENARIOS.get("random") as Scenario;

const PARAMETERISED: [string, string][] = [
  ["choice:<name>", "Choice questions that offer <name> select it; others stay random."],
  ["score:<level>", "Score questions favour level <level> (clamped to the rubric)."],
];

export class UnknownScenario extends Error {
  constructor(readonly value: string) {
    super(value);
  }
}

/** Python's `int(str)`: whitespace, a sign, ASCII digits with single underscores. */
function pyInt(text: string): bigint | null {
  const m = /^([+-]?)(\d+(?:_\d+)*)$/.exec(pyStrip(text));
  if (!m) return null;
  const value = BigInt((m[2] as string).replaceAll("_", ""));
  return m[1] === "-" ? -value : value;
}

export function parseScenario(raw: string | null): Scenario {
  if (!raw) return DEFAULT_SCENARIO;
  const value = pyStrip(raw);
  const known = SCENARIOS.get(value);
  if (known) return known;

  const colon = value.indexOf(":");
  if (colon >= 0 && colon < value.length - 1) {
    const kind = value.slice(0, colon);
    const arg = value.slice(colon + 1);
    if (kind === "choice") {
      return scenario(value, PARAMETERISED[0]?.[1] as string, { choicePick: arg });
    }
    if (kind === "score") {
      const level = pyInt(arg);
      if (level !== null) return scenario(value, PARAMETERISED[1]?.[1] as string, { scorePick: level });
    }
  }
  throw new UnknownScenario(value);
}

export function catalogue(): { name: string; description: string }[] {
  return [
    ...LIST.map(({ name, description }) => ({ name, description })),
    ...PARAMETERISED.map(([name, description]) => ({ name, description })),
  ];
}
