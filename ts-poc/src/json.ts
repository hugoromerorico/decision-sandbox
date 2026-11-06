/**
 * JSON with Python semantics.
 *
 * The Python Worker parses bodies with CPython's `json` module and hashes them with
 * `json.dumps(sort_keys=True)`, so seeded answers depend on details JSON.parse loses:
 * key order (JS objects put integer-like keys first), ints vs floats (`1` vs `1.0`),
 * big ints, NaN/Infinity literals and Python's error messages. This module reproduces
 * them: objects are Maps in insertion order, ints are bigints, floats are numbers.
 */

export type Json = null | boolean | string | number | bigint | Json[] | JsonObject;
export type JsonObject = Map<string, Json>;

/** Python's `json.JSONDecodeError`: FastAPI turns it into a 422 `json_invalid` error. */
export class JsonDecodeError extends Error {
  constructor(
    readonly msg: string,
    /** Position in code points, as Python reports it. */
    readonly pos: number,
  ) {
    super(msg);
  }
}

/** Any other decoding failure (bad UTF-8, too deep, huge ints): FastAPI answers 400. */
export class BodyParseError extends Error {}

/** A string that cannot be encoded as UTF-8 (lone surrogate): Python fails with a 500. */
export class EncodeError extends Error {}

/** Deeper nesting raises RecursionError in Python (a 400). The exact depth differs by runtime. */
export const MAX_DEPTH = 512;
/** CPython's default int_max_str_digits. */
const MAX_INT_DIGITS = 4300;

const isWs = (c: number) => c === 0x20 || c === 0x09 || c === 0x0a || c === 0x0d;
const isDigit = (c: number) => c >= 0x30 && c <= 0x39;

/** Decodes request bytes the way Starlette's `request.json()` does. */
export function loadsBytes(bytes: Uint8Array): Json {
  let text: string;
  try {
    // json.loads(bytes) decodes with utf-8-sig, so one leading BOM is dropped.
    text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(bytes);
  } catch {
    throw new BodyParseError("invalid utf-8");
  }
  return loads(text);
}

/** Decodes a string like CPython's `json.loads` (C scanner), errors included. */
export function loads(s: string): Json {
  return new Scanner(s).decode();
}

class Scanner {
  private depth = 0;
  constructor(private readonly s: string) {}

  decode(): Json {
    const s = this.s;
    let idx = this.skipWs(0);
    const [value, end] = this.scanOnce(idx);
    idx = this.skipWs(end);
    if (idx !== s.length) throw this.error("Extra data", idx);
    return value;
  }

  private error(msg: string, idx: number): JsonDecodeError {
    let points = 0;
    for (let i = 0; i < idx; i++) {
      const c = this.s.charCodeAt(i);
      // A surrogate pair is one code point in Python.
      if (c >= 0xd800 && c <= 0xdbff && i + 1 < idx) {
        const d = this.s.charCodeAt(i + 1);
        if (d >= 0xdc00 && d <= 0xdfff) i++;
      }
      points++;
    }
    return new JsonDecodeError(msg, points);
  }

  private skipWs(idx: number): number {
    while (idx < this.s.length && isWs(this.s.charCodeAt(idx))) idx++;
    return idx;
  }

  private scanOnce(idx: number): [Json, number] {
    const s = this.s;
    if (idx >= s.length) throw this.error("Expecting value", idx);
    switch (s[idx]) {
      case '"':
        return this.scanString(idx + 1);
      case "{":
        return this.nested(() => this.parseObject(idx + 1));
      case "[":
        return this.nested(() => this.parseArray(idx + 1));
      case "n":
        if (s.startsWith("null", idx)) return [null, idx + 4];
        break;
      case "t":
        if (s.startsWith("true", idx)) return [true, idx + 4];
        break;
      case "f":
        if (s.startsWith("false", idx)) return [false, idx + 5];
        break;
      case "N":
        if (s.startsWith("NaN", idx)) return [NaN, idx + 3];
        break;
      case "I":
        if (s.startsWith("Infinity", idx)) return [Infinity, idx + 8];
        break;
      case "-":
        if (s.startsWith("-Infinity", idx)) return [-Infinity, idx + 9];
        break;
    }
    return this.matchNumber(idx);
  }

  private nested(parse: () => [Json, number]): [Json, number] {
    if (++this.depth > MAX_DEPTH) throw new BodyParseError("maximum nesting depth exceeded");
    try {
      return parse();
    } finally {
      this.depth--;
    }
  }

