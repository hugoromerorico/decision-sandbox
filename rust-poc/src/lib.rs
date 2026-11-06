//! Decision Sandbox, Rust port of the Python Worker (src/app.py).
//!
//! Same routes, status codes, headers and bodies; seeded answers are byte-identical.
//! The request pipeline reproduces the Python app's middleware stack, outermost first:
//! Starlette's ServerErrorMiddleware (plain 500), CORSMiddleware, SandboxMiddleware
//! (size limit, X-Request-Id and synthetic headers), then FastAPI routing and
//! dependency order: body parsing, API key, rate limits, parameter validation.

mod auth;
mod config;
mod generator;
mod json;
mod pyrandom;
mod scenarios;
mod schemas;

use std::time::Duration;

use worker::{event, Context, Delay, Env, Headers, Method, Request, Response, Url};

use auth::{enforce_rate_limits, metrics_snapshot, require_admin, require_api_key, HttpError};
use config::*;
use generator::{generate, new_seed, random_hex};
use json::{dumps, loads_bytes, py_repr_str, DecodeError, EncodeError, FloatStyle, Json, Obj, PyStr};
use scenarios::{catalogue, parse_scenario};
use schemas::{int_param, Body, LocPart, SystemOneRequest, ValidationError, Validator};

const TOO_LARGE: &str = "Request body exceeds the sandbox limit of 65536 bytes.";
const EXPOSE_HEADERS: &str = "X-Request-Id, X-Sandbox-Synthetic, X-Sandbox-Model, X-Sandbox-Seed, X-Sandbox-Scenario, Retry-After";
const RELEASE_DATE: &str = "2026-10-04";

const OPENAPI_JSON: &str = include_str!("../../shared/openapi.json");
const DOCS_HTML: &str = include_str!("../../shared/docs.html");
const OAUTH_REDIRECT_HTML: &str = include_str!("../../shared/oauth2-redirect.html");
const REDOC_HTML: &str = include_str!("../../shared/redoc.html");

/// A response before it becomes a Fetch Response, so middleware can add headers.
struct Reply {
    status: u16,
    body: String,
    headers: Vec<(String, String)>,
}

impl Reply {
    fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    fn headers<'a>(mut self, extra: impl IntoIterator<Item = (&'a str, String)>) -> Self {
        for (name, value) in extra {
            self.headers.push((name.to_string(), value));
        }
        self
    }
}

/// Failures that end the request with a plain 500, as Python's ServerErrorMiddleware
/// (or the Python Worker crashing) would.
enum Fatal {
    Encode,
    Worker(worker::Error),
}

impl From<EncodeError> for Fatal {
    fn from(_: EncodeError) -> Self {
        Fatal::Encode
    }
}

impl From<worker::Error> for Fatal {
    fn from(err: worker::Error) -> Self {
        Fatal::Worker(err)
    }
}

type Out = Result<Reply, Fatal>;

fn json(status: u16, body: &Json, floats: FloatStyle) -> Out {
    Ok(Reply { status, body: dumps(body, false, floats)?, headers: vec![("content-type".into(), "application/json".into())] })
}

fn detail(status: u16, message: &str) -> Out {
    json(status, &Json::Obj(Obj::new().with("detail", Json::str(message))), FloatStyle::Python)
}

fn http_error(err: HttpError) -> Out {
    Ok(detail(err.status, err.detail)?.headers(err.headers))
}

fn text(status: u16, body: &str, content_type: &str) -> Reply {
    Reply { status, body: body.to_string(), headers: vec![("content-type".into(), content_type.into())] }
}

fn strs<'a>(items: impl IntoIterator<Item = &'a str>) -> Json {
    Json::Arr(items.into_iter().map(Json::str).collect())
}

// --- Routes ------------------------------------------------------------------------

fn models() -> Json {
    let model = |name: &str, description: String| {
        Json::Obj(Obj::new().with("name", Json::str(name)).with("description", Json::str(description)).with("release_date", Json::str(RELEASE_DATE)))
    };
    Json::Obj(Obj::new().with(
        "models",
        Json::Arr(vec![
            model(SANDBOX_MODEL, "Synthetic decision generator. Returns random, seeded or scenario-driven answers. Not an AI model.".into()),
            model(
                "jev-latest",
                format!("Compatibility alias for {SANDBOX_MODEL} so existing integrations work unchanged. Answers are synthetic; this is not TypeSafe's Jev model."),
            ),
        ]),
    ))
}

fn route_method(path: &str) -> Option<Method> {
    match path {
        "/" | "/health" | "/sandbox/scenarios" | "/sandbox/stats" | "/v1/models" | "/openapi.json" | "/docs" | "/docs/oauth2-redirect" | "/redoc" => {
            Some(Method::Get)
        }
        "/v1/systemone" => Some(Method::Post),
        _ => None,
    }
}

