// SPDX-License-Identifier: MIT
//! The JSON this protocol uses: a strict reader and a writer with one
//! spelling.
//!
//! The crate reads JSON itself, for three reasons. A key given twice is
//! refused here, where a general reader keeps the last one. A message's
//! bytes are the same whatever features another crate in the build turns on
//! in a shared JSON library, so the golden frames hold. And a number is kept
//! as its digits: the protocol's own numbers are whole and at most 2^53 - 1,
//! and the smoke report passes through as it was written.
//!
//! [`parse`] takes any bytes and never panics.
use core::fmt;

/// Nesting deeper than this is refused.
pub const MAX_DEPTH: usize = 32;
/// The largest whole number a message may carry: 2^53 - 1, so every number
/// survives a reader that keeps numbers as doubles.
pub const MAX_NUMBER: u64 = (1 << 53) - 1;

/// A JSON value. An object keeps its keys in the order they were written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    /// The number as written, checked against JSON's grammar.
    Num(String),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

/// Why bytes are not JSON this crate takes, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError {
    /// The byte offset.
    pub at: usize,
    pub what: &'static str,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.what, self.at)
    }
}

impl std::error::Error for JsonError {}

impl Value {
    /// A whole number from 0 to [`MAX_NUMBER`], written as plain digits: no
    /// sign, fraction, exponent or leading zero.
    pub fn as_u64(&self) -> Option<u64> {
        let Value::Num(s) = self else { return None };
        let b = s.as_bytes();
        if b.is_empty()
            || b.len() > 16
            || !b.iter().all(u8::is_ascii_digit)
            || (b.len() > 1 && b[0] == b'0')
        {
            return None;
        }
        let n: u64 = s.parse().ok()?;
        (n <= MAX_NUMBER).then_some(n)
    }

    /// The value in this crate's one spelling: no spaces, keys in order.
    pub fn write(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            Value::Num(n) => out.push_str(n),
            Value::Str(s) => write_str(out, s),
            Value::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Value::Obj(o) => {
                out.push('{');
                for (i, (k, v)) in o.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_str(out, k);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    pub fn to_json(&self) -> String {
        let mut s = String::new();
        self.write(&mut s);
        s
    }
}

/// A string literal: `"` and `\` escaped, bytes below 0x20 as `\u00XX`,
/// everything else as it is.
pub fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                out.push_str("\\u00");
                out.push(HEX[(c as usize) >> 4] as char);
                out.push(HEX[(c as usize) & 15] as char);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// An object under construction, keys in the order they are added.
pub(crate) struct ObjWriter(String);

impl ObjWriter {
    pub fn new() -> Self {
        ObjWriter(String::from("{"))
    }
    fn key(&mut self, k: &str) {
        if self.0.len() > 1 {
            self.0.push(',');
        }
        write_str(&mut self.0, k);
        self.0.push(':');
    }
    pub fn str(&mut self, k: &str, v: &str) -> &mut Self {
        self.key(k);
        write_str(&mut self.0, v);
        self
    }
    pub fn num(&mut self, k: &str, v: u64) -> &mut Self {
        self.key(k);
        self.0.push_str(&v.to_string());
        self
    }
    pub fn bool(&mut self, k: &str, v: bool) -> &mut Self {
        self.key(k);
        self.0.push_str(if v { "true" } else { "false" });
        self
    }
    /// A value already written as JSON.
    pub fn raw(&mut self, k: &str, json: &str) -> &mut Self {
        self.key(k);
        self.0.push_str(json);
        self
    }
    pub fn end(&mut self) -> String {
        let mut s = core::mem::take(&mut self.0);
        s.push('}');
        s
    }
}

/// A list of values already written as JSON.
pub(crate) fn array<I: IntoIterator<Item = String>>(items: I) -> String {
    let mut s = String::from("[");
    for (i, v) in items.into_iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&v);
    }
    s.push(']');
    s
}

/// A string as a JSON literal.
pub(crate) fn quoted(s: &str) -> String {
    let mut out = String::new();
    write_str(&mut out, s);
    out
}

