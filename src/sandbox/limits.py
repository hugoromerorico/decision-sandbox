"""Hard limits that keep every request cheap and bounded."""

MAX_BODY_BYTES = 64 * 1024
MAX_QUESTIONS = 32
MAX_QUESTION_NAME_LENGTH = 128
MAX_CHOICES = 64
MAX_CHOICE_NAME_LENGTH = 256
MAX_SCORE_LEVELS = 32
MAX_SEED_LENGTH = 128
MAX_DELAY_MS = 5_000
TIMEOUT_SCENARIO_MS = 10_000

# Requests per 60 seconds. Enforced by the Cloudflare rate-limit bindings in
# wrangler.jsonc; these values are only used by the in-memory fallback.
IP_REQUESTS_PER_MINUTE = 60
KEY_REQUESTS_PER_MINUTE = 300
