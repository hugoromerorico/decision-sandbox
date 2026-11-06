/** Mirrors src/sandbox/__init__.py and src/sandbox/limits.py. */

export const SANDBOX_MODEL = "decision-sandbox-v1";
export const SANDBOX_VERSION = "0.1.0";

export const MAX_BODY_BYTES = 64 * 1024;
export const MAX_QUESTIONS = 32;
export const MAX_QUESTION_NAME_LENGTH = 128;
export const MAX_CHOICES = 64;
export const MAX_CHOICE_NAME_LENGTH = 256;
export const MAX_SCORE_LEVELS = 32;
export const MAX_SEED_LENGTH = 128;
export const MAX_DELAY_MS = 5_000;
export const TIMEOUT_SCENARIO_MS = 10_000;

// Requests per 60 seconds. Enforced by the rate-limit bindings in wrangler.jsonc;
// these values are only used by the in-memory fallback.
export const IP_REQUESTS_PER_MINUTE = 60;
export const KEY_REQUESTS_PER_MINUTE = 300;
export const WINDOW_SECONDS = 60;

export const PUBLIC_KEYS = ["github-2026"];
