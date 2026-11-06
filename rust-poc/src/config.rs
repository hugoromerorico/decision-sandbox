//! Mirrors src/sandbox/__init__.py and src/sandbox/limits.py.

pub const SANDBOX_MODEL: &str = "decision-sandbox-v1";
pub const SANDBOX_VERSION: &str = "0.1.0";

pub const MAX_BODY_BYTES: usize = 64 * 1024;
pub const MAX_QUESTIONS: usize = 32;
pub const MAX_QUESTION_NAME_LENGTH: usize = 128;
pub const MAX_CHOICES: usize = 64;
pub const MAX_CHOICE_NAME_LENGTH: usize = 256;
pub const MAX_SCORE_LEVELS: usize = 32;
pub const MAX_SEED_LENGTH: usize = 128;
pub const MAX_DELAY_MS: i64 = 5_000;
pub const TIMEOUT_SCENARIO_MS: u64 = 10_000;

// Requests per 60 seconds. Enforced by the rate-limit bindings in wrangler.jsonc;
// these values are only used by the in-memory fallback.
pub const IP_REQUESTS_PER_MINUTE: u32 = 60;
pub const KEY_REQUESTS_PER_MINUTE: u32 = 300;
pub const WINDOW_SECONDS: u64 = 60;

pub const PUBLIC_KEYS: [&str; 1] = ["github-2026"];
