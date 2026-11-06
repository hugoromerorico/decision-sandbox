/**
 * Request validation matching src/sandbox/schemas.py, error for error.
 *
 * FastAPI reports pydantic's errors verbatim, so the type, loc, msg, input and ctx of
 * each error, and their order, follow pydantic-core's rules for these models.
 */

import {
  MAX_CHOICE_NAME_LENGTH,
  MAX_CHOICES,
  MAX_QUESTION_NAME_LENGTH,
  MAX_QUESTIONS,
  MAX_SCORE_LEVELS,
} from "./config";
import { type Json, type JsonObject, pyLen, pyStr, rustTrim } from "./json";

export type Loc = (string | number)[];

export interface ValidationError {
  type: string;
  loc: Loc;
  msg: string;
  input: Json;
  ctx?: JsonObject;
}

export type Instructions = Json;

export type Question =
  | { type: "noul"; instructions: Instructions; criteria: { true: Instructions; false: Instructions } | null }
  | { type: "choice"; instructions: Instructions; criteria: Map<string, Instructions> }
  | { type: "score"; instructions: Instructions; criteria: Json[] };

export interface SystemOneRequest {
  state: Json;
  model: string;
  questions: Map<string, Question>;
}

const isObject = (v: Json | undefined): v is JsonObject => v instanceof Map;
const ctx = (entries: [string, Json][]): JsonObject => new Map(entries);

class Validator {
  readonly errors: ValidationError[] = [];

  private add(type: string, loc: Loc, msg: string, input: Json, context?: JsonObject): void {
    this.errors.push(context ? { type, loc, msg, input, ctx: context } : { type, loc, msg, input });
  }

  missing(loc: Loc, input: Json): void {
    this.add("missing", loc, "Field required", input);
  }

  modelAttributes(loc: Loc, input: Json): void {
    this.add("model_attributes_type", loc, "Input should be a valid dictionary or object to extract fields from", input);
  }

  /** `str | dict[str, Any] | list[Any]`, optionally nullable. */
  freeForm(value: Json, loc: Loc, nullable: boolean): boolean {
    if (typeof value === "string" || isObject(value) || Array.isArray(value)) return true;
    if (value === null && nullable) return true;
    this.add("string_type", [...loc, "str"], "Input should be a valid string", value);
    this.add("dict_type", [...loc, "dict[str,any]"], "Input should be a valid dictionary", value);
    this.add("list_type", [...loc, "list[any]"], "Input should be a valid list", value);
    return false;
  }

  string(value: Json, loc: Loc): value is string {
    if (typeof value === "string") return true;
    this.add("string_type", loc, "Input should be a valid string", value);
    return false;
  }

  keyLength(key: string, loc: Loc, max: number): void {
    if (pyLen(key) > max) {
      this.add("string_too_long", [...loc, key, "[key]"], `String should have at most ${max} characters`, key, ctx([["max_length", BigInt(max)]]));
    }
  }

  /** Dict length is checked only when every item validated. */
  dictLength(value: JsonObject, loc: Loc, max: number): void {
    const n = value.size;
    if (n < 1) {
      this.add("too_short", loc, `Dictionary should have at least 1 item after validation, not ${n}`, value,
        ctx([["field_type", "Dictionary"], ["min_length", 1n], ["actual_length", BigInt(n)]]));
    } else if (n > max) {
      this.add("too_long", loc, `Dictionary should have at most ${max} items after validation, not ${n}`, value,
        ctx([["field_type", "Dictionary"], ["max_length", BigInt(max)], ["actual_length", BigInt(n)]]));
    }
  }

  dictType(value: Json, loc: Loc): value is JsonObject {
    if (isObject(value)) return true;
    this.add("dict_type", loc, "Input should be a valid dictionary", value);
    return false;
  }

  /** A list longer than its maximum reports only that, before validating items. */
  list(value: Json, loc: Loc, max: number): value is Json[] {
    if (!Array.isArray(value)) {
      this.add("list_type", loc, "Input should be a valid list", value);
      return false;
    }
    if (value.length > max) {
      this.add("too_long", loc, `List should have at most ${max} items after validation, not ${value.length}`, value,
        ctx([["field_type", "List"], ["max_length", BigInt(max)], ["actual_length", BigInt(value.length)]]));
      return false;
    }
    return true;
  }

  listTooShort(value: Json[], loc: Loc): void {
    this.add("too_short", loc, `List should have at least 1 item after validation, not ${value.length}`, value,
      ctx([["field_type", "List"], ["min_length", 1n], ["actual_length", BigInt(value.length)]]));
  }

  question(value: Json, loc: Loc): Question | null {
    if (!isObject(value)) {
      this.modelAttributes(loc, value);
      return null;
    }
    if (!value.has("type")) {
      this.add("union_tag_not_found", loc, "Unable to extract tag using discriminator 'type'", value,
        ctx([["discriminator", "'type'"]]));
      return null;
    }
    const tag = value.get("type") as Json;
    if (tag === "noul") return this.noul(value, [...loc, "noul"]);
    if (tag === "choice") return this.choice(value, [...loc, "choice"]);
    if (tag === "score") return this.score(value, [...loc, "score"]);
    const shown = pyStr(tag);
    const expected = "'noul', 'choice', 'score'";
    this.add("union_tag_invalid", loc,
      `Input tag '${shown}' found using 'type' does not match any of the expected tags: ${expected}`, value,
      ctx([["discriminator", "'type'"], ["tag", shown], ["expected_tags", expected]]));
    return null;
  }

  private instructions(value: JsonObject, loc: Loc): [Instructions, boolean] {
    const instructions = value.get("instructions") ?? null;
    return [instructions, this.freeForm(instructions, [...loc, "instructions"], true)];
  }