/// One JSON value and nothing after it but white space. UTF-8 only; a key
/// given twice in one object, nesting past [`MAX_DEPTH`], a lone surrogate
/// and a raw control character in a string are refused.
pub fn parse(bytes: &[u8]) -> Result<Value, JsonError> {
    let text = core::str::from_utf8(bytes).map_err(|e| JsonError {
        at: e.valid_up_to(),
        what: "not UTF-8",
    })?;
    let mut p = Parser {
        s: text.as_bytes(),
        text,
        at: 0,
    };
    p.space();
    let v = p.value(0)?;
    p.space();
    if p.at != p.s.len() {
        return Err(p.err("bytes after the value"));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    text: &'a str,
    at: usize,
}

impl Parser<'_> {
    fn err(&self, what: &'static str) -> JsonError {
        JsonError { at: self.at, what }
    }
    fn peek(&self) -> Option<u8> {
        self.s.get(self.at).copied()
    }
    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }
    fn word(&mut self, w: &str, v: Value) -> Result<Value, JsonError> {
        if self.s[self.at..].starts_with(w.as_bytes()) {
            self.at += w.len();
            Ok(v)
        } else {
            Err(self.err("not a JSON value"))
        }
    }
    fn value(&mut self, depth: usize) -> Result<Value, JsonError> {
        match self.peek() {
            None => Err(self.err("the value is missing")),
            Some(b'n') => self.word("null", Value::Null),
            Some(b't') => self.word("true", Value::Bool(true)),
            Some(b'f') => self.word("false", Value::Bool(false)),
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b'[') => {
                if depth >= MAX_DEPTH {
                    return Err(self.err("nested too deep"));
                }
                self.at += 1;
                let mut items = Vec::new();
                self.space();
                if self.peek() == Some(b']') {
                    self.at += 1;
                    return Ok(Value::Arr(items));
                }
                loop {
                    self.space();
                    items.push(self.value(depth + 1)?);
                    self.space();
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b']') => {
                            self.at += 1;
                            return Ok(Value::Arr(items));
                        }
                        _ => return Err(self.err("expected , or ]")),
                    }
                }
            }
            Some(b'{') => {
                if depth >= MAX_DEPTH {
                    return Err(self.err("nested too deep"));
                }
                self.at += 1;
                let mut items: Vec<(String, Value)> = Vec::new();
                self.space();
                if self.peek() == Some(b'}') {
                    self.at += 1;
                    return Ok(Value::Obj(items));
                }
                loop {
                    self.space();
                    if self.peek() != Some(b'"') {
                        return Err(self.err("expected a key"));
                    }
                    let at = self.at;
                    let key = self.string()?;
                    if items.iter().any(|(k, _)| *k == key) {
                        return Err(JsonError {
                            at,
                            what: "a key given twice",
                        });
                    }
                    self.space();
                    if self.peek() != Some(b':') {
                        return Err(self.err("expected :"));
                    }
                    self.at += 1;
                    self.space();
                    let v = self.value(depth + 1)?;
                    items.push((key, v));
                    self.space();
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {
                            self.at += 1;
                            return Ok(Value::Obj(items));
                        }
                        _ => return Err(self.err("expected , or }")),
                    }
                }
            }
            Some(_) => Err(self.err("not a JSON value")),
        }
    }
    fn digits(&mut self) -> usize {
        let from = self.at;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        self.at - from
    }
    fn number(&mut self) -> Result<Value, JsonError> {
        let from = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(self.err("not a number")),
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if self.digits() == 0 {
                return Err(self.err("not a number"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if self.digits() == 0 {
                return Err(self.err("not a number"));
            }
        }
        Ok(Value::Num(self.text[from..self.at].to_string()))
    }
    fn hex4(&mut self) -> Result<u32, JsonError> {
        let Some(h) = self.s.get(self.at..self.at + 4) else {
            return Err(self.err("a \\u escape cut short"));
        };
        let mut n = 0u32;
        for &c in h {
            let d = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => return Err(self.err("a \\u escape that is not hex")),
            };
            n = n * 16 + d as u32;
        }
        self.at += 4;
        Ok(n)
    }
    fn string(&mut self) -> Result<String, JsonError> {
        self.at += 1; // the opening quote
        let mut out = String::new();
        loop {
            let from = self.at;
            while !matches!(self.peek(), None | Some(b'"' | b'\\' | 0..=0x1f)) {
                self.at += 1;
            }
            // `from` and `at` sit before ASCII bytes or at the end, so both
            // are character boundaries of the text.
            out.push_str(&self.text[from..self.at]);
            match self.peek() {
                None => return Err(self.err("a string with no end")),
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    let c = self
                        .peek()
                        .ok_or_else(|| self.err("a string with no end"))?;
                    self.at += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = if (0xd800..0xdc00).contains(&hi) {
                                if self.s.get(self.at..self.at + 2) != Some(b"\\u") {
                                    return Err(self.err("a lone surrogate"));
                                }
                                self.at += 2;
                                let lo = self.hex4()?;
                                if !(0xdc00..0xe000).contains(&lo) {
                                    return Err(self.err("a lone surrogate"));
                                }
                                0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00)
                            } else {
                                hi
                            };
                            match char::from_u32(code) {
                                Some(ch) => out.push(ch),
                                None => return Err(self.err("a lone surrogate")),
                            }
                        }
                        _ => {
                            self.at -= 1;
                            return Err(self.err("an escape JSON does not have"));
                        }
                    }
                }
                Some(_) => return Err(self.err("a control character in a string")),
            }
        }
    }
}
