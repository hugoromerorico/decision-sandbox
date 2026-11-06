//! API key allowlist, admin token, per-IP/per-key rate limits and in-memory request
//! counters. Mirrors src/sandbox/auth.py and src/sandbox/ratelimit.py.
//!
//! Counters live in memory only: they are per isolate and reset on eviction or deploy.

use std::cell::RefCell;
use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use worker::{Env, Request};

use crate::config::*;
use crate::json::{py_strip, Json, Obj};

/// An HTTPException: FastAPI answers `{"detail": ...}` with these headers.
pub struct HttpError {
    pub status: u16,
    pub detail: &'static str,
    pub headers: Vec<(&'static str, String)>,
}

fn unauthorized() -> HttpError {
    HttpError { status: 401, detail: "Not authenticated", headers: vec![("WWW-Authenticate", "Bearer".into())] }
}

fn now() -> u64 {
    (worker::Date::now().as_millis() / 1000) as u64
}

fn env_value(env: &Env, name: &str) -> String {
    env.secret(name).map(|s| s.to_string()).or_else(|_| env.var(name).map(|v| v.to_string())).unwrap_or_default()
}

// --- Keys --------------------------------------------------------------------------

/// `Authorization: Bearer <key>`: the scheme is case-insensitive, the key is stripped.
pub fn api_key(req: &Request) -> Option<String> {
    let header = req.headers().get("authorization").ok().flatten().unwrap_or_default();
    let (scheme, token) = header.split_once(' ').unwrap_or((&header, ""));
    if scheme.to_lowercase() != "bearer" {
        return None;
    }
    let token = py_strip(token);
    (!token.is_empty()).then(|| token.to_string())
}

fn allowlist(env: &Env) -> Vec<String> {
    let mut keys: Vec<String> = PUBLIC_KEYS.iter().map(|k| k.to_string()).collect();
    for key in env_value(env, "SANDBOX_API_KEYS").split(',').map(py_strip).filter(|k| !k.is_empty()) {
        if !keys.iter().any(|k| k == key) {
            keys.push(key.to_string());
        }
    }
    keys
}

/// `hmac.compare_digest`: time depends on the length only, not on the content.
fn compare_digest(a: &str, b: &str) -> bool {
    let (x, y) = (a.as_bytes(), b.as_bytes());
    let other = if x.len() == y.len() { y } else { x };
    let mut diff = (x.len() != y.len()) as u8;
    for (p, q) in x.iter().zip(other) {
        diff |= p ^ q;
    }
    diff == 0
}

fn find_key(candidate: &str, keys: &[String]) -> Option<String> {
    let mut found = None;
    for key in keys {
        // No early exit.
        if compare_digest(candidate, key) {
            found = Some(key.clone());
        }
    }
    found
}

// --- Metrics -------------------------------------------------------------------------

#[derive(Default)]
struct KeyStats {
    requests: u64,
    by_endpoint: Vec<(String, u64)>,
    last_seen: u64,
}

struct Metrics {
    started_at: u64,
    unauthorized: u64,
    keys: Vec<(String, KeyStats)>,
}

thread_local! {
    static METRICS: RefCell<Option<Metrics>> = const { RefCell::new(None) };
    static LOCAL_LIMITER: RefCell<BTreeMap<String, (u64, u32)>> = const { RefCell::new(BTreeMap::new()) };
}

fn with_metrics<T>(f: impl FnOnce(&mut Metrics) -> T) -> T {
    METRICS.with(|m| {
        let mut m = m.borrow_mut();
        f(m.get_or_insert_with(|| Metrics { started_at: now(), unauthorized: 0, keys: Vec::new() }))
    })
}

/// Starts the uptime clock when the isolate loads, like the Python module-level Metrics().
pub fn init_metrics() {
    with_metrics(|_| ());
}

fn record(key: &str, endpoint: &str) {
    with_metrics(|m| {
        let index = match m.keys.iter().position(|(k, _)| k == key) {
            Some(i) => i,
            None => {
                m.keys.push((key.to_string(), KeyStats::default()));
                m.keys.len() - 1
            }
        };
        let entry = &mut m.keys[index].1;
        entry.requests += 1;
        match entry.by_endpoint.iter_mut().find(|(e, _)| e == endpoint) {
            Some((_, n)) => *n += 1,
            None => entry.by_endpoint.push((endpoint.to_string(), 1)),
        }
        entry.last_seen = now();
    });
}

pub fn metrics_snapshot() -> Json {
    with_metrics(|m| {
        let mut keys = Obj::new();
        let mut total = 0;
        for (key, e) in &m.keys {
            total += e.requests;
            let by_endpoint: Obj = e.by_endpoint.iter().map(|(k, v)| (k.clone(), Json::int(*v as i64))).collect();
            let stats = Obj::new()
                .with("requests", Json::int(e.requests as i64))
                .with("by_endpoint", Json::Obj(by_endpoint))
                .with("last_seen", Json::int(e.last_seen as i64));
            keys = keys.with(key, Json::Obj(stats));
        }
        Json::Obj(
            Obj::new()
                .with("started_at", Json::int(m.started_at as i64))
                .with("total", Json::int(total as i64))
                .with("unauthorized", Json::int(m.unauthorized as i64))
                .with("keys", Json::Obj(keys))
                .with("note", Json::str("In-memory, per isolate; resets on eviction or deploy.")),
        )
    })
}

pub fn require_api_key(req: &Request, env: &Env, path: &str) -> Result<String, HttpError> {
    let key = api_key(req).and_then(|candidate| find_key(&candidate, &allowlist(env)));
    let Some(key) = key else {
        with_metrics(|m| m.unauthorized += 1);
        return Err(unauthorized());
    };
    record(&key, path);
    let line = Obj::new().with("event", Json::str("request")).with("key_label", Json::str(key.clone())).with("endpoint", Json::str(path));
    if let Ok(line) = crate::json::dumps(&Json::Obj(line), false, crate::json::FloatStyle::Python) {
        worker::console_log!("{line}");
    }
    Ok(key)
}

pub fn require_admin(req: &Request, env: &Env) -> Result<(), HttpError> {
    let token = env_value(env, "SANDBOX_ADMIN_TOKEN");
    if token.is_empty() {
        return Err(HttpError { status: 404, detail: "Not found", headers: vec![] });
    }
    match api_key(req) {
        Some(candidate) if compare_digest(&candidate, &token) => Ok(()),
        _ => Err(unauthorized()),
    }
}

// --- Rate limits -----------------------------------------------------------------------

/// In-memory fixed window, used only when a rate-limit binding is missing.
fn local_allow(key: &str, limit: u32) -> bool {
    LOCAL_LIMITER.with(|windows| {
        let mut windows = windows.borrow_mut();
        let window = now() / WINDOW_SECONDS;
        let (start, mut count) = windows.get(key).copied().unwrap_or((window, 0));
        if start != window {
            if windows.len() > 10_000 {
                windows.clear();
            }
            count = 0;
        }
        windows.insert(key.to_string(), (window, count + 1));
        count < limit
    })
}

async fn allow(env: &Env, binding: &str, key: String, fallback_limit: u32) -> bool {
    let Ok(limiter) = env.rate_limiter(binding) else {
        return local_allow(&key, fallback_limit);
    };
    match limiter.limit(key).await {
        Ok(outcome) => outcome.success,
        Err(err) => {
            // Fail open: an unavailable limiter should not take the sandbox down.
            worker::console_error!("Rate limiter {binding} failed: {err}");
            true
        }
    }
}

fn client_ip(req: &Request) -> String {
    req.headers().get("cf-connecting-ip").ok().flatten().filter(|ip| !ip.is_empty()).unwrap_or_else(|| "unknown".into())
}

pub async fn enforce_rate_limits(req: &Request, env: &Env) -> Result<(), HttpError> {
    let mut checks = vec![("IP_RATE_LIMITER", format!("ip:{}", client_ip(req)), IP_REQUESTS_PER_MINUTE)];
    if let Some(key) = api_key(req) {
        // Only a hash of the key is used; keys are never stored or logged.
        let digest: String = Sha256::digest(key.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        checks.push(("KEY_RATE_LIMITER", format!("key:{}", &digest[..32]), KEY_REQUESTS_PER_MINUTE));
    }
    for (binding, key, limit) in checks {
        if !allow(env, binding, key, limit).await {
            return Err(HttpError {
                status: 429,
                detail: "Rate limit exceeded. Decision Sandbox is a free shared service; please slow down.",
                headers: vec![("Retry-After", WINDOW_SECONDS.to_string())],
            });
        }
    }
    Ok(())
}
