//! JSON with Python semantics.
//!
//! The Python Worker parses bodies with CPython's `json` module and hashes them with
//! `json.dumps(sort_keys=True)`, so seeded answers depend on key order, ints vs floats
//! (`1` vs `1.0`), big ints, NaN/Infinity literals and Python's error messages. This
//! module reproduces them instead of relying on serde_json.

use std::collections::BTreeMap;

/// Deeper nesting raises RecursionError in Python (a 400). The exact depth differs by runtime.
pub const MAX_DEPTH: usize = 512;
/// CPython's default int_max_str_digits.
const MAX_INT_DIGITS: usize = 4300;

/// A Python `str`. Rust strings cannot hold lone surrogates (`"\ud800"` is valid JSON),
/// so they are replaced with U+FFFD and flagged: encoding such a string fails in Python.
#[derive(Clone, Debug, PartialEq)]
pub struct PyStr {
    pub s: String,
    pub lone_surrogate: bool,
}

impl PyStr {
    pub fn new(s: impl Into<String>) -> Self {
        PyStr { s: s.into(), lone_surrogate: false }
    }

    /// `len(str)`: code points.
    pub fn len(&self) -> usize {
        self.s.chars().count()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// A Python int, as its decimal representation.
    Int(String),
    Float(f64),
    Str(PyStr),
    Arr(Vec<Json>),
    Obj(Obj),
}

impl Json {
    pub fn str(s: impl Into<String>) -> Json {
        Json::Str(PyStr::new(s))
    }

    pub fn int(n: i64) -> Json {
        Json::Int(n.to_string())
    }

    pub fn as_str(&self) -> Option<&PyStr> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

}

/// A dict in insertion order. A repeated key keeps its first position and takes the last value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Obj {
    entries: Vec<(PyStr, Json)>,
    index: BTreeMap<String, usize>,
}

impl Obj {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: PyStr, value: Json) {
        match self.index.get(&key.s) {
            Some(&i) => self.entries[i].1 = value,
            None => {
                self.index.insert(key.s.clone(), self.entries.len());
                self.entries.push((key, value));
            }
        }
    }

    pub fn with(mut self, key: &str, value: Json) -> Self {
        self.insert(PyStr::new(key), value);
        self
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        self.index.get(key).map(|&i| &self.entries[i].1)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&PyStr, &Json)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }
}

impl FromIterator<(String, Json)> for Obj {
    fn from_iter<I: IntoIterator<Item = (String, Json)>>(iter: I) -> Self {
        let mut obj = Obj::new();
        for (k, v) in iter {
            obj.insert(PyStr::new(k), v);
        }
        obj
    }
}

// --- Decoding ------------------------------------------------------------------

pub enum DecodeError {
    /// `json.JSONDecodeError`: FastAPI answers 422 `json_invalid`. Position in code points.
    Json { msg: &'static str, pos: usize },
    /// Any other failure (bad UTF-8, too deep, huge ints): FastAPI answers 400.
    Body,
}

/// Decodes request bytes the way Starlette's `request.json()` does.
pub fn loads_bytes(bytes: &[u8]) -> Result<Json, DecodeError> {
    let text = std::str::from_utf8(bytes).map_err(|_| DecodeError::Body)?;
    // json.loads(bytes) decodes with utf-8-sig, so one leading BOM is dropped.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let chars: Vec<char> = text.chars().collect();
    Scanner { s: &chars, depth: 0 }.decode()
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

struct Scanner<'a> {
    s: &'a [char],
    depth: usize,
}

type Scan<T> = Result<(T, usize), DecodeError>;

impl Scanner<'_> {
    fn err<T>(&self, msg: &'static str, pos: usize) -> Result<T, DecodeError> {
        Err(DecodeError::Json { msg, pos })
    }

    fn at(&self, i: usize) -> Option<char> {
        self.s.get(i).copied()
    }

    fn skip_ws(&self, mut i: usize) -> usize {
        while i < self.s.len() && is_ws(self.s[i]) {
            i += 1;
        }
        i
    }