  private noul(value: JsonObject, loc: Loc): Question | null {
    const [instructions, ok] = this.instructions(value, loc);
    let criteria: { true: Instructions; false: Instructions } | null = null;
    let criteriaOk = true;
    const raw = value.get("criteria") ?? null;
    if (raw !== null) {
      if (!isObject(raw)) {
        this.modelAttributes([...loc, "criteria"], raw);
        criteriaOk = false;
      } else {
        const t = raw.get("true") ?? null;
        const f = raw.get("false") ?? null;
        const tOk = this.freeForm(t, [...loc, "criteria", "true"], true);
        const fOk = this.freeForm(f, [...loc, "criteria", "false"], true);
        criteriaOk = tOk && fOk;
        criteria = { true: t, false: f };
      }
    }
    return ok && criteriaOk ? { type: "noul", instructions, criteria } : null;
  }

  private choice(value: JsonObject, loc: Loc): Question | null {
    const [instructions, ok] = this.instructions(value, loc);
    const criteriaLoc = [...loc, "criteria"];
    if (!value.has("criteria")) {
      this.missing(criteriaLoc, value);
      return null;
    }
    const raw = value.get("criteria") as Json;
    if (!this.dictType(raw, criteriaLoc)) return null;
    const before = this.errors.length;
    for (const [name, description] of raw) {
      this.keyLength(name, criteriaLoc, MAX_CHOICE_NAME_LENGTH);
      this.freeForm(description, [...criteriaLoc, name], true);
    }
    if (this.errors.length > before) return null;
    this.dictLength(raw, criteriaLoc, MAX_CHOICES);
    return ok && this.errors.length === before ? { type: "choice", instructions, criteria: raw } : null;
  }

  private score(value: JsonObject, loc: Loc): Question | null {
    const [instructions, ok] = this.instructions(value, loc);
    const criteriaLoc = [...loc, "criteria"];
    if (!value.has("criteria")) {
      this.missing(criteriaLoc, value);
      return null;
    }
    const raw = value.get("criteria") as Json;
    if (!this.list(raw, criteriaLoc, MAX_SCORE_LEVELS)) return null;
    const before = this.errors.length;
    raw.forEach((level, i) => this.freeForm(level, [...criteriaLoc, i], false));
    if (this.errors.length > before) return null;
    if (raw.length < 1) {
      this.listTooShort(raw, criteriaLoc);
      return null;
    }
    return ok ? { type: "score", instructions, criteria: raw } : null;
  }
}

/** The body as FastAPI hands it to pydantic: parsed JSON, raw text, or nothing. */
export type Body = { kind: "json"; value: Json } | { kind: "text"; value: string } | { kind: "none" };

export function validateRequest(body: Body, v = new Validator()): { request: SystemOneRequest | null; errors: ValidationError[] } {
  const fail = () => ({ request: null, errors: v.errors });
  if (body.kind === "none" || (body.kind === "json" && body.value === null)) {
    v.missing(["body"], null);
    return fail();
  }
  if (body.kind === "text" || !isObject(body.value)) {
    v.modelAttributes(["body"], body.value);
    return fail();
  }
  const data = body.value;

  let state: Json = null;
  if (!data.has("state")) v.missing(["body", "state"], data);
  else {
    state = data.get("state") as Json;
    v.freeForm(state, ["body", "state"], false);
  }

  let model = "";
  if (!data.has("model")) v.missing(["body", "model"], data);
  else {
    const raw = data.get("model") as Json;
    if (v.string(raw, ["body", "model"])) model = raw;
  }

  const questions = new Map<string, Question>();
  if (!data.has("questions")) v.missing(["body", "questions"], data);
  else {
    const raw = data.get("questions") as Json;
    const loc = ["body", "questions"];
    if (v.dictType(raw, loc)) {
      const before = v.errors.length;
      for (const [name, value] of raw) {
        v.keyLength(name, loc, MAX_QUESTION_NAME_LENGTH);
        const question = v.question(value, [...loc, name]);
        if (question) questions.set(name, question);
      }
      if (v.errors.length === before) v.dictLength(raw, loc, MAX_QUESTIONS);
    }
  }

  if (v.errors.length) return fail();
  return { request: { state, model, questions }, errors: [] };
}

/** Header/query `int` with ge=0, le=MAX: pydantic's lax str -> int parsing. */
export function validateIntParam(raw: string, loc: Loc, max: number, errors: ValidationError[]): number | null {
  const m = /^([+-]?)(\d+(?:_\d+)*)(?:\.0+)?$/.exec(rustTrim(raw));
  if (!m) {
    errors.push({ type: "int_parsing", loc, msg: "Input should be a valid integer, unable to parse string as an integer", input: raw });
    return null;
  }
  const magnitude = BigInt((m[2] as string).replaceAll("_", ""));
  const value = m[1] === "-" ? -magnitude : magnitude;
  if (value < 0n) {
    errors.push({ type: "greater_than_equal", loc, msg: "Input should be greater than or equal to 0", input: raw, ctx: ctx([["ge", 0n]]) });
    return null;
  }
  if (value > BigInt(max)) {
    errors.push({ type: "less_than_equal", loc, msg: `Input should be less than or equal to ${max}`, input: raw, ctx: ctx([["le", BigInt(max)]]) });
    return null;
  }
  return Number(value);
}

/** Errors as FastAPI serialises them. */
export function errorsToJson(errors: ValidationError[]): Json[] {
  return errors.map((e) => {
    const out: JsonObject = new Map<string, Json>([
      ["type", e.type],
      ["loc", e.loc.map((part) => (typeof part === "number" ? BigInt(part) : part))],
      ["msg", e.msg],
      ["input", e.input],
    ]);
    if (e.ctx) out.set("ctx", e.ctx);
    return out;
  });
}
