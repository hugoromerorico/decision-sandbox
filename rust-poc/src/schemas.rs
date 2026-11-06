//! Request validation matching src/sandbox/schemas.py, error for error.
//!
//! FastAPI reports pydantic's errors verbatim, so the type, loc, msg, input and ctx of
//! each error, and their order, follow pydantic-core's rules for these models.

use crate::config::*;
use crate::json::{py_str, Json, Obj, PyStr};

#[derive(Clone)]
pub enum LocPart {
    Key(String),
    Index(usize),
}

impl From<&str> for LocPart {
    fn from(s: &str) -> Self {
        LocPart::Key(s.to_string())
    }
}

pub type Loc = Vec<LocPart>;

fn loc_with(loc: &Loc, part: impl Into<LocPart>) -> Loc {
    let mut out = loc.clone();
    out.push(part.into());
    out
}

pub struct ValidationError {
    pub kind: &'static str,
    pub loc: Loc,
    pub msg: String,
    pub input: Json,
    pub ctx: Option<Obj>,
}

impl ValidationError {
    /// The error as FastAPI serialises it.
    pub fn to_json(&self) -> Json {
        let loc = self
            .loc
            .iter()
            .map(|p| match p {
                LocPart::Key(k) => Json::str(k.clone()),
                LocPart::Index(i) => Json::int(*i as i64),
            })
            .collect();
        let mut out = Obj::new()
            .with("type", Json::str(self.kind))
            .with("loc", Json::Arr(loc))
            .with("msg", Json::str(self.msg.clone()))
            .with("input", self.input.clone());
        if let Some(ctx) = &self.ctx {
            out = out.with("ctx", Json::Obj(ctx.clone()));
        }
        Json::Obj(out)
    }
}

pub enum Question {
    Noul { instructions: Json, criteria: Option<(Json, Json)> },
    Choice { instructions: Json, criteria: Obj },
    Score { instructions: Json, criteria: Vec<Json> },
}

pub struct SystemOneRequest {
    pub state: Json,
    pub questions: Vec<(PyStr, Question)>,
}

/// The body as FastAPI hands it to pydantic: parsed JSON, raw text, or nothing.
pub enum Body {
    Json(Json),
    Text(PyStr),
    None,
}

#[derive(Default)]
pub struct Validator {
    pub errors: Vec<ValidationError>,
}

impl Validator {
    fn add(&mut self, kind: &'static str, loc: Loc, msg: impl Into<String>, input: &Json, ctx: Option<Obj>) {
        self.errors.push(ValidationError { kind, loc, msg: msg.into(), input: input.clone(), ctx });
    }

    fn missing(&mut self, loc: Loc, input: &Json) {
        self.add("missing", loc, "Field required", input, None);
    }

    fn model_attributes(&mut self, loc: Loc, input: &Json) {
        self.add("model_attributes_type", loc, "Input should be a valid dictionary or object to extract fields from", input, None);
    }

    /// `str | dict[str, Any] | list[Any]`, optionally nullable.
    fn free_form(&mut self, value: &Json, loc: &Loc, nullable: bool) -> bool {
        match value {
            Json::Str(_) | Json::Obj(_) | Json::Arr(_) => true,
            Json::Null if nullable => true,
            _ => {
                self.add("string_type", loc_with(loc, "str"), "Input should be a valid string", value, None);
                self.add("dict_type", loc_with(loc, "dict[str,any]"), "Input should be a valid dictionary", value, None);
                self.add("list_type", loc_with(loc, "list[any]"), "Input should be a valid list", value, None);
                false
            }
        }
    }

    fn key_length(&mut self, key: &PyStr, loc: &Loc, max: usize) {
        if key.len() > max {
            let ctx = Obj::new().with("max_length", Json::int(max as i64));
            let loc = loc_with(&loc_with(loc, LocPart::Key(key.s.clone())), "[key]");
            self.add("string_too_long", loc, format!("String should have at most {max} characters"), &Json::Str(key.clone()), Some(ctx));
        }
    }

    /// Dict length is checked only when every item validated.
    fn dict_length(&mut self, value: &Obj, loc: &Loc, max: usize) {
        let n = value.len();
        let input = Json::Obj(value.clone());
        if n < 1 {
            let ctx = Obj::new().with("field_type", Json::str("Dictionary")).with("min_length", Json::int(1)).with("actual_length", Json::int(n as i64));
            self.add("too_short", loc.clone(), format!("Dictionary should have at least 1 item after validation, not {n}"), &input, Some(ctx));
        } else if n > max {
            let ctx = Obj::new().with("field_type", Json::str("Dictionary")).with("max_length", Json::int(max as i64)).with("actual_length", Json::int(n as i64));
            self.add("too_long", loc.clone(), format!("Dictionary should have at most {max} items after validation, not {n}"), &input, Some(ctx));
        }
    }

