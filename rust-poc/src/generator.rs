//! Synthetic answer generation, ported from src/sandbox/generator.py.
//!
//! Each question gets its own RNG derived from (seed, state, question name, question
//! body), so answers are reproducible for a given seed, identical to the Python
//! Worker's, and adding or removing one question never changes the others.

use sha2::{Digest, Sha256};

use crate::config::SANDBOX_MODEL;
use crate::json::{dumps, EncodeError, FloatStyle, Json, Obj, PyStr};
use crate::pyrandom::{py_round4, py_sum, PyRandom};
use crate::scenarios::{ChoicePick, Scenario, ScorePick, Spread};
use crate::schemas::{Question, SystemOneRequest};

pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::getrandom(&mut buf).expect("crypto.getRandomValues is available");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn new_seed() -> String {
    random_hex(8)
}

fn canonical_json(value: &Json) -> Result<String, EncodeError> {
    dumps(value, true, FloatStyle::Canonical)
}

/// `model_dump(mode="json")`: defaults filled in; NaN/Infinity inside questions become null.
fn dump_question(question: &Question) -> Json {
    let (kind, instructions, criteria) = match question {
        Question::Noul { instructions, criteria } => (
            "noul",
            instructions,
            criteria.as_ref().map_or(Json::Null, |(t, f)| Json::Obj(Obj::new().with("true", json_mode(t)).with("false", json_mode(f)))),
        ),
        Question::Choice { instructions, criteria } => (
            "choice",
            instructions,
            Json::Obj(criteria.iter().map(|(k, v)| (k.s.clone(), json_mode(v))).collect()),
        ),
        Question::Score { instructions, criteria } => ("score", instructions, Json::Arr(criteria.iter().map(json_mode).collect())),
    };
    Json::Obj(Obj::new().with("type", Json::str(kind)).with("instructions", json_mode(instructions)).with("criteria", criteria))
}

fn json_mode(value: &Json) -> Json {
    match value {
        Json::Float(x) if !x.is_finite() => Json::Null,
        Json::Arr(items) => Json::Arr(items.iter().map(json_mode).collect()),
        Json::Obj(obj) => {
            let mut out = Obj::new();
            for (k, v) in obj.iter() {
                out.insert(k.clone(), json_mode(v));
            }
            Json::Obj(out)
        }
        other => other.clone(),
    }
}

fn question_rng(seed: &str, state_json: &str, name: &PyStr, question: &Json) -> Result<PyRandom, EncodeError> {
    // Python's material.encode() fails on a lone surrogate.
    if name.lone_surrogate {
        return Err(EncodeError);
    }
    let material = [seed, state_json, &name.s, &canonical_json(question)?].join("\x1f");
    Ok(PyRandom::from_digest(&Sha256::digest(material.as_bytes())))
}

pub fn generate(request: &SystemOneRequest, seed: &str, scenario: &Scenario) -> Result<Json, EncodeError> {
    let state_json = canonical_json(&request.state)?;
    let dumped: Vec<Json> = request.questions.iter().map(|(_, q)| dump_question(q)).collect();
    let mut answers = Obj::new();
    for ((name, question), dump) in request.questions.iter().zip(&dumped) {
        let mut rng = question_rng(seed, &state_json, name, dump)?;
        let answer = match question {
            Question::Noul { .. } => noul(&mut rng, scenario),
            Question::Choice { criteria, .. } => choice(&mut rng, criteria, scenario),
            Question::Score { criteria, .. } => score(&mut rng, criteria, scenario),
        };
        answers.insert(name.clone(), answer);
    }

    // Plausible, deterministic token counts (roughly four characters per token).
    let mut all = Obj::new();
    for ((name, _), dump) in request.questions.iter().zip(dumped) {
        all.insert(name.clone(), dump);
    }
    let text_len = state_json.chars().count() + canonical_json(&Json::Obj(all))?.chars().count();
    let usage = Obj::new()
        .with("input_tokens", Json::int(text_len.div_ceil(4).max(1) as i64))
        .with("output_tokens", Json::int(2 * request.questions.len() as i64));

    Ok(Json::Obj(
        Obj::new().with("model", Json::str(SANDBOX_MODEL)).with("answers", Json::Obj(answers)).with("usage", Json::Obj(usage)),
    ))
}

// --- Answer types --------------------------------------------------------------

fn noul(rng: &mut PyRandom, scenario: &Scenario) -> Json {
    let value = if let Some((low, high)) = scenario.noul_range {
        rng.uniform(low, high)
    } else if scenario.spread == Spread::Peaked {
        let value = rng.uniform(0.93, 0.995);
        if rng.random() < 0.5 {
            value
        } else {
            1.0 - value
        }
    } else if scenario.spread == Spread::Flat {
        rng.uniform(0.4, 0.6)
    } else {
        // U-shaped: most answers are fairly decisive, some are uncertain.
        rng.betavariate(0.55, 0.55)
    };
    Json::Obj(Obj::new().with("type", Json::str("noul")).with("noul", Json::Float(py_round4(value))))
}

