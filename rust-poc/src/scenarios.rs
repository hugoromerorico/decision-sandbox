//! Named behaviours that let developers deliberately exercise application branches.

use crate::json::py_strip;

#[derive(Clone, Copy, PartialEq)]
pub enum Spread {
    Natural,
    Peaked,
    Flat,
}

#[derive(Clone)]
pub enum ChoicePick {
    First,
    Last,
    Name(String),
}

#[derive(Clone)]
pub enum ScorePick {
    Min,
    Max,
    /// A level index, saturated to the i64 range (it is clamped to the rubric anyway).
    Level(i64),
}

#[derive(Clone)]
pub struct Scenario {
    pub name: String,
    pub description: &'static str,
    pub spread: Spread,
    pub noul_range: Option<(f64, f64)>,
    pub choice_pick: Option<ChoicePick>,
    pub score_pick: Option<ScorePick>,
    /// Replaces the successful response entirely.
    pub error: Option<&'static str>,
}

impl Scenario {
    fn new(name: &str, description: &'static str) -> Self {
        Scenario {
            name: name.to_string(),
            description,
            spread: Spread::Natural,
            noul_range: None,
            choice_pick: None,
            score_pick: None,
            error: None,
        }
    }

    fn spread(mut self, spread: Spread) -> Self {
        self.spread = spread;
        self
    }

    fn noul(mut self, low: f64, high: f64) -> Self {
        self.noul_range = Some((low, high));
        self
    }

    fn choice(mut self, pick: ChoicePick) -> Self {
        self.choice_pick = Some(pick);
        self
    }

    fn score(mut self, pick: ScorePick) -> Self {
        self.score_pick = Some(pick);
        self
    }

    fn error(mut self, error: &'static str) -> Self {
        self.error = Some(error);
        self
    }
}

fn scenarios() -> Vec<Scenario> {
    use Spread::*;
    vec![
        Scenario::new("random", "Varied synthetic answers (default)."),
        Scenario::new("high-confidence", "Decisive answers: noul near 0 or 1, one dominant choice or score level.").spread(Peaked),
        Scenario::new("low-confidence", "Uncertain answers: noul near 0.5, near-uniform probabilities.").spread(Flat),
        Scenario::new("always-true", "Every noul answer is close to 1.").noul(0.95, 0.995),
        Scenario::new("always-false", "Every noul answer is close to 0.").noul(0.005, 0.05),
        Scenario::new("first-choice", "Choice questions select their first option.").choice(ChoicePick::First),
        Scenario::new("last-choice", "Choice questions select their last option.").choice(ChoicePick::Last),
        Scenario::new("min-score", "Score questions favour the lowest level.").score(ScorePick::Min),
        Scenario::new("max-score", "Score questions favour the highest level.").score(ScorePick::Max),
        Scenario::new("invalid-response", "HTTP 200 with a body that violates the response schema.").error("invalid-response"),
        Scenario::new("validation-error", "HTTP 422 with a validation error body.").error("validation-error"),
        Scenario::new("unauthorized", "HTTP 401, as if the API key were missing or invalid.").error("unauthorized"),
        Scenario::new("rate-limit", "HTTP 429 with a Retry-After header.").error("rate-limit"),
        Scenario::new("server-error", "HTTP 500.").error("server-error"),
        Scenario::new("unavailable", "HTTP 503 with a Retry-After header.").error("unavailable"),
        Scenario::new("timeout", "Waits 10 seconds, then returns HTTP 504.").error("timeout"),
    ]
}

const CHOICE_DESCRIPTION: &str = "Choice questions that offer <name> select it; others stay random.";
const SCORE_DESCRIPTION: &str = "Score questions favour level <level> (clamped to the rubric).";
const PARAMETERISED: [(&str, &str); 2] = [("choice:<name>", CHOICE_DESCRIPTION), ("score:<level>", SCORE_DESCRIPTION)];

pub struct UnknownScenario(pub String);

/// Python's `int(str)`: whitespace, a sign, ASCII digits with single underscores.
fn py_int(text: &str) -> Option<i64> {
    let text = py_strip(text);
    let (negative, digits) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let mut value: i64 = 0;
    for c in digits.chars() {
        match c {
            '_' => continue,
            '0'..='9' => value = value.saturating_mul(10).saturating_add(c as i64 - '0' as i64),
            _ => return None,
        }
    }
    Some(if negative { -value } else { value })
}

pub fn parse_scenario(raw: Option<&str>) -> Result<Scenario, UnknownScenario> {
    let all = scenarios();
    let raw = match raw {
        None | Some("") => return Ok(all.into_iter().next().expect("random is first")),
        Some(raw) => raw,
    };
    let value = py_strip(raw);
    if let Some(known) = all.into_iter().find(|s| s.name == value) {
        return Ok(known);
    }
    if let Some((kind, arg)) = value.split_once(':') {
        if !arg.is_empty() {
            if kind == "choice" {
                return Ok(Scenario::new(value, CHOICE_DESCRIPTION).choice(ChoicePick::Name(arg.to_string())));
            }
            if kind == "score" {
                if let Some(level) = py_int(arg) {
                    return Ok(Scenario::new(value, SCORE_DESCRIPTION).score(ScorePick::Level(level)));
                }
            }
        }
    }
    Err(UnknownScenario(value.to_string()))
}

pub fn catalogue() -> Vec<(String, &'static str)> {
    scenarios()
        .into_iter()
        .map(|s| (s.name, s.description))
        .chain(PARAMETERISED.iter().map(|(n, d)| (n.to_string(), *d)))
        .collect()
}