    fn dict<'a>(&mut self, value: &'a Json, loc: &Loc) -> Option<&'a Obj> {
        match value {
            Json::Obj(o) => Some(o),
            _ => {
                self.add("dict_type", loc.clone(), "Input should be a valid dictionary", value, None);
                None
            }
        }
    }

    pub fn question(&mut self, value: &Json, loc: &Loc) -> Option<Question> {
        let Json::Obj(obj) = value else {
            self.model_attributes(loc.clone(), value);
            return None;
        };
        let Some(tag) = obj.get("type") else {
            let ctx = Obj::new().with("discriminator", Json::str("'type'"));
            self.add("union_tag_not_found", loc.clone(), "Unable to extract tag using discriminator 'type'", value, Some(ctx));
            return None;
        };
        match tag.as_str().map(|s| s.s.as_str()) {
            Some("noul") => return self.noul(obj, &loc_with(loc, "noul")),
            Some("choice") => return self.choice(obj, &loc_with(loc, "choice")),
            Some("score") => return self.score(obj, &loc_with(loc, "score")),
            _ => {}
        }
        let shown = py_str(tag);
        let expected = "'noul', 'choice', 'score'";
        let ctx = Obj::new()
            .with("discriminator", Json::str("'type'"))
            .with("tag", Json::str(shown.clone()))
            .with("expected_tags", Json::str(expected));
        let msg = format!("Input tag '{shown}' found using 'type' does not match any of the expected tags: {expected}");
        self.add("union_tag_invalid", loc.clone(), msg, value, Some(ctx));
        None
    }

    fn instructions(&mut self, obj: &Obj, loc: &Loc) -> (Json, bool) {
        let instructions = obj.get("instructions").cloned().unwrap_or(Json::Null);
        let ok = self.free_form(&instructions, &loc_with(loc, "instructions"), true);
        (instructions, ok)
    }

    fn noul(&mut self, obj: &Obj, loc: &Loc) -> Option<Question> {
        let (instructions, ok) = self.instructions(obj, loc);
        let mut criteria = None;
        let mut criteria_ok = true;
        match obj.get("criteria").unwrap_or(&Json::Null) {
            Json::Null => {}
            Json::Obj(raw) => {
                let t = raw.get("true").cloned().unwrap_or(Json::Null);
                let f = raw.get("false").cloned().unwrap_or(Json::Null);
                let criteria_loc = loc_with(loc, "criteria");
                let t_ok = self.free_form(&t, &loc_with(&criteria_loc, "true"), true);
                let f_ok = self.free_form(&f, &loc_with(&criteria_loc, "false"), true);
                criteria_ok = t_ok && f_ok;
                criteria = Some((t, f));
            }
            other => {
                self.model_attributes(loc_with(loc, "criteria"), other);
                criteria_ok = false;
            }
        }
        (ok && criteria_ok).then_some(Question::Noul { instructions, criteria })
    }

    fn choice(&mut self, obj: &Obj, loc: &Loc) -> Option<Question> {
        let (instructions, ok) = self.instructions(obj, loc);
        let criteria_loc = loc_with(loc, "criteria");
        let Some(raw) = obj.get("criteria") else {
            self.missing(criteria_loc, &Json::Obj(obj.clone()));
            return None;
        };
        let raw = self.dict(raw, &criteria_loc)?;
        let before = self.errors.len();
        for (name, description) in raw.iter() {
            self.key_length(name, &criteria_loc, MAX_CHOICE_NAME_LENGTH);
            self.free_form(description, &loc_with(&criteria_loc, LocPart::Key(name.s.clone())), true);
        }
        if self.errors.len() > before {
            return None;
        }
        self.dict_length(raw, &criteria_loc, MAX_CHOICES);
        (ok && self.errors.len() == before).then(|| Question::Choice { instructions, criteria: raw.clone() })
    }

    fn score(&mut self, obj: &Obj, loc: &Loc) -> Option<Question> {
        let (instructions, ok) = self.instructions(obj, loc);
        let criteria_loc = loc_with(loc, "criteria");
        let Some(raw) = obj.get("criteria") else {
            self.missing(criteria_loc, &Json::Obj(obj.clone()));
            return None;
        };
        let Json::Arr(levels) = raw else {
            self.add("list_type", criteria_loc, "Input should be a valid list", raw, None);
            return None;
        };
        let n = levels.len();
        if n > MAX_SCORE_LEVELS {
            // A list longer than its maximum reports only that, before validating items.
            let ctx = Obj::new().with("field_type", Json::str("List")).with("max_length", Json::int(MAX_SCORE_LEVELS as i64)).with("actual_length", Json::int(n as i64));
            self.add("too_long", criteria_loc, format!("List should have at most {MAX_SCORE_LEVELS} items after validation, not {n}"), raw, Some(ctx));
            return None;
        }
        let before = self.errors.len();
        for (i, level) in levels.iter().enumerate() {
            self.free_form(level, &loc_with(&criteria_loc, LocPart::Index(i)), false);
        }
        if self.errors.len() > before {
            return None;
        }
        if n < 1 {
            let ctx = Obj::new().with("field_type", Json::str("List")).with("min_length", Json::int(1)).with("actual_length", Json::int(0));
            self.add("too_short", criteria_loc, "List should have at least 1 item after validation, not 0", raw, Some(ctx));
            return None;
        }
        ok.then(|| Question::Score { instructions, criteria: levels.clone() })
    }

    pub fn request(&mut self, body: &Body) -> Option<SystemOneRequest> {
        let body_loc: Loc = vec!["body".into()];
        let data = match body {
            Body::None | Body::Json(Json::Null) => {
                self.missing(body_loc, &Json::Null);
                return None;
            }
            Body::Text(text) => {
                self.model_attributes(body_loc, &Json::Str(text.clone()));
                return None;
            }
            Body::Json(Json::Obj(data)) => data,
            Body::Json(other) => {
                self.model_attributes(body_loc, other);
                return None;
            }
        };
        let input = Json::Obj(data.clone());

        let state = match data.get("state") {
            None => {
                self.missing(loc_with(&body_loc, "state"), &input);
                Json::Null
            }
            Some(state) => {
                self.free_form(state, &loc_with(&body_loc, "state"), false);
                state.clone()
            }
        };

        match data.get("model") {
            None => self.missing(loc_with(&body_loc, "model"), &input),
            Some(Json::Str(_)) => {}
            Some(other) => self.add("string_type", loc_with(&body_loc, "model"), "Input should be a valid string", other, None),
        }

        let mut questions = Vec::new();
        let loc = loc_with(&body_loc, "questions");
        match data.get("questions") {
            None => self.missing(loc, &input),
            Some(raw) => {
                if let Some(raw) = self.dict(raw, &loc) {
                    let before = self.errors.len();
                    for (name, value) in raw.iter() {
                        self.key_length(name, &loc, MAX_QUESTION_NAME_LENGTH);
                        if let Some(q) = self.question(value, &loc_with(&loc, LocPart::Key(name.s.clone()))) {
                            questions.push((name.clone(), q));
                        }
                    }
                    if self.errors.len() == before {
                        self.dict_length(raw, &loc, MAX_QUESTIONS);
                    }
                }
            }
        }

        self.errors.is_empty().then_some(SystemOneRequest { state, questions })
    }
}