  private parseObject(idx: number): [JsonObject, number] {
    const s = this.s;
    const out: JsonObject = new Map();
    idx = this.skipWs(idx);
    if (idx >= s.length || s[idx] !== "}") {
      for (;;) {
        if (idx >= s.length || s[idx] !== '"') {
          throw this.error("Expecting property name enclosed in double quotes", idx);
        }
        const [key, afterKey] = this.scanString(idx + 1);
        idx = this.skipWs(afterKey);
        if (idx >= s.length || s[idx] !== ":") throw this.error("Expecting ':' delimiter", idx);
        idx = this.skipWs(idx + 1);
        const [value, afterValue] = this.scanOnce(idx);
        // Like a Python dict: a repeated key keeps its first position and takes the last value.
        out.set(key, value);
        idx = this.skipWs(afterValue);
        if (idx < s.length && s[idx] === "}") break;
        if (idx >= s.length || s[idx] !== ",") throw this.error("Expecting ',' delimiter", idx);
        const comma = idx;
        idx = this.skipWs(idx + 1);
        if (idx < s.length && s[idx] === "}") {
          throw this.error("Illegal trailing comma before end of object", comma);
        }
      }
    }
    return [out, idx + 1];
  }

  private parseArray(idx: number): [Json[], number] {
    const s = this.s;
    const out: Json[] = [];
    idx = this.skipWs(idx);
    if (idx >= s.length || s[idx] !== "]") {
      for (;;) {
        const [value, afterValue] = this.scanOnce(idx);
        out.push(value);
        idx = this.skipWs(afterValue);
        if (idx < s.length && s[idx] === "]") break;
        if (idx >= s.length || s[idx] !== ",") throw this.error("Expecting ',' delimiter", idx);
        const comma = idx;
        idx = this.skipWs(idx + 1);
        if (idx < s.length && s[idx] === "]") {
          throw this.error("Illegal trailing comma before end of array", comma);
        }
      }
    }
    return [out, idx + 1];
  }

  private matchNumber(start: number): [Json, number] {
    const s = this.s;
    const last = s.length - 1;
    let idx = start;
    if (s[idx] === "-") {
      idx++;
      if (idx > last) throw this.error("Expecting value", start);
    }
    const first = s.charCodeAt(idx);
    if (first >= 0x31 && first <= 0x39) {
      idx++;
      while (idx <= last && isDigit(s.charCodeAt(idx))) idx++;
    } else if (first === 0x30) {
      idx++;
    } else {
      throw this.error("Expecting value", start);
    }
    let isFloat = false;
    if (idx < last && s[idx] === "." && isDigit(s.charCodeAt(idx + 1))) {
      isFloat = true;
      idx += 2;
      while (idx <= last && isDigit(s.charCodeAt(idx))) idx++;
    }
    if (idx < last && (s[idx] === "e" || s[idx] === "E")) {
      const eStart = idx++;
      if (idx < last && (s[idx] === "-" || s[idx] === "+")) idx++;
      while (idx <= last && isDigit(s.charCodeAt(idx))) idx++;
      if (isDigit(s.charCodeAt(idx - 1))) isFloat = true;
      else idx = eStart;
    }
    const text = s.slice(start, idx);
    if (isFloat) return [Number(text), idx];
    if (text.length - (text[0] === "-" ? 1 : 0) > MAX_INT_DIGITS) {
      throw new BodyParseError("integer string too long");
    }
    return [BigInt(text), idx];
  }

  /** CPython's scanstring_unicode; `end` is the index after the opening quote. */
  private scanString(end: number): [string, number] {
    const s = this.s;
    const begin = end - 1;
    const len = s.length;
    let out = "";
    for (;;) {
      let next = end;
      let c = 0;
      for (; next < len; next++) {
        c = s.charCodeAt(next);
        if (c === 0x22 || c === 0x5c) break;
        if (c <= 0x1f) throw this.error("Invalid control character at", next);
      }
      if (next >= len) throw this.error("Unterminated string starting at", begin);
      out += s.slice(end, next);
      next++;
      if (c === 0x22) return [out, next];
      if (next === len) throw this.error("Unterminated string starting at", begin);
      const esc = s[next];
      if (esc !== "u") {
        end = next + 1;
        const simple: Record<string, string> = { '"': '"', "\\": "\\", "/": "/", b: "\b", f: "\f", n: "\n", r: "\r", t: "\t" };
        const ch = esc === undefined ? undefined : simple[esc];
        if (ch === undefined) throw this.error("Invalid \\escape", end - 2);
        out += ch;
        continue;
      }
      next++;
      end = next + 4;
      if (end >= len) throw this.error("Invalid \\uXXXX escape", next - 1);
      let code = this.hex4(next, end);
      next = end;
      // A high surrogate followed by \uDC00-\uDFFF combines into one code point.
      if (code >= 0xd800 && code <= 0xdbff && end + 6 < len && s[next] === "\\" && s[next + 1] === "u") {
        const low = this.hex4(next + 2, end + 6);
        if (low >= 0xdc00 && low <= 0xdfff) {
          code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
          end += 6;
        }
      }
      out += String.fromCodePoint(code);
    }
  }