fn choice(rng: &mut PyRandom, criteria: &Obj, scenario: &Scenario) -> Json {
    let names: Vec<&PyStr> = criteria.iter().map(|(k, _)| k).collect();
    let winner = match &scenario.choice_pick {
        Some(ChoicePick::First) => Some(0),
        Some(ChoicePick::Last) => Some(names.len() - 1),
        Some(ChoicePick::Name(name)) => names.iter().position(|n| &n.s == name),
        None => None,
    };
    let probs = categorical(rng, names.len(), scenario.spread, winner);
    let best = argmax(&probs);
    let mut probabilities = Obj::new();
    for (name, p) in names.iter().zip(&probs) {
        probabilities.insert((*name).clone(), Json::Float(*p));
    }
    Json::Obj(
        Obj::new()
            .with("type", Json::str("choice"))
            .with("choice", Json::Str(names[best].clone()))
            .with("confidence", Json::Float(probs[best]))
            .with("probabilities", Json::Obj(probabilities)),
    )
}

fn score(rng: &mut PyRandom, criteria: &[Json], scenario: &Scenario) -> Json {
    let levels = criteria.len();
    let winner = match scenario.score_pick {
        Some(ScorePick::Min) => Some(0),
        Some(ScorePick::Max) => Some(levels - 1),
        Some(ScorePick::Level(level)) => Some(level.clamp(0, levels as i64 - 1) as usize),
        None => None,
    };
    let probs = if scenario.spread == Spread::Flat && winner.is_none() {
        categorical(rng, levels, Spread::Flat, None)
    } else {
        unimodal(rng, levels, scenario.spread, winner)
    };
    let best = argmax(&probs);
    let expected = py_round4(py_sum(probs.iter().enumerate().map(|(i, p)| i as f64 * p)));
    let legend: Obj = criteria.iter().enumerate().map(|(i, c)| (i.to_string(), c.clone())).collect();
    let probabilities: Obj = probs.iter().enumerate().map(|(i, p)| (i.to_string(), Json::Float(*p))).collect();
    Json::Obj(
        Obj::new()
            .with("type", Json::str("score"))
            .with("score", Json::Float(expected))
            .with("confidence", Json::Float(probs[best]))
            .with("legend", Json::Obj(legend))
            .with("probabilities", Json::Obj(probabilities)),
    )
}

/// `max(range(n), key=probs.__getitem__)`: the first index of the maximum.
fn argmax(probs: &[f64]) -> usize {
    let mut best = 0;
    for (i, p) in probs.iter().enumerate().skip(1) {
        if *p > probs[best] {
            best = i;
        }
    }
    best
}

// --- Distributions ---------------------------------------------------------------

fn categorical(rng: &mut PyRandom, n: usize, spread: Spread, mut winner: Option<usize>) -> Vec<f64> {
    if n == 1 {
        return vec![1.0];
    }
    if winner.is_none() && spread == Spread::Peaked {
        winner = Some(rng.randrange(n));
    }
    if let Some(winner) = winner {
        let top = rng.uniform(0.82, 0.97);
        let mut probs: Vec<f64> = dirichlet(rng, n - 1, 1.0).into_iter().map(|p| p * (1.0 - top)).collect();
        probs.insert(winner, top);
        return round_all(probs);
    }
    round_all(dirichlet(rng, n, if spread == Spread::Flat { 25.0 } else { 0.8 }))
}

/// Probabilities that peak at one level and fall off with distance, like a real rubric.
fn unimodal(rng: &mut PyRandom, n: usize, spread: Spread, winner: Option<usize>) -> Vec<f64> {
    if n == 1 {
        return vec![1.0];
    }
    let centre = winner.unwrap_or_else(|| rng.randrange(n));
    let sigma = if spread == Spread::Peaked || winner.is_some() { rng.uniform(0.25, 0.45) } else { rng.uniform(0.4, 1.4) };
    let weights: Vec<f64> = (0..n)
        .map(|i| {
            let d = i as i64 - centre as i64;
            (-((d * d) as f64) / (2.0 * sigma.powf(2.0))).exp() * rng.uniform(0.85, 1.15)
        })
        .collect();
    let total = py_sum(weights.iter().copied());
    round_all(weights.into_iter().map(|w| w / total).collect())
}

fn dirichlet(rng: &mut PyRandom, n: usize, alpha: f64) -> Vec<f64> {
    let draws: Vec<f64> = (0..n).map(|_| rng.gammavariate(alpha, 1.0)).collect();
    let total = py_sum(draws.iter().copied());
    let total = if total == 0.0 { 1.0 } else { total };
    draws.into_iter().map(|d| d / total).collect()
}

fn round_all(probs: Vec<f64>) -> Vec<f64> {
    probs.into_iter().map(py_round4).collect()
}