async fn dispatch(path: &str, req: &mut Request, env: &Env, url: &Url) -> Out {
    let result = match path {
        "/" => json(
            200,
            &Json::Obj(
                Obj::new()
                    .with("name", Json::str("Decision Sandbox"))
                    .with("description", Json::str("A free synthetic decision API for testing Jev-compatible integrations."))
                    .with("synthetic", Json::Bool(true))
                    .with("affiliated_with_typesafe", Json::Bool(false))
                    .with("model", Json::str(SANDBOX_MODEL))
                    .with("version", Json::str(SANDBOX_VERSION))
                    .with("docs", Json::str("/docs"))
                    .with("openapi", Json::str("/openapi.json"))
                    .with("endpoints", strs(["POST /v1/systemone", "GET /v1/models", "GET /sandbox/scenarios", "GET /health"])),
            ),
            FloatStyle::Python,
        ),
        "/health" => json(
            200,
            &Json::Obj(
                Obj::new()
                    .with("status", Json::str("ok"))
                    .with("model", Json::str(SANDBOX_MODEL))
                    .with("version", Json::str(SANDBOX_VERSION))
                    .with("synthetic", Json::Bool(true)),
            ),
            FloatStyle::Python,
        ),
        "/sandbox/scenarios" => {
            let list = catalogue()
                .into_iter()
                .map(|(name, description)| Json::Obj(Obj::new().with("name", Json::str(name)).with("description", Json::str(description))))
                .collect();
            json(200, &Json::Obj(Obj::new().with("scenarios", Json::Arr(list))), FloatStyle::Python)
        }
        "/sandbox/stats" => match require_admin(req, env) {
            Ok(()) => json(200, &metrics_snapshot(), FloatStyle::Python),
            Err(err) => http_error(err),
        },
        "/v1/models" => {
            if let Err(err) = require_api_key(req, env, url.path()) {
                return http_error(err);
            }
            if let Err(err) = enforce_rate_limits(req, env).await {
                return http_error(err);
            }
            json(200, &models(), FloatStyle::Pydantic)
        }
        "/v1/systemone" => systemone(req, env, url).await,
        "/openapi.json" => Ok(text(200, OPENAPI_JSON, "application/json")),
        "/docs" => Ok(text(200, DOCS_HTML, "text/html; charset=utf-8")),
        "/docs/oauth2-redirect" => Ok(text(200, OAUTH_REDIRECT_HTML, "text/html; charset=utf-8")),
        "/redoc" => Ok(text(200, REDOC_HTML, "text/html; charset=utf-8")),
        _ => detail(404, "Not Found"),
    };
    result
}

// --- POST /v1/systemone ----------------------------------------------------------------

fn is_json_content_type(value: &str) -> bool {
    // email.message's get_content_type(): anything but exactly one "/" means text/plain.
    let kind = value.split(';').next().unwrap_or("").trim().to_lowercase();
    let parts: Vec<&str> = kind.split('/').collect();
    let [main, sub] = parts.as_slice() else {
        return false;
    };
    let sub = sub.trim();
    main.trim() == "application" && (sub == "json" || sub.ends_with("+json"))
}

/// FastAPI reads and decodes the body before resolving any dependency.
fn read_body(raw: &[u8], content_type: Option<&str>) -> Result<Body, Out> {
    if raw.is_empty() {
        return Ok(Body::None);
    }
    if content_type.is_some_and(is_json_content_type) {
        return match loads_bytes(raw) {
            Ok(value) => Ok(Body::Json(value)),
            Err(DecodeError::Json { msg, pos }) => {
                let error = Obj::new()
                    .with("type", Json::str("json_invalid"))
                    .with("loc", Json::Arr(vec![Json::str("body"), Json::int(pos as i64)]))
                    .with("msg", Json::str("JSON decode error"))
                    .with("input", Json::Obj(Obj::new()))
                    .with("ctx", Json::Obj(Obj::new().with("error", Json::str(msg))));
                Err(json(422, &Json::Obj(Obj::new().with("detail", Json::Arr(vec![Json::Obj(error)]))), FloatStyle::Python))
            }
            Err(DecodeError::Body) => Err(detail(400, "There was an error parsing the body")),
        };
    }
    // Without a JSON content type the body stays bytes; echoing undecodable bytes in a
    // validation error fails in Python, so they become a string that fails the same way.
    Ok(Body::Text(match std::str::from_utf8(raw) {
        Ok(text) => PyStr::new(text),
        Err(_) => PyStr { s: String::new(), lone_surrogate: true },
    }))
}

fn last_query(url: &Url, name: &str) -> Option<String> {
    url.query_pairs().filter(|(k, _)| k == name).map(|(_, v)| v.into_owned()).last()
}

fn header(req: &Request, name: &str) -> Option<String> {
    req.headers().get(name).ok().flatten()
}

