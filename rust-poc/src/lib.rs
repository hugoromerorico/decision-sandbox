//! Minimal Rust Worker for measuring cold start against the Python Worker.
//!
//! Serves the same routes bench/latency.py hits (GET /health, POST /v1/systemone
//! with a Bearer key) but returns a fixed answer: no generator, no rate limiting.
//! The point is to measure runtime boot cost, not to port the sandbox.

use serde_json::{json, Value};
use worker::*;

const PUBLIC_KEY: &str = "github-2026";

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    Router::new()
        .get("/health", |_, _| Response::from_json(&json!({"status": "ok"})))
        .post_async("/v1/systemone", |mut req, _| async move {
            let auth = req.headers().get("Authorization")?.unwrap_or_default();
            if auth != format!("Bearer {PUBLIC_KEY}") {
                return Ok(Response::from_json(&json!({"detail": "Not authenticated"}))?
                    .with_status(401));
            }

            // Parse the body so the request does comparable work to the real endpoint.
            let body: Value = match req.json().await {
                Ok(body) => body,
                Err(_) => {
                    return Ok(Response::from_json(&json!({"detail": "Invalid JSON body"}))?
                        .with_status(422))
                }
            };

            let answers: serde_json::Map<String, Value> = body["questions"]
                .as_object()
                .map(|questions| {
                    questions
                        .keys()
                        .map(|name| (name.clone(), json!({"type": "noul", "noul": 0.9312})))
                        .collect()
                })
                .unwrap_or_default();
            let output_tokens = 2 * answers.len();

            let mut res = Response::from_json(&json!({
                "model": "decision-sandbox-v1",
                "answers": answers,
                "usage": {"input_tokens": 20, "output_tokens": output_tokens},
            }))?;
            res.headers_mut().set("X-Sandbox-Synthetic", "true")?;
            Ok(res)
        })
        .run(req, env)
        .await
}