  private hex4(from: number, end: number): number {
    let code = 0;
    for (let i = from; i < end; i++) {
      const d = parseInt(this.s[i] ?? "", 16);
      if (Number.isNaN(d) || !/[0-9a-fA-F]/.test(this.s[i] ?? "")) {
        throw this.error("Invalid \\uXXXX escape", end - 5);
      }
      code = code * 16 + d;
    }
    return code;
  }
}

// --- Serialisation -----------------------------------------------------------

/**
 * How floats are written:
 * - "canonical": `json.dumps` (Python repr, NaN/Infinity literals), used for seed hashing
 * - "python":    Starlette's JSONResponse (Python repr; NaN raises, so the request 500s)
 * - "pydantic":  pydantic-core's serializer used for response models (NaN/Infinity -> null)
 */
export type FloatStyle = "canonical" | "python" | "pydantic";

export interface DumpOptions {
  sortKeys?: boolean;
  floats: FloatStyle;
}

export function dumps(value: Json, options: DumpOptions): string {
  const parts: string[] = [];
  write(value, options, parts, 0);
  return parts.join("");
}

function write(value: Json, options: DumpOptions, out: string[], depth: number): void {
  if (depth > MAX_DEPTH + 8) throw new EncodeError("too deep");
  if (value === null) out.push("null");
  else if (value === true) out.push("true");
  else if (value === false) out.push("false");
  else if (typeof value === "string") out.push(quote(value));
  else if (typeof value === "bigint") out.push(value.toString());
  else if (typeof value === "number") out.push(formatJsonFloat(value, options.floats));
  else if (Array.isArray(value)) {
    out.push("[");
    value.forEach((item, i) => {
      if (i) out.push(",");
      write(item, options, out, depth + 1);
    });
    out.push("]");
  } else {
    let keys = [...value.keys()];
    if (options.sortKeys) keys = keys.sort(compareCodePoints);
    out.push("{");
    keys.forEach((key, i) => {
      if (i) out.push(",");
      out.push(quote(key), ":");
      write(value.get(key) as Json, options, out, depth + 1);
    });
    out.push("}");
  }
}

function formatJsonFloat(x: number, style: FloatStyle): string {
  if (Number.isFinite(x)) return style === "pydantic" ? pydanticFloat(x) : pythonFloatRepr(x);
  if (style === "pydantic") return "null";
  if (style === "python") throw new EncodeError("Out of range float values are not JSON compliant");
  return Number.isNaN(x) ? "NaN" : x > 0 ? "Infinity" : "-Infinity";
}

/** JSON string escaping shared by json.dumps(ensure_ascii=False) and pydantic. */
function quote(s: string): string {
  if (!s.isWellFormed()) throw new EncodeError("surrogates not allowed");
  // JSON.stringify escapes exactly the same characters for well-formed strings.
  return JSON.stringify(s);
}

/** Python sorts str keys by code point; JS compares UTF-16 code units. */
export function compareCodePoints(a: string, b: string): number {
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) {
    const x = a.codePointAt(i) as number;
    const y = b.codePointAt(i) as number;
    if (x !== y) return x - y;
    if (x > 0xffff) i++;
  }
  return a.length - b.length;
}

/**
 * Shortest round-trip digits and the decimal exponent of the first digit.
 *
 * Number#toString must pick the even digit when two shortest strings are equally close
 * (x = 758379700834392.25), as Python's repr does; toExponential() may round up instead.
 */
function shortestDigits(x: number): [string, number] {
  const [mantissa, exponent = "0"] = String(Math.abs(x)).split("e");
  const [int, frac = ""] = (mantissa as string).split(".");
  const all = (int as string) + frac;
  const lead = all.length - all.replace(/^0+/, "").length;
  const digits = all.slice(lead).replace(/0+$/, "") || "0";
  return [digits, (int as string).length - 1 - lead + Number(exponent)];
}

