/**
 * Synthetic answer generation, ported from src/sandbox/generator.py.
 *
 * Each question gets its own RNG derived from (seed, state, question name, question
 * body), so answers are reproducible for a given seed, identical to the Python
 * Worker's, and adding or removing one question never changes the others.
 */

import { SANDBOX_MODEL } from "./config";
import { EncodeError, type Json, type JsonObject, dumps, pyLen } from "./json";
import { PyRandom, pyRound, pySum } from "./pyrandom";
import type { Scenario, Spread } from "./scenarios";
import type { Question, SystemOneRequest } from "./schemas";

const PRECISION = 4;
const encoder = new TextEncoder();

export function newSeed(): string {
  return randomHex(8);
}

export function randomHex(bytes: number): string {
  return [...crypto.getRandomValues(new Uint8Array(bytes))].map((b) => b.toString(16).padStart(2, "0")).join("");
}

const canonicalJson = (value: Json) => dumps(value, { sortKeys: true, floats: "canonical" });

/** `model_dump(mode="json")`: defaults filled in; NaN/Infinity inside questions become null. */
function dumpQuestion(question: Question): JsonObject {
  const criteria: Json =
    question.type === "noul"
      ? question.criteria && new Map([["true", jsonMode(question.criteria.true)], ["false", jsonMode(question.criteria.false)]])
      : question.type === "choice"
        ? new Map([...question.criteria].map(([k, v]) => [k, jsonMode(v)]))
        : question.criteria.map(jsonMode);
  return new Map<string, Json>([
    ["type", question.type],
    ["instructions", jsonMode(question.instructions)],
    ["criteria", criteria],
  ]);
}

function jsonMode(value: Json): Json {
  if (typeof value === "number") return Number.isFinite(value) ? value : null;
  if (Array.isArray(value)) return value.map(jsonMode);
  if (value instanceof Map) return new Map([...value].map(([k, v]) => [k, jsonMode(v)]));
  return value;
}

async function questionRng(seed: string, stateJson: string, name: string, question: JsonObject): Promise<PyRandom> {
  // Python's material.encode() fails on a lone surrogate; TextEncoder would replace it.
  if (!name.isWellFormed()) throw new EncodeError("surrogates not allowed");
  const material = [seed, stateJson, name, canonicalJson(question)].join("\x1f");
  const digest = await crypto.subtle.digest("SHA-256", encoder.encode(material));
  return new PyRandom(new Uint8Array(digest));
}

export async function generate(request: SystemOneRequest, seed: string, scenario: Scenario): Promise<JsonObject> {
  const stateJson = canonicalJson(request.state);
  const dumped = new Map([...request.questions].map(([name, q]) => [name, dumpQuestion(q)]));
  const answers: JsonObject = new Map();
  for (const [name, question] of request.questions) {
    const rng = await questionRng(seed, stateJson, name, dumped.get(name) as JsonObject);
    if (question.type === "noul") answers.set(name, noul(rng, scenario));
    else if (question.type === "choice") answers.set(name, choice(rng, question.criteria, scenario));
    else answers.set(name, score(rng, question.criteria, scenario));
  }
  return new Map<string, Json>([
    ["model", SANDBOX_MODEL],
    ["answers", answers],
    ["usage", usage(request, stateJson, dumped)],
  ]);
}

// --- Answer types ------------------------------------------------------------

function noul(rng: PyRandom, scenario: Scenario): JsonObject {
  let value: number;
  if (scenario.noulRange) value = rng.uniform(...scenario.noulRange);
  else if (scenario.spread === "peaked") {
    value = rng.uniform(0.93, 0.995);
    value = rng.random() < 0.5 ? value : 1 - value;
  } else if (scenario.spread === "flat") value = rng.uniform(0.4, 0.6);
  // U-shaped: most answers are fairly decisive, some are uncertain.
  else value = rng.betavariate(0.55, 0.55);
  return new Map<string, Json>([["type", "noul"], ["noul", pyRound(value, PRECISION)]]);
}