    fn starts_with(&self, i: usize, word: &str) -> bool {
        let mut j = i;
        for c in word.chars() {
            if self.at(j) != Some(c) {
                return false;
            }
            j += 1;
        }
        true
    }

    fn decode(&mut self) -> Result<Json, DecodeError> {
        let idx = self.skip_ws(0);
        let (value, end) = self.scan_once(idx)?;
        let end = self.skip_ws(end);
        if end != self.s.len() {
            return self.err("Extra data", end);
        }
        Ok(value)
    }

    fn scan_once(&mut self, idx: usize) -> Scan<Json> {
        let Some(c) = self.at(idx) else {
            return self.err("Expecting value", idx);
        };
        match c {
            '"' => {
                let (s, end) = self.scan_string(idx + 1)?;
                return Ok((Json::Str(s), end));
            }
            '{' | '[' => {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err(DecodeError::Body);
                }
                let result = if c == '{' {
                    self.parse_object(idx + 1).map(|(o, e)| (Json::Obj(o), e))
                } else {
                    self.parse_array(idx + 1).map(|(a, e)| (Json::Arr(a), e))
                };
                self.depth -= 1;
                return result;
            }
            'n' if self.starts_with(idx, "null") => return Ok((Json::Null, idx + 4)),
            't' if self.starts_with(idx, "true") => return Ok((Json::Bool(true), idx + 4)),
            'f' if self.starts_with(idx, "false") => return Ok((Json::Bool(false), idx + 5)),
            'N' if self.starts_with(idx, "NaN") => return Ok((Json::Float(f64::NAN), idx + 3)),
            'I' if self.starts_with(idx, "Infinity") => return Ok((Json::Float(f64::INFINITY), idx + 8)),
            '-' if self.starts_with(idx, "-Infinity") => return Ok((Json::Float(f64::NEG_INFINITY), idx + 9)),
            _ => {}
        }
        self.match_number(idx)
    }

    fn parse_object(&mut self, start: usize) -> Scan<Obj> {
        let mut out = Obj::new();
        let mut idx = self.skip_ws(start);
        if self.at(idx) != Some('}') {
            loop {
                if self.at(idx) != Some('"') {
                    return self.err("Expecting property name enclosed in double quotes", idx);
                }
                let (key, after_key) = self.scan_string(idx + 1)?;
                idx = self.skip_ws(after_key);
                if self.at(idx) != Some(':') {
                    return self.err("Expecting ':' delimiter", idx);
                }
                idx = self.skip_ws(idx + 1);
                let (value, after_value) = self.scan_once(idx)?;
                out.insert(key, value);
                idx = self.skip_ws(after_value);
                if self.at(idx) == Some('}') {
                    break;
                }
                if self.at(idx) != Some(',') {
                    return self.err("Expecting ',' delimiter", idx);
                }
                let comma = idx;
                idx = self.skip_ws(idx + 1);
                if self.at(idx) == Some('}') {
                    return self.err("Illegal trailing comma before end of object", comma);
                }
            }
        }
        Ok((out, idx + 1))
    }

    fn parse_array(&mut self, start: usize) -> Scan<Vec<Json>> {
        let mut out = Vec::new();
        let mut idx = self.skip_ws(start);
        if self.at(idx) != Some(']') {
            loop {
                let (value, after_value) = self.scan_once(idx)?;
                out.push(value);
                idx = self.skip_ws(after_value);
                if self.at(idx) == Some(']') {
                    break;
                }
                if self.at(idx) != Some(',') {
                    return self.err("Expecting ',' delimiter", idx);
                }
                let comma = idx;
                idx = self.skip_ws(idx + 1);
                if self.at(idx) == Some(']') {
                    return self.err("Illegal trailing comma before end of array", comma);
                }
            }
        }
        Ok((out, idx + 1))
    }

    fn is_digit(&self, i: usize) -> bool {
        self.at(i).is_some_and(|c| c.is_ascii_digit())
    }

    fn match_number(&self, start: usize) -> Scan<Json> {
        let len = self.s.len();
        let mut idx = start;
        if self.at(idx) == Some('-') {
            idx += 1;
            if idx >= len {
                return self.err("Expecting value", start);
            }
        }
        match self.at(idx) {
            Some('1'..='9') => {
                idx += 1;
                while self.is_digit(idx) {
                    idx += 1;
                }
            }
            Some('0') => idx += 1,
            _ => return self.err("Expecting value", start),
        }
        let mut is_float = false;
        if idx + 1 < len && self.s[idx] == '.' && self.is_digit(idx + 1) {
            is_float = true;
            idx += 2;
            while self.is_digit(idx) {
                idx += 1;
            }
        }
        if idx + 1 < len && matches!(self.s[idx], 'e' | 'E') {
            let e_start = idx;
            idx += 1;
            if idx + 1 < len && matches!(self.s[idx], '-' | '+') {
                idx += 1;
            }
            while self.is_digit(idx) {
                idx += 1;
            }
            if self.is_digit(idx - 1) {
                is_float = true;
            } else {
                idx = e_start;
            }
        }
        let text: String = self.s[start..idx].iter().collect();
        if is_float {
            return Ok((Json::Float(text.parse().map_err(|_| DecodeError::Body)?), idx));
        }
        let digits = text.trim_start_matches('-');
        if digits.len() > MAX_INT_DIGITS {
            return Err(DecodeError::Body);
        }
        let normalized = if digits == "0" { "0".to_string() } else { text };
        Ok((Json::Int(normalized), idx))
    }

    /// CPython's scanstring_unicode; `end` is the index after the opening quote.
    fn scan_string(&self, mut end: usize) -> Scan<PyStr> {
        let s = self.s;
        let len = s.len();
        let begin = end - 1;
        let mut out = String::new();
        let mut lone = false;
        loop {
            let mut next = end;
            let mut c = '\0';
            while next < len {
                c = s[next];
                if c == '"' || c == '\\' {
                    break;
                }
                if (c as u32) <= 0x1f {
                    return self.err("Invalid control character at", next);
                }
                next += 1;
            }
            if next >= len {
                return self.err("Unterminated string starting at", begin);
            }
            out.extend(&s[end..next]);
            next += 1;
            if c == '"' {
                return Ok((PyStr { s: out, lone_surrogate: lone }, next));
            }
            if next == len {
                return self.err("Unterminated string starting at", begin);
            }
            let esc = s[next];
            if esc != 'u' {
                end = next + 1;
                let ch = match esc {
                    '"' => '"',
                    '\\' => '\\',
                    '/' => '/',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    _ => return self.err("Invalid \\escape", end - 2),
                };
                out.push(ch);
                continue;
            }
            next += 1;
            end = next + 4;
            if end >= len {
                return self.err("Invalid \\uXXXX escape", next - 1);
            }
            let mut code = self.hex4(next, end)?;
            next = end;
            if (0xd800..=0xdbff).contains(&code) && end + 6 < len && s[next] == '\\' && s[next + 1] == 'u' {
                let low = self.hex4(next + 2, end + 6)?;
                if (0xdc00..=0xdfff).contains(&low) {
                    code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
                    end += 6;
                }
            }
            match char::from_u32(code) {
                Some(ch) => out.push(ch),
                None => {
                    out.push('\u{fffd}');
                    lone = true;
                }
            }
        }
    }

    fn hex4(&self, from: usize, end: usize) -> Result<u32, DecodeError> {
        let mut code = 0;
        for i in from..end {
            match self.s[i].to_digit(16) {
                Some(d) => code = code * 16 + d,
                None => return self.err("Invalid \\uXXXX escape", end - 5),
            }
        }
        Ok(code)
    }
}