function formatFloat(x: number, low: number, high: number, padExponent: boolean, plusSign: boolean): string {
  if (x === 0) return Object.is(x, -0) ? "-0.0" : "0.0";
  const sign = x < 0 ? "-" : "";
  const [digits, exp] = shortestDigits(x);
  if (exp >= low && exp < high) {
    if (exp < 0) return `${sign}0.${"0".repeat(-exp - 1)}${digits}`;
    const int = digits.slice(0, exp + 1).padEnd(exp + 1, "0");
    return `${sign}${int}.${digits.slice(exp + 1) || "0"}`;
  }
  const mantissa = digits.length > 1 ? `${digits[0]}.${digits.slice(1)}` : digits;
  const e = String(Math.abs(exp)).padStart(padExponent ? 2 : 1, "0");
  return `${sign}${mantissa}e${exp < 0 ? "-" : plusSign ? "+" : ""}${e}`;
}

/** `repr(float)`: fixed notation for 1e-4 <= |x| < 1e16, else `1e-05` / `1e+16`. */
export const pythonFloatRepr = (x: number) => formatFloat(x, -4, 16, true, true);
/**
 * pydantic-core as bundled in the Python Workers runtime: fixed notation for
 * 1e-5 <= |x| < 1e16, else `1e-6` / `1e16` (newer pydantic-core writes `1e+16`).
 */
export const pydanticFloat = (x: number) => formatFloat(x, -5, 16, false, false);

// --- Python repr() ----------------------------------------------------------

const NON_PRINTABLE = /[\p{Cc}\p{Cf}\p{Cs}\p{Co}\p{Cn}\p{Zl}\p{Zp}\p{Zs}]/u;

/** `repr(str)`, used in error messages. */
export function pyReprStr(s: string): string {
  const q = s.includes("'") && !s.includes('"') ? '"' : "'";
  let out = q;
  for (const ch of s) {
    const cp = ch.codePointAt(0) as number;
    if (ch === q || ch === "\\") out += "\\" + ch;
    else if (ch === "\t") out += "\\t";
    else if (ch === "\n") out += "\\n";
    else if (ch === "\r") out += "\\r";
    else if (cp < 0x20 || cp === 0x7f || (ch !== " " && NON_PRINTABLE.test(ch))) {
      if (cp <= 0xff) out += "\\x" + cp.toString(16).padStart(2, "0");
      else if (cp <= 0xffff) out += "\\u" + cp.toString(16).padStart(4, "0");
      else out += "\\U" + cp.toString(16).padStart(8, "0");
    } else out += ch;
  }
  return out + q;
}

/** `repr()` of a decoded JSON value: None/True/False, dicts as {'k': v}. */
export function pyRepr(value: Json): string {
  if (value === null) return "None";
  if (value === true) return "True";
  if (value === false) return "False";
  if (typeof value === "string") return pyReprStr(value);
  if (typeof value === "bigint") return value.toString();
  if (typeof value === "number") {
    if (Number.isNaN(value)) return "nan";
    if (!Number.isFinite(value)) return value > 0 ? "inf" : "-inf";
    return pythonFloatRepr(value);
  }
  if (Array.isArray(value)) return `[${value.map(pyRepr).join(", ")}]`;
  return `{${[...value].map(([k, v]) => `${pyReprStr(k)}: ${pyRepr(v)}`).join(", ")}}`;
}

/** `str()`: like repr, but a str is returned as is. */
export const pyStr = (value: Json) => (typeof value === "string" ? value : pyRepr(value));

// --- Python string helpers ----------------------------------------------------

/** Characters `str.isspace()` treats as whitespace (and `str.strip()` removes). */
const PY_SPACE = new Set([
  0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x1c, 0x1d, 0x1e, 0x1f, 0x20, 0x85, 0xa0, 0x1680,
  0x2000, 0x2001, 0x2002, 0x2003, 0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200a,
  0x2028, 0x2029, 0x202f, 0x205f, 0x3000,
]);

/** Characters Rust's `str::trim` removes (White_Space property), used by pydantic-core. */
const RUST_SPACE = new Set([
  0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x20, 0x85, 0xa0, 0x1680, 0x2000, 0x2001, 0x2002, 0x2003,
  0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200a, 0x2028, 0x2029, 0x202f, 0x205f, 0x3000,
]);

function stripWith(s: string, set: Set<number>): string {
  let start = 0;
  let end = s.length;
  while (start < end && set.has(s.charCodeAt(start))) start++;
  while (end > start && set.has(s.charCodeAt(end - 1))) end--;
  return s.slice(start, end);
}

export const pyStrip = (s: string) => stripWith(s, PY_SPACE);
export const rustTrim = (s: string) => stripWith(s, RUST_SPACE);

/** `len(str)`: code points, not UTF-16 units. */
export function pyLen(s: string): number {
  let n = 0;
  for (const _ of s) n++;
  return n;
}