async fn sleep_ms(ms: u64) {
    Delay::from(Duration::from_millis(ms)).await;
}

async fn systemone(req: &mut Request, env: &Env, url: &Url) -> Out {
    let raw = req.bytes().await?;
    if raw.len() > MAX_BODY_BYTES {
        return detail(413, TOO_LARGE);
    }
    let body = match read_body(&raw, header(req, "content-type").as_deref()) {
        Ok(body) => body,
        Err(reply) => return reply,
    };

    if let Err(err) = require_api_key(req, env, url.path()) {
        return http_error(err);
    }
    if let Err(err) = enforce_rate_limits(req, env).await {
        return http_error(err);
    }

    let mut errors: Vec<ValidationError> = Vec::new();
    let query_loc = vec![LocPart::from("query"), LocPart::from("delay_ms")];
    let header_loc = vec![LocPart::from("header"), LocPart::from("X-Sandbox-Delay-Ms")];
    let delay_query = last_query(url, "delay_ms").and_then(|raw| int_param(&raw, query_loc, MAX_DELAY_MS, &mut errors));
    let delay_header = header(req, "x-sandbox-delay-ms").and_then(|raw| int_param(&raw, header_loc, MAX_DELAY_MS, &mut errors));
    let mut validator = Validator::default();
    let request = validator.request(&body);
    errors.extend(validator.errors);
    if !errors.is_empty() {
        let detail = Json::Arr(errors.iter().map(ValidationError::to_json).collect());
        return json(422, &Json::Obj(Obj::new().with("detail", detail)), FloatStyle::Python);
    }
    let request = request.expect("validated without errors");

    let seed = last_query(url, "seed").or_else(|| header(req, "x-sandbox-seed"));
    if let Some(seed) = &seed {
        let n = seed.chars().count();
        if n == 0 || n > MAX_SEED_LENGTH {
            return detail(400, &format!("Seed must be 1 to {MAX_SEED_LENGTH} characters."));
        }
    }
    let seed = seed.unwrap_or_else(new_seed);

    let scenario_raw = last_query(url, "scenario").or_else(|| header(req, "x-sandbox-scenario"));
    let chosen = match parse_scenario(scenario_raw.as_deref()) {
        Ok(s) => s,
        Err(unknown) => {
            return detail(400, &format!("Unknown sandbox scenario {}. See GET /sandbox/scenarios for the list.", py_repr_str(&unknown.0)));
        }
    };

    let sandbox_headers = vec![("X-Sandbox-Seed", seed.clone()), ("X-Sandbox-Scenario", chosen.name.clone())];
    if let Some(delay) = delay_query.or(delay_header).filter(|d| *d > 0) {
        sleep_ms(delay as u64).await;
    }

    if let Some(error) = chosen.error {
        return simulated_error(error, &request, sandbox_headers).await;
    }
    Ok(json(200, &generate(&request, &seed, &chosen)?, FloatStyle::Pydantic)?.headers(sandbox_headers))
}