// --- Encoding ------------------------------------------------------------------

/// A value Python cannot encode: a lone surrogate, or NaN where NaN is not allowed.
#[derive(Debug)]
pub struct EncodeError;

/// How floats are written:
/// - `Canonical`: `json.dumps` (Python repr, NaN/Infinity literals), used for seed hashing
/// - `Python`:    Starlette's JSONResponse (Python repr; NaN fails, so the request 500s)
/// - `Pydantic`:  pydantic-core's serializer used for response models (NaN/Infinity -> null)
#[derive(Clone, Copy, PartialEq)]
pub enum FloatStyle {
    Canonical,
    Python,
    Pydantic,
}

pub fn dumps(value: &Json, sort_keys: bool, floats: FloatStyle) -> Result<String, EncodeError> {
    let mut out = String::new();
    write(value, sort_keys, floats, &mut out)?;
    Ok(out)
}

fn write(value: &Json, sort_keys: bool, floats: FloatStyle, out: &mut String) -> Result<(), EncodeError> {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Int(digits) => out.push_str(digits),
        Json::Float(x) => out.push_str(&format_json_float(*x, floats)?),
        Json::Str(s) => quote(s, out)?,
        Json::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(item, sort_keys, floats, out)?;
            }
            out.push(']');
        }
        Json::Obj(obj) => {
            let mut entries: Vec<_> = obj.iter().collect();
            if sort_keys {
                // Byte order of UTF-8 is code point order, as Python sorts str.
                entries.sort_by(|a, b| a.0.s.cmp(&b.0.s));
            }
            out.push('{');
            for (i, (k, v)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                quote(k, out)?;
                out.push(':');
                write(v, sort_keys, floats, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn format_json_float(x: f64, style: FloatStyle) -> Result<String, EncodeError> {
    if x.is_finite() {
        return Ok(if style == FloatStyle::Pydantic { pydantic_float(x) } else { python_float_repr(x) });
    }
    match style {
        FloatStyle::Pydantic => Ok("null".into()),
        FloatStyle::Python => Err(EncodeError),
        FloatStyle::Canonical => Ok(if x.is_nan() {
            "NaN"
        } else if x > 0.0 {
            "Infinity"
        } else {
            "-Infinity"
        }
        .into()),
    }
}

/// JSON string escaping shared by json.dumps(ensure_ascii=False) and pydantic.
fn quote(s: &PyStr, out: &mut String) -> Result<(), EncodeError> {
    if s.lone_surrogate {
        return Err(EncodeError);
    }
    out.push('"');
    for c in s.s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    Ok(())
}

/// Shortest round-trip digits and the decimal exponent of the first digit.
///
/// When two shortest strings round-trip and are equally close (x = 758379700834392.25),
/// Python's repr picks the even last digit, while `{:e}` rounds up. So `{:e}` only
/// gives the length; `{:.Ne}` (exact, ties to even) gives the digits.
fn shortest_digits(x: f64) -> (String, i32) {
    let shortest = format!("{:e}", x.abs());
    let length = shortest.split_once('e').expect("{:e} has an exponent").0.replace('.', "").len();
    let formatted = format!("{:.*e}", length - 1, x.abs());
    let (mantissa, exp) = formatted.split_once('e').expect("{:e} has an exponent");
    (mantissa.replace('.', ""), exp.parse().expect("integer exponent"))
}

fn format_float(x: f64, low: i32, high: i32, pad_exponent: bool, plus_sign: bool) -> String {
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    let sign = if x < 0.0 { "-" } else { "" };
    let (digits, exp) = shortest_digits(x);
    if (low..high).contains(&exp) {
        if exp < 0 {
            return format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize));
        }
        let split = (exp + 1) as usize;
        let (int, frac) = if digits.len() > split { digits.split_at(split) } else { (digits.as_str(), "") };
        let int = format!("{int:0<split$}");
        return format!("{sign}{int}.{}", if frac.is_empty() { "0" } else { frac });
    }
    let mantissa = if digits.len() > 1 { format!("{}.{}", &digits[..1], &digits[1..]) } else { digits };
    let e = if pad_exponent { format!("{:02}", exp.abs()) } else { exp.abs().to_string() };
    let exp_sign = if exp < 0 { "-" } else if plus_sign { "+" } else { "" };
    format!("{sign}{mantissa}e{exp_sign}{e}")
}

/// `repr(float)`: fixed notation for 1e-4 <= |x| < 1e16, else `1e-05` / `1e+16`.
pub fn python_float_repr(x: f64) -> String {
    format_float(x, -4, 16, true, true)
}

/// pydantic-core as bundled in the Python Workers runtime: fixed notation for
/// 1e-5 <= |x| < 1e16, else `1e-6` / `1e16` (newer pydantic-core writes `1e+16`).
pub fn pydantic_float(x: f64) -> String {
    format_float(x, -5, 16, false, false)
}

// --- Python repr() -----------------------------------------------------------------

/// `str.isprintable()` for the characters that matter in practice: controls, format
/// characters, separators, surrogates, private use and noncharacters. Unassigned code
/// points (which Python also escapes) are treated as printable.
fn is_printable(c: char) -> bool {
    let cp = c as u32;
    if c == ' ' {
        return true;
    }
    !(cp < 0x20
        || (0x7f..=0xa0).contains(&cp)
        || cp == 0xad
        || (0x600..=0x605).contains(&cp)
        || cp == 0x61c
        || cp == 0x6dd
        || cp == 0x70f
        || cp == 0x8e2
        || cp == 0x1680
        || cp == 0x180e
        || (0x2000..=0x200f).contains(&cp)
        || (0x2028..=0x202f).contains(&cp)
        || (0x205f..=0x2064).contains(&cp)
        || (0x2066..=0x206f).contains(&cp)
        || cp == 0x3000
        || (0xd800..=0xf8ff).contains(&cp)
        || (0xfdd0..=0xfdef).contains(&cp)
        || cp == 0xfeff
        || (0xfff9..=0xfffb).contains(&cp)
        || (cp & 0xfffe) == 0xfffe
        || cp == 0x110bd
        || cp == 0x110cd
        || (0x13430..=0x1343f).contains(&cp)
        || (0x1bca0..=0x1bca3).contains(&cp)
        || (0x1d173..=0x1d17a).contains(&cp)
        || cp == 0xe0001
        || (0xe0020..=0xe007f).contains(&cp)
        || cp >= 0xf0000)
}

/// `repr(str)`, used in error messages.
pub fn py_repr_str(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::from(q);
    for c in s.chars() {
        let cp = c as u32;
        match c {
            c if c == q || c == '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if !is_printable(c) => out.push_str(&if cp <= 0xff {
                format!("\\x{cp:02x}")
            } else if cp <= 0xffff {
                format!("\\u{cp:04x}")
            } else {
                format!("\\U{cp:08x}")
            }),
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// `repr()` of a decoded JSON value: None/True/False, dicts as {'k': v}.
pub fn py_repr(value: &Json) -> String {
    match value {
        Json::Null => "None".into(),
        Json::Bool(true) => "True".into(),
        Json::Bool(false) => "False".into(),
        Json::Int(digits) => digits.clone(),
        Json::Float(x) if x.is_nan() => "nan".into(),
        Json::Float(x) if x.is_infinite() => if *x > 0.0 { "inf" } else { "-inf" }.into(),
        Json::Float(x) => python_float_repr(*x),
        Json::Str(s) => py_repr_str(&s.s),
        Json::Arr(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Json::Obj(obj) => format!(
            "{{{}}}",
            obj.iter().map(|(k, v)| format!("{}: {}", py_repr_str(&k.s), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `str()`: like repr, but a str is returned as is.
pub fn py_str(value: &Json) -> String {
    match value {
        Json::Str(s) => s.s.clone(),
        other => py_repr(other),
    }
}

// --- Python string helpers ---------------------------------------------------------

/// Characters `str.isspace()` treats as whitespace (and `str.strip()` removes).
fn is_py_space(c: char) -> bool {
    matches!(c as u32, 0x09..=0x0d | 0x1c..=0x20 | 0x85 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000)
}

pub fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}