function choice(rng: PyRandom, criteria: Map<string, Json>, scenario: Scenario): JsonObject {
  const names = [...criteria.keys()];
  let winner: number | null = null;
  if (scenario.choicePick === "first") winner = 0;
  else if (scenario.choicePick === "last") winner = names.length - 1;
  else if (scenario.choicePick !== null && criteria.has(scenario.choicePick)) winner = names.indexOf(scenario.choicePick);

  const probs = categorical(rng, names.length, scenario.spread, winner);
  const best = argmax(probs);
  return new Map<string, Json>([
    ["type", "choice"],
    ["choice", names[best] as string],
    ["confidence", probs[best] as number],
    ["probabilities", new Map(names.map((n, i) => [n, probs[i] as number]))],
  ]);
}

function score(rng: PyRandom, criteria: Json[], scenario: Scenario): JsonObject {
  const levels = criteria.length;
  const pick = scenario.scorePick;
  let winner: number | null;
  if (pick === "min") winner = 0;
  else if (pick === "max") winner = levels - 1;
  else if (typeof pick === "bigint") winner = Number(pick < 0n ? 0n : pick > BigInt(levels - 1) ? BigInt(levels - 1) : pick);
  else winner = null;

  const probs =
    scenario.spread === "flat" && winner === null
      ? categorical(rng, levels, "flat", null)
      : unimodal(rng, levels, scenario.spread, winner);

  const best = argmax(probs);
  const keys = criteria.map((_, i) => String(i));
  return new Map<string, Json>([
    ["type", "score"],
    ["score", pyRound(pySum(probs.map((p, i) => i * p)), PRECISION)],
    ["confidence", probs[best] as number],
    ["legend", new Map(keys.map((k, i) => [k, criteria[i] as Json]))],
    ["probabilities", new Map(keys.map((k, i) => [k, probs[i] as number]))],
  ]);
}

/** `max(range(n), key=probs.__getitem__)`: the first index of the maximum. */
function argmax(probs: number[]): number {
  let best = 0;
  for (let i = 1; i < probs.length; i++) if ((probs[i] as number) > (probs[best] as number)) best = i;
  return best;
}

// --- Distributions -----------------------------------------------------------

function categorical(rng: PyRandom, n: number, spread: Spread, winner: number | null): number[] {
  if (n === 1) return [1.0];
  if (winner === null && spread === "peaked") winner = rng.randrange(n);
  if (winner !== null) {
    const top = rng.uniform(0.82, 0.97);
    const probs = dirichlet(rng, n - 1, 1.0).map((p) => p * (1 - top));
    probs.splice(winner, 0, top);
    return roundAll(probs);
  }
  return roundAll(dirichlet(rng, n, spread === "flat" ? 25.0 : 0.8));
}

/** Probabilities that peak at one level and fall off with distance, like a real rubric. */
function unimodal(rng: PyRandom, n: number, spread: Spread, winner: number | null): number[] {
  if (n === 1) return [1.0];
  const centre = winner !== null ? winner : rng.randrange(n);
  const sigma = spread === "peaked" || winner !== null ? rng.uniform(0.25, 0.45) : rng.uniform(0.4, 1.4);
  const weights: number[] = [];
  for (let i = 0; i < n; i++) {
    weights.push(Math.exp(-((i - centre) ** 2) / (2 * sigma * sigma)) * rng.uniform(0.85, 1.15));
  }
  const total = pySum(weights);
  return roundAll(weights.map((w) => w / total));
}

function dirichlet(rng: PyRandom, n: number, alpha: number): number[] {
  const draws: number[] = [];
  for (let i = 0; i < n; i++) draws.push(rng.gammavariate(alpha, 1.0));
  const total = pySum(draws) || 1.0;
  return draws.map((d) => d / total);
}

const roundAll = (probs: number[]) => probs.map((p) => pyRound(p, PRECISION));

/** Plausible, deterministic token counts (roughly four characters per token). */
function usage(request: SystemOneRequest, stateJson: string, dumped: Map<string, JsonObject>): JsonObject {
  const text = stateJson + canonicalJson(dumped);
  return new Map<string, Json>([
    ["input_tokens", BigInt(Math.max(1, Math.ceil(pyLen(text) / 4)))],
    ["output_tokens", BigInt(2 * request.questions.size)],
  ]);
}