async fn simulated_error(error: &str, request: &SystemOneRequest, headers: Vec<(&'static str, String)>) -> Out {
    let reply = match error {
        "invalid-response" => {
            // Violates the contract: wrong types, a missing field and an unknown answer type.
            let name = request.questions[0].0.clone();
            let answer = Obj::new().with("type", Json::str("unknown")).with("noul", Json::str("yes")).with("confidence", Json::Null);
            let mut answers = Obj::new();
            answers.insert(name, Json::Obj(answer));
            json(200, &Json::Obj(Obj::new().with("model", Json::str(SANDBOX_MODEL)).with("answers", Json::Obj(answers))), FloatStyle::Python)?
        }
        "validation-error" => {
            let error = Obj::new().with("loc", strs(["body", "state"])).with("msg", Json::str("Field required")).with("type", Json::str("missing"));
            json(422, &Json::Obj(Obj::new().with("detail", Json::Arr(vec![Json::Obj(error)]))), FloatStyle::Python)?
        }
        "unauthorized" => detail(401, "Not authenticated")?.header("WWW-Authenticate", "Bearer"),
        "rate-limit" => detail(429, "Rate limit exceeded (simulated).")?.header("Retry-After", "1"),
        "unavailable" => detail(503, "Service unavailable (simulated).")?.header("Retry-After", "1"),
        "timeout" => {
            sleep_ms(TIMEOUT_SCENARIO_MS).await;
            detail(504, "Gateway timeout (simulated).")?
        }
        _ => detail(500, "Internal server error (simulated).")?,
    };
    Ok(reply.headers(headers))
}

// --- Routing (Starlette router + FastAPI exception handlers) ------------------------------

fn percent_decode(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&path[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| path.to_string())
}

async fn route(req: &mut Request, env: &Env, url: &Url) -> Out {
    let path = percent_decode(url.path());
    let Some(method) = route_method(&path) else {
        // redirect_slashes: /health/ -> /health and /health -> /health/, if that route exists.
        if path != "/" {
            let other = if path.ends_with('/') { path.trim_end_matches('/').to_string() } else { format!("{path}/") };
            if route_method(&other).is_some() {
                let mut location = url.clone();
                location.set_path(&other);
                return Ok(Reply { status: 307, body: String::new(), headers: vec![("location".into(), location.to_string())] });
            }
        }
        return detail(404, "Not Found");
    };
    if req.method() != method {
        return Ok(detail(405, "Method Not Allowed")?.header("Allow", method.to_string()));
    }
    dispatch(&path, req, env, url).await
}

/// SandboxMiddleware: body size limit and the headers stamped on every response.
async fn sandbox(req: &mut Request, env: &Env, url: &Url) -> Out {
    let request_id = format!("req_{}", random_hex(12));
    let too_large = header(req, "content-length")
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .is_some_and(|v| {
            let digits = v.trim_start_matches('0');
            digits.len() > 6 || digits.parse::<usize>().unwrap_or(0) > MAX_BODY_BYTES
        });
    let reply = if too_large { detail(413, TOO_LARGE)? } else { route(req, env, url).await? };
    Ok(reply
        .header("x-request-id", request_id)
        .header("x-sandbox-synthetic", "true")
        .header("x-sandbox-model", SANDBOX_MODEL))
}

/// CORSMiddleware(allow_origins=["*"], allow_methods=[GET, POST, OPTIONS], allow_headers=["*"]).
fn preflight(req: &Request) -> Reply {
    let mut headers = vec![
        ("Vary", "Origin, Access-Control-Request-Method, Access-Control-Request-Headers, Access-Control-Request-Private-Network".to_string()),
        ("Access-Control-Allow-Origin", "*".into()),
        ("Access-Control-Allow-Methods", "GET, POST, OPTIONS".into()),
        ("Access-Control-Max-Age", "600".into()),
    ];
    let mut failures = Vec::new();
    let method = header(req, "access-control-request-method").unwrap_or_default();
    if !["GET", "POST", "OPTIONS"].contains(&method.as_str()) {
        failures.push("method");
    }
    if let Some(requested) = header(req, "access-control-request-headers") {
        headers.push(("Access-Control-Allow-Headers", requested));
    }
    if req.headers().has("access-control-request-private-network").unwrap_or(false) {
        failures.push("private-network");
    }
    let (status, body) = if failures.is_empty() { (200, "OK".to_string()) } else { (400, format!("Disallowed CORS {}", failures.join(", "))) };
    text(status, &body, "text/plain; charset=utf-8").headers(headers)
}

/// The Python Worker's ASGI adapter encodes header values as latin-1 and decodes them as
/// UTF-8. Values that fail either step (e.g. a non-ASCII seed) crash the Python Worker
/// with a 500; this reproduces that, and the value it would send otherwise.
fn asgi_header_value(value: &str) -> Result<String, Fatal> {
    let bytes: Vec<u8> = value.chars().map(|c| u8::try_from(c as u32).map_err(|_| Fatal::Encode)).collect::<Result<_, _>>()?;
    String::from_utf8(bytes).map_err(|_| Fatal::Encode)
}

async fn handle(mut req: Request, env: &Env) -> Result<Response, Fatal> {
    let url = req.url()?;
    let origin = header(&req, "origin");
    let reply = if origin.is_some() && req.method() == Method::Options && req.headers().has("access-control-request-method")? {
        preflight(&req)
    } else {
        let mut reply = sandbox(&mut req, env, &url).await?;
        if origin.is_some() {
            reply = reply.header("Access-Control-Allow-Origin", "*").header("Access-Control-Expose-Headers", EXPOSE_HEADERS);
        }
        reply.header("Vary", "Origin")
    };
    let headers = Headers::new();
    for (name, value) in &reply.headers {
        headers.set(name, &asgi_header_value(value)?)?;
    }
    let response = if reply.body.is_empty() { Response::empty()? } else { Response::from_bytes(reply.body.into_bytes())? };
    Ok(response.with_status(reply.status).with_headers(headers))
}

fn internal_error() -> worker::Result<Response> {
    let headers = Headers::new();
    headers.set("content-type", "text/plain; charset=utf-8")?;
    Ok(Response::from_bytes(b"Internal Server Error".to_vec())?.with_status(500).with_headers(headers))
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> worker::Result<Response> {
    auth::init_metrics();
    match handle(req, &env).await {
        Ok(response) => Ok(response),
        Err(Fatal::Encode) => internal_error(),
        Err(Fatal::Worker(err)) => {
            // Starlette's ServerErrorMiddleware, or the Python Worker failing outright.
            worker::console_error!("{err}");
            internal_error()
        }
    }
}