/// Characters Rust's `str::trim` removes, exactly what pydantic-core strips.
pub fn int_param(raw: &str, loc: Loc, max: i64, errors: &mut Vec<ValidationError>) -> Option<i64> {
    let input = Json::str(raw);
    let text = raw.trim();
    let (int_part, frac) = match text.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (text, None),
    };
    let (negative, digits) = match int_part.as_bytes().first() {
        Some(b'-') => (true, &int_part[1..]),
        Some(b'+') => (false, &int_part[1..]),
        _ => (false, int_part),
    };
    let valid = !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit() || c == '_')
        && !digits.starts_with('_')
        && !digits.ends_with('_')
        && !digits.contains("__")
        && frac.is_none_or(|f| !f.is_empty() && f.chars().all(|c| c == '0'));
    if !valid {
        errors.push(ValidationError {
            kind: "int_parsing",
            loc,
            msg: "Input should be a valid integer, unable to parse string as an integer".into(),
            input,
            ctx: None,
        });
        return None;
    }
    let magnitude = digits.chars().filter(|c| *c != '_').fold(0i64, |v, c| v.saturating_mul(10).saturating_add(c as i64 - '0' as i64));
    let value = if negative { -magnitude } else { magnitude };
    if value < 0 {
        let ctx = Obj::new().with("ge", Json::int(0));
        errors.push(ValidationError { kind: "greater_than_equal", loc, msg: "Input should be greater than or equal to 0".into(), input, ctx: Some(ctx) });
        return None;
    }
    if value > max {
        let ctx = Obj::new().with("le", Json::int(max));
        errors.push(ValidationError { kind: "less_than_equal", loc, msg: format!("Input should be less than or equal to {max}"), input, ctx: Some(ctx) });
        return None;
    }
    Some(value)
}
