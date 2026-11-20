//! JSON functions: a small JSON parser and writer and the SQL functions
//! built on them (`json`, `json_extract`, `->`, `->>`, `json_array`,
//! `json_object`, `json_type`, `json_valid`, `json_array_length`,
//! `json_quote`, and the `json_group_*` aggregates).
//!
//! Output is minified, and numbers keep the text they were written with,
//! as in SQLite. A JSON value passed straight from one JSON function into
//! `json_array` / `json_object` nests as JSON instead of becoming a string
//! (SQLite marks such values with a "subtype"; here the binder wraps those
//! arguments in [`f_json_sub`], which tags the text with [`SUB`]).

use crate::error::{Error, Result};
use crate::value::Value;

/// Marks text that is already JSON (first character, never user-visible:
/// only the JSON builders receive tagged values).
pub const SUB: char = '\u{F8FF}';

#[derive(Clone, Debug, PartialEq)]
pub enum J {
    Null,
    True,
    False,
    /// A number, kept as written.
    Num(String),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

fn malformed() -> Error {
    Error::Runtime("malformed JSON".into())
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
    depth: usize,
    /// Set when the text used a JSON5 extension.
    json5: bool,
}

/// Internal parse result: the error is the byte offset of the problem.
type PR<T> = std::result::Result<T, usize>;

impl P<'_> {
    fn at(&self, k: usize) -> Option<u8> {
        self.b.get(self.i + k).copied()
    }

    /// Skip whitespace, including JSON5 whitespace and comments.
    fn ws(&mut self) -> PR<()> {
        while let Some(c) = self.at(0) {
            match c {
                b' ' | b'\t' | b'\n' | b'\r' => self.i += 1,
                0x0b | 0x0c => {
                    self.json5 = true;
                    self.i += 1;
                }
                b'/' if self.at(1) == Some(b'*') => {
                    self.json5 = true;
                    let end = self.b[self.i + 2..]
                        .windows(2)
                        .position(|w| w == b"*/")
                        .ok_or(self.b.len())?;
                    self.i += end + 4;
                }
                b'/' if self.at(1) == Some(b'/') => {
                    self.json5 = true;
                    while self.at(0).is_some_and(|c| c != b'\n') {
                        self.i += 1;
                    }
                }
                0xc2..=0xef => {
                    // Unicode spaces JSON5 allows: U+00A0, U+1680,
                    // U+2000..U+200A, U+2028, U+2029, U+202F, U+205F,
                    // U+3000, U+FEFF.
                    let rest = &self.b[self.i..];
                    let n = match rest {
                        [0xc2, 0xa0, ..] => 2,
                        [0xe1, 0x9a, 0x80, ..] => 3,
                        [0xe2, 0x80, 0x80..=0x8a | 0xa8 | 0xa9 | 0xaf, ..] => 3,
                        [0xe2, 0x81, 0x9f, ..] | [0xe3, 0x80, 0x80, ..] => 3,
                        [0xef, 0xbb, 0xbf, ..] => 3,
                        _ => return Ok(()),
                    };
                    self.json5 = true;
                    self.i += n;
                }
                _ => return Ok(()),
            }
        }
        Ok(())
    }

    fn value(&mut self) -> PR<J> {
        self.depth += 1;
        if self.depth > 1000 {
            return Err(self.i);
        }
        // Errors at the start of a value are reported where the value's
        // whitespace began, as SQLite does.
        let pre = self.i;
        self.ws()?;
        let c = self.at(0).ok_or(pre)?;
        let v = match c {
            b'{' => {
                self.i += 1;
                let mut obj = Vec::new();
                loop {
                    let kpre = self.i;
                    self.ws()?;
                    if self.at(0) == Some(b'}') {
                        if !obj.is_empty() {
                            self.json5 = true;
                        }
                        self.i += 1;
                        break;
                    }
                    let k = match self.at(0) {
                        None => return Err(self.b.len()),
                        Some(q @ (b'"' | b'\'')) => self.string(q)?,
                        Some(b'\\') if self.ident_char(true) == 0 => return Err(self.i + 1),
                        Some(_) if self.ident_char(true) > 0 => {
                            self.json5 = true;
                            let start = self.i;
                            loop {
                                let n = self.ident_char(false);
                                if n == 0 {
                                    break;
                                }
                                self.i += n;
                            }
                            String::from_utf8_lossy(&self.b[start..self.i]).into_owned()
                        }
                        // SQLite parses any other key as a value, then
                        // rejects it for not being a string.
                        Some(_) => {
                            self.value()?;
                            return Err(kpre);
                        }
                    };
                    self.ws()?;
                    if self.at(0) != Some(b':') {
                        return Err(self.i);
                    }
                    self.i += 1;
                    let v = self.value()?;
                    obj.push((k, v));
                    self.ws()?;
                    match self.at(0) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            break;
                        }
                        _ => return Err(self.i),
                    }
                }
                J::Obj(obj)
            }
            b'[' => {
                self.i += 1;
                let mut arr = Vec::new();
                loop {
                    let vpre = self.i;
                    self.ws()?;
                    if self.at(0) == Some(b']') {
                        if !arr.is_empty() {
                            self.json5 = true;
                        }
                        self.i += 1;
                        break;
                    }
                    self.i = vpre;
                    arr.push(self.value()?);
                    self.ws()?;
                    match self.at(0) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            break;
                        }
                        _ => return Err(self.i),
                    }
                }
                J::Arr(arr)
            }
            q @ (b'"' | b'\'') => J::Str(self.string(q)?),
            _ if self.word("true") => J::True,
            _ if self.word("false") => J::False,
            _ if self.word("null") => J::Null,
            _ => {
                if let Some(j) = self.nan_inf() {
                    j
                } else {
                    // A structural character where a value belongs is
                    // reported where the whitespace began; anything else
                    // at its own position.
                    if matches!(c, b']' | b'}' | b',' | b':') {
                        return Err(pre);
                    }
                    if !(c.is_ascii_alphanumeric() || matches!(c, b'-' | b'+' | b'.')) {
                        return Err(self.i);
                    }
                    self.number()?
                }
            }
        };
        self.depth -= 1;
        Ok(v)
    }

    /// Length of an identifier character at the cursor (`\uXXXX` counts
    /// as one), or 0.
    fn ident_char(&self, first: bool) -> usize {
        match self.at(0) {
            Some(c) if c == b'_' || c == b'$' || c.is_ascii_alphabetic() || c >= 0x80 => 1,
            Some(c) if !first && c.is_ascii_digit() => 1,
            Some(b'\\')
                if self.at(1) == Some(b'u')
                    && self
                        .b
                        .get(self.i + 2..self.i + 6)
                        .is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit)) =>
            {
                6
            }
            _ => 0,
        }
    }

    /// SQLite's JSON5 spellings of infinity and NaN, any letter case:
    /// `inf`, `infinity` (optionally signed), `nan`, `qnan`, `snan`.
    fn nan_inf(&mut self) -> Option<J> {
        let rest = &self.b[self.i..];
        let (sign, body) = match rest.first() {
            Some(b'-') => ("-", &rest[1..]),
            Some(b'+') => ("", &rest[1..]),
            _ => ("", rest),
        };
        let skip = rest.len() - body.len();
        let word_len = body
            .iter()
            .take_while(|c| c.is_ascii_alphanumeric())
            .count();
        let w = body[..word_len].to_ascii_lowercase();
        let j = match w.as_slice() {
            b"inf" | b"infinity" => J::Num(format!("{sign}9e999")),
            b"nan" | b"qnan" | b"snan" if skip == 0 => J::Null,
            _ => return None,
        };
        self.json5 = true;
        self.i += skip + word_len;
        Some(j)
    }

    fn word(&mut self, w: &str) -> bool {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            true
        } else {
            false
        }
    }

    fn digits(&mut self) -> usize {
        let s = self.i;
        while self.at(0).is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        self.i - s
    }

    /// A number, rewritten in canonical JSON when it uses JSON5 forms
    /// (`+1`, `.5`, `5.`, hexadecimal, `Infinity`), as SQLite renders it.
    fn number(&mut self) -> PR<J> {
        let start = self.i;
        let mut neg = false;
        match self.at(0) {
            Some(b'-') => {
                neg = true;
                self.i += 1;
            }
            Some(b'+') => {
                self.json5 = true;
                self.i += 1;
            }
            _ => {}
        }
        let sign = if neg { "-" } else { "" };
        if self.word("Infinity") {
            self.json5 = true;
            return Ok(J::Num(format!("{sign}9e999")));
        }
        if self.at(0) == Some(b'0') && matches!(self.at(1), Some(b'x' | b'X')) {
            self.json5 = true;
            self.i += 2;
            let hs = self.i;
            while self.at(0).is_some_and(|c| c.is_ascii_hexdigit()) {
                self.i += 1;
            }
            if hs == self.i {
                return Err(self.i - 1);
            }
            let text = std::str::from_utf8(&self.b[hs..self.i]).unwrap_or("0");
            return Ok(J::Num(match u64::from_str_radix(text, 16) {
                Ok(v) => format!("{sign}{v}"),
                Err(_) => format!("{sign}9.0e999"),
            }));
        }
        let int_start = self.i;
        let int_digits = self.digits();
        if int_digits > 1 && self.b[int_start] == b'0' {
            return Err(int_start + 1);
        }
        let mut frac: Option<&[u8]> = None;
        if self.at(0) == Some(b'.')
            && (int_digits > 0 || self.at(1).is_some_and(|c| c.is_ascii_digit()))
        {
            self.i += 1;
            let fs = self.i;
            let n = self.digits();
            if n == 0 || int_digits == 0 {
                self.json5 = true;
            }
            let _ = n;
            frac = Some(&self.b[fs..self.i]);
        } else if int_digits == 0 {
            return Err(start);
        }
        let exp_start = self.i;
        if matches!(self.at(0), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.at(0), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if self.digits() == 0 {
                return Err(exp_start);
            }
        }
        let canonical =
            self.b[start] != b'+' && int_digits > 0 && frac.is_none_or(|f| !f.is_empty());
        if canonical {
            let raw = &self.b[start..self.i];
            return Ok(J::Num(String::from_utf8_lossy(raw).into_owned()));
        }
        let mut out = String::from(sign);
        if int_digits == 0 {
            out.push('0');
        } else {
            out.push_str(
                std::str::from_utf8(&self.b[int_start..int_start + int_digits]).unwrap_or("0"),
            );
        }
        if let Some(f) = frac {
            out.push('.');
            if f.is_empty() {
                out.push('0');
            } else {
                out.push_str(std::str::from_utf8(f).unwrap_or("0"));
            }
        }
        out.push_str(std::str::from_utf8(&self.b[exp_start..self.i]).unwrap_or(""));
        Ok(J::Num(out))
    }

    /// A quoted string (opening quote `q` at the cursor). Returns the body
    /// as JSON text with its escapes kept as written (SQLite stores and
    /// prints them verbatim); JSON5 escapes are rewritten to JSON ones.
    fn string(&mut self, q: u8) -> PR<String> {
        if q == b'\'' {
            self.json5 = true;
        }
        if q != 0 {
            self.i += 1;
        }
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(c) = self.at(0) else {
                if q == 0 {
                    break;
                }
                return Err(self.i);
            };
            match c {
                c if c == q => {
                    self.i += 1;
                    break;
                }
                b'\\' => {
                    let e = self.at(1).ok_or(self.i + 1)?;
                    match e {
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {
                            out.extend_from_slice(&[b'\\', e]);
                            self.i += 2;
                        }
                        b'u' => {
                            let h = self.b.get(self.i + 2..self.i + 6).ok_or(self.i + 1)?;
                            if !h.iter().all(u8::is_ascii_hexdigit) {
                                return Err(self.i + 1);
                            }
                            out.extend_from_slice(&self.b[self.i..self.i + 6]);
                            self.i += 6;
                        }
                        b'x' => {
                            let h = self.b.get(self.i + 2..self.i + 4).ok_or(self.i + 1)?;
                            if !h.iter().all(u8::is_ascii_hexdigit) {
                                return Err(self.i + 1);
                            }
                            self.json5 = true;
                            out.extend_from_slice(b"\\u00");
                            out.extend_from_slice(h);
                            self.i += 4;
                        }
                        b'\'' => {
                            self.json5 = true;
                            out.push(b'\'');
                            self.i += 2;
                        }
                        // SQLite 3.45 renders `\v` as \u0009 (sic).
                        b'v' => {
                            self.json5 = true;
                            out.extend_from_slice(b"\\u0009");
                            self.i += 2;
                        }
                        b'0' => {
                            self.json5 = true;
                            out.extend_from_slice(b"\\u0000");
                            self.i += 2;
                        }
                        // Line continuations.
                        b'\n' => {
                            self.json5 = true;
                            self.i += 2;
                        }
                        b'\r' => {
                            self.json5 = true;
                            self.i += if self.at(2) == Some(b'\n') { 3 } else { 2 };
                        }
                        0xe2 if matches!(
                            self.b.get(self.i + 2..self.i + 4),
                            Some([0x80, 0xa8 | 0xa9])
                        ) =>
                        {
                            self.json5 = true;
                            self.i += 4;
                        }
                        _ => return Err(self.i + 1),
                    }
                }
                c if c < 0x20 => return Err(self.i),
                b'"' => {
                    // Only inside a single-quoted string.
                    out.extend_from_slice(b"\\\"");
                    self.i += 1;
                }
                c => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

/// Parse JSON text (JSON5 accepted). On failure the error is the byte
/// offset of the problem; the flag says whether JSON5 was used.
fn parse_text(text: &str) -> (PR<J>, bool) {
    let mut p = P {
        b: text.as_bytes(),
        i: 0,
        depth: 0,
        json5: false,
    };
    let r = p.value().and_then(|v| {
        p.ws()?;
        if p.i != p.b.len() {
            // SQLite reports trailing text at position 1.
            return Err(0);
        }
        Ok(v)
    });
    (r, p.json5)
}

pub fn parse(text: &str) -> Result<J> {
    parse_text(text).0.map_err(|_| malformed())
}

/// Escape text for use as a JSON string body (no quotes).
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
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
    out
}

/// Decode a JSON string body (as the parser keeps it) to text.
fn unescape(raw: &str) -> String {
    if !raw.contains('\\') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut it = raw.chars().peekable();
    let hex4 = |it: &mut std::iter::Peekable<std::str::Chars>| -> u32 {
        let h: String = it.by_ref().take(4).collect();
        u32::from_str_radix(&h, 16).unwrap_or(0xFFFD)
    };
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('u') => {
                let mut cp = hex4(&mut it);
                if (0xD800..0xDC00).contains(&cp) {
                    let mut look = it.clone();
                    if look.next() == Some('\\') && look.next() == Some('u') {
                        let lo = hex4(&mut look);
                        if (0xDC00..0xE000).contains(&lo) {
                            cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                            it = look;
                        }
                    }
                }
                out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Do two string bodies hold the same text?
fn same_text(a: &str, b: &str) -> bool {
    if !a.contains('\\') && !b.contains('\\') {
        a == b
    } else {
        unescape(a) == unescape(b)
    }
}

/// Does the string body `raw` hold the text `key`?
fn is_key(raw: &str, key: &str) -> bool {
    if raw.contains('\\') {
        unescape(raw) == key
    } else {
        raw == key
    }
}

/// A JSON string holding `s`.
pub fn text(s: &str) -> J {
    J::Str(escape(s))
}

fn write_str(raw: &str, out: &mut String) {
    out.push('"');
    out.push_str(raw);
    out.push('"');
}

pub fn write(j: &J, out: &mut String) {
    match j {
        J::Null => out.push_str("null"),
        J::True => out.push_str("true"),
        J::False => out.push_str("false"),
        J::Num(n) => out.push_str(n),
        J::Str(s) => write_str(s, out),
        J::Arr(a) => {
            out.push('[');
            for (i, v) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(v, out);
            }
            out.push(']');
        }
        J::Obj(o) => {
            out.push('{');
            for (i, (k, v)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(k, out);
                out.push(':');
                write(v, out);
            }
            out.push('}');
        }
    }
}

pub fn to_text(j: &J) -> String {
    let mut s = String::new();
    write(j, &mut s);
    s
}

/// The SQL value of a JSON value: containers stay JSON text.
pub fn to_sql(j: &J) -> Value {
    match j {
        J::Null => Value::Null,
        J::True => Value::Int(1),
        J::False => Value::Int(0),
        J::Num(n) => {
            if !n.contains(['.', 'e', 'E']) {
                if let Ok(i) = n.parse::<i64>() {
                    return Value::Int(i);
                }
            }
            Value::Real(n.parse().unwrap_or(0.0))
        }
        J::Str(s) => Value::text(unescape(s)),
        J::Arr(_) | J::Obj(_) => Value::text(to_text(j)),
    }
}

/// A SQL value as a JSON value. Text tagged with [`SUB`] is JSON already.
pub fn from_sql(v: &Value) -> Result<J> {
    Ok(match v {
        Value::Null => J::Null,
        Value::Int(i) => J::Num(i.to_string()),
        Value::Real(r) => {
            if r.is_finite() {
                J::Num(v.to_text())
            } else if *r > 0.0 {
                J::Num("9e999".into())
            } else if *r < 0.0 {
                J::Num("-9e999".into())
            } else {
                J::Null
            }
        }
        Value::Text(t) => match t.strip_prefix(SUB) {
            Some(raw) => parse(raw)?,
            None => text(t),
        },
        Value::Blob(b) => from_jsonb(b)?,
    })
}

/// Decode SQLite's binary JSON ("JSONB", SQLite 3.45+): each element is a
/// header (type in the low nibble, payload size or size-of-size in the
/// high nibble) followed by its payload.
pub fn from_jsonb(b: &[u8]) -> Result<J> {
    let (j, used) = jsonb_element(b, 0)?;
    if used != b.len() {
        return Err(malformed());
    }
    Ok(j)
}

fn jsonb_element(b: &[u8], depth: usize) -> Result<(J, usize)> {
    if depth > 1000 || b.is_empty() {
        return Err(malformed());
    }
    let kind = b[0] & 0x0f;
    let (size, hdr): (usize, usize) = match b[0] >> 4 {
        n @ 0..=11 => (n as usize, 1),
        12 => (*b.get(1).ok_or_else(malformed)? as usize, 2),
        13 => (
            u16::from_be_bytes(b.get(1..3).ok_or_else(malformed)?.try_into().unwrap()) as usize,
            3,
        ),
        14 => (
            u32::from_be_bytes(b.get(1..5).ok_or_else(malformed)?.try_into().unwrap()) as usize,
            5,
        ),
        _ => (
            u64::from_be_bytes(b.get(1..9).ok_or_else(malformed)?.try_into().unwrap()) as usize,
            9,
        ),
    };
    let end = hdr.checked_add(size).ok_or_else(malformed)?;
    let body = b.get(hdr..end).ok_or_else(malformed)?;
    let text = || String::from_utf8(body.to_vec()).map_err(|_| malformed());
    let j = match kind {
        0 => J::Null,
        1 => J::True,
        2 => J::False,
        3 | 5 => J::Num(text()?),
        // INT5 / FLOAT5 / TEXT5 hold JSON5 spellings: rewrite as JSON.
        4 | 6 | 9 => {
            let t = text()?;
            let mut p = P {
                b: t.as_bytes(),
                i: 0,
                depth: 0,
                json5: false,
            };
            let v = if kind == 9 {
                J::Str(p.string(0).map_err(|_| malformed())?)
            } else {
                p.number().map_err(|_| malformed())?
            };
            if p.i != t.len() {
                return Err(malformed());
            }
            v
        }
        7 | 10 => J::Str(escape(&text()?)),
        8 => J::Str(text()?),
        11 => {
            let mut items = Vec::new();
            let mut p = 0;
            while p < body.len() {
                let (v, n) = jsonb_element(&body[p..], depth + 1)?;
                items.push(v);
                p += n;
            }
            J::Arr(items)
        }
        12 => {
            let mut items = Vec::new();
            let mut p = 0;
            while p < body.len() {
                let (k, n) = jsonb_element(&body[p..], depth + 1)?;
                p += n;
                let J::Str(k) = k else {
                    return Err(malformed());
                };
                let (v, n) = jsonb_element(&body[p..], depth + 1)?;
                p += n;
                items.push((k, v));
            }
            J::Obj(items)
        }
        _ => return Err(malformed()),
    };
    Ok((j, end))
}

/// Parse a function's JSON argument (text, or a tagged JSON value).
fn arg(v: &Value) -> Result<Option<J>> {
    match v {
        Value::Null => Ok(None),
        Value::Text(t) => parse(t.strip_prefix(SUB).unwrap_or(t)).map(Some),
        Value::Blob(b) => from_jsonb(b).map(Some),
        other => parse(&other.to_text()).map(Some),
    }
}

/// Follow a path (`$`, `.key`, `."quoted key"`, `[N]`, `[#-N]`).
fn lookup<'j>(root: &'j J, path: &str) -> Result<Option<&'j J>> {
    let bad = || Error::Runtime(format!("bad JSON path: '{path}'"));
    let rest = path.strip_prefix('$').ok_or_else(bad)?;
    let b = rest.as_bytes();
    let mut i = 0;
    let mut cur = root;
    while i < b.len() {
        match b[i] {
            b'.' => {
                i += 1;
                let key: String = if b.get(i) == Some(&b'"') {
                    let end = rest[i + 1..].find('"').ok_or_else(bad)?;
                    let k = rest[i + 1..i + 1 + end].to_string();
                    i += end + 2;
                    k
                } else {
                    let start = i;
                    while i < b.len() && b[i] != b'.' && b[i] != b'[' {
                        i += 1;
                    }
                    if start == i {
                        return Err(bad());
                    }
                    rest[start..i].to_string()
                };
                match cur {
                    J::Obj(o) => match o.iter().find(|(k, _)| is_key(k, &key)) {
                        Some((_, v)) => cur = v,
                        None => return Ok(None),
                    },
                    _ => return Ok(None),
                }
            }
            b'[' => {
                let end = rest[i..].find(']').ok_or_else(bad)? + i;
                let inner = rest[i + 1..end].trim();
                i = end + 1;
                let J::Arr(a) = cur else { return Ok(None) };
                let idx: i64 = if let Some(back) = inner.strip_prefix('#') {
                    let back = back.trim();
                    let off: i64 = if back.is_empty() {
                        0
                    } else {
                        back.strip_prefix('-')
                            .ok_or_else(bad)?
                            .trim()
                            .parse::<i64>()
                            .map_err(|_| bad())?
                    };
                    a.len() as i64 - off
                } else {
                    inner.parse().map_err(|_| bad())?
                };
                match usize::try_from(idx).ok().and_then(|k| a.get(k)) {
                    Some(v) => cur = v,
                    None => return Ok(None),
                }
            }
            _ => return Err(bad()),
        }
    }
    Ok(Some(cur))
}

/// The right side of `->` / `->>`: a path, a key, or an array index.
fn arrow_path(v: &Value) -> String {
    match v {
        Value::Int(i) => format!("$[{i}]"),
        other => {
            let t = other.to_text();
            if t.starts_with('$') {
                t
            } else {
                format!("$.\"{t}\"")
            }
        }
    }
}

fn type_name(j: &J) -> &'static str {
    match j {
        J::Null => "null",
        J::True => "true",
        J::False => "false",
        J::Num(n) if n.contains(['.', 'e', 'E']) => "real",
        J::Num(_) => "integer",
        J::Str(_) => "text",
        J::Arr(_) => "array",
        J::Obj(_) => "object",
    }
}

// ------------------------------------------------------------ functions

pub fn f_json(a: &[Value]) -> Result<Value> {
    Ok(match arg(&a[0])? {
        None => Value::Null,
        Some(j) => Value::text(to_text(&j)),
    })
}

/// `json_valid(x [, flags])`: flags 1 = RFC 8259 text, 2 = JSON5 text,
/// 4 = probably JSONB, 8 = strictly JSONB (SQLite 3.45).
pub fn f_json_valid(a: &[Value]) -> Result<Value> {
    let flags = match a.get(1) {
        None => 1,
        Some(f) => {
            let f = f.to_int();
            if !(1..=15).contains(&f) {
                return Err(Error::Runtime(
                    "FLAGS parameter to json_valid() must be between 1 and 15".into(),
                ));
            }
            f
        }
    };
    Ok(match &a[0] {
        Value::Null => Value::Null,
        Value::Blob(b) => Value::Int(match flags & 0x0c {
            0 => 0,
            _ => jsonb_valid(b, flags & 8 != 0) as i64,
        }),
        other => {
            if flags & 3 == 0 {
                return Ok(Value::Int(0));
            }
            let (r, json5) = parse_text(&other.to_text());
            Value::Int((r.is_ok() && (flags & 2 != 0 || !json5)) as i64)
        }
    })
}

/// Is `b` JSONB? Loosely: the outer header is sound and spans the blob.
/// Strictly: every element decodes, with sizes the types allow.
fn jsonb_valid(b: &[u8], strict: bool) -> bool {
    fn check(b: &[u8], depth: usize) -> Option<usize> {
        if depth > 1000 {
            return None;
        }
        let kind = *b.first()? & 0x0f;
        let (size, hdr): (usize, usize) = match b[0] >> 4 {
            n @ 0..=11 => (n as usize, 1),
            12 => (*b.get(1)? as usize, 2),
            13 => (
                u16::from_be_bytes(b.get(1..3)?.try_into().ok()?) as usize,
                3,
            ),
            14 => (
                u32::from_be_bytes(b.get(1..5)?.try_into().ok()?) as usize,
                5,
            ),
            _ => (
                usize::try_from(u64::from_be_bytes(b.get(1..9)?.try_into().ok()?)).ok()?,
                9,
            ),
        };
        let end = hdr.checked_add(size)?;
        let body = b.get(hdr..end)?;
        match kind {
            0..=2 if size != 0 => return None,
            0..=2 => {}
            3..=10 => {
                if from_jsonb(&b[..end]).is_err() {
                    return None;
                }
            }
            11 | 12 => {
                let mut p = 0;
                let mut n = 0;
                while p < body.len() {
                    if kind == 12 && n % 2 == 0 && !(7..=10).contains(&(body[p] & 0x0f)) {
                        return None;
                    }
                    p += check(&body[p..], depth + 1)?;
                    n += 1;
                }
                if kind == 12 && n % 2 != 0 {
                    return None;
                }
            }
            _ => return None,
        }
        Some(end)
    }
    if strict {
        return check(b, 0) == Some(b.len());
    }
    let Some(&h) = b.first() else { return false };
    let kind = h & 0x0f;
    let (size, hdr): (usize, usize) = match h >> 4 {
        n @ 0..=11 => (n as usize, 1),
        12 => (b.get(1).map_or(usize::MAX, |&x| x as usize), 2),
        13 => (
            b.get(1..3)
                .map_or(usize::MAX, |x| u16::from_be_bytes([x[0], x[1]]) as usize),
            3,
        ),
        14 => (
            b.get(1..5).map_or(usize::MAX, |x| {
                u32::from_be_bytes([x[0], x[1], x[2], x[3]]) as usize
            }),
            5,
        ),
        _ => return false,
    };
    kind <= 12 && hdr.checked_add(size) == Some(b.len()) && !(kind <= 2 && size != 0)
}

/// `json_error_position(x)`: 0 for well-formed JSON (JSON5 allowed), else
/// the 1-based character position of the first error.
pub fn f_json_error_position(a: &[Value]) -> Result<Value> {
    Ok(match &a[0] {
        Value::Null => Value::Null,
        Value::Blob(b) => Value::Int(if jsonb_valid(b, true) { 0 } else { 1 }),
        other => {
            let t = other.to_text();
            match parse_text(&t).0 {
                Ok(_) => Value::Int(0),
                Err(at) => {
                    let at = at.min(t.len());
                    let chars = t.as_bytes()[..at]
                        .iter()
                        .filter(|b| (**b & 0xc0) != 0x80)
                        .count();
                    Value::Int(chars as i64 + 1)
                }
            }
        }
    })
}

pub fn f_json_type(a: &[Value]) -> Result<Value> {
    let Some(j) = arg(&a[0])? else {
        return Ok(Value::Null);
    };
    let found = match a.get(1) {
        Some(p) if p.is_null() => return Ok(Value::Null),
        Some(p) => lookup(&j, &p.to_text())?,
        None => Some(&j),
    };
    Ok(found.map_or(Value::Null, |v| Value::text(type_name(v))))
}

pub fn f_json_extract(a: &[Value]) -> Result<Value> {
    let Some(j) = arg(&a[0])? else {
        return Ok(Value::Null);
    };
    if a.len() == 2 {
        if a[1].is_null() {
            return Ok(Value::Null);
        }
        return Ok(lookup(&j, &a[1].to_text())?.map_or(Value::Null, to_sql));
    }
    // Several paths: a JSON array of the results.
    let mut out = Vec::new();
    for p in &a[1..] {
        if p.is_null() {
            return Ok(Value::Null);
        }
        out.push(lookup(&j, &p.to_text())?.cloned().unwrap_or(J::Null));
    }
    Ok(Value::text(to_text(&J::Arr(out))))
}

/// `x -> path`: the JSON text of the element (NULL if absent).
pub fn f_arrow(a: &[Value]) -> Result<Value> {
    let Some(j) = arg(&a[0])? else {
        return Ok(Value::Null);
    };
    if a[1].is_null() {
        return Ok(Value::Null);
    }
    Ok(lookup(&j, &arrow_path(&a[1]))?.map_or(Value::Null, |v| Value::text(to_text(v))))
}

/// `x ->> path`: the SQL value of the element.
pub fn f_arrow2(a: &[Value]) -> Result<Value> {
    let Some(j) = arg(&a[0])? else {
        return Ok(Value::Null);
    };
    if a[1].is_null() {
        return Ok(Value::Null);
    }
    Ok(lookup(&j, &arrow_path(&a[1]))?.map_or(Value::Null, to_sql))
}

pub fn f_json_array(a: &[Value]) -> Result<Value> {
    let items = a.iter().map(from_sql).collect::<Result<Vec<_>>>()?;
    Ok(Value::text(to_text(&J::Arr(items))))
}

pub fn f_json_object(a: &[Value]) -> Result<Value> {
    if !a.len().is_multiple_of(2) {
        return Err(Error::Sql(
            "json_object() requires an even number of arguments".into(),
        ));
    }
    let mut o = Vec::new();
    for pair in a.chunks(2) {
        let Value::Text(k) = &pair[0] else {
            return Err(Error::Runtime("json_object() labels must be TEXT".into()));
        };
        o.push((escape(k), from_sql(&pair[1])?));
    }
    Ok(Value::text(to_text(&J::Obj(o))))
}

pub fn f_json_array_length(a: &[Value]) -> Result<Value> {
    let Some(j) = arg(&a[0])? else {
        return Ok(Value::Null);
    };
    let found = match a.get(1) {
        Some(p) if p.is_null() => return Ok(Value::Null),
        Some(p) => lookup(&j, &p.to_text())?,
        None => Some(&j),
    };
    Ok(match found {
        None => Value::Null,
        Some(J::Arr(v)) => Value::Int(v.len() as i64),
        Some(_) => Value::Int(0),
    })
}

pub fn f_json_quote(a: &[Value]) -> Result<Value> {
    Ok(Value::text(to_text(&from_sql(&a[0])?)))
}

/// Tag a value produced by a JSON function so a JSON builder nests it
/// instead of quoting it.
pub fn f_json_sub(a: &[Value]) -> Result<Value> {
    Ok(match &a[0] {
        Value::Text(t) if !t.starts_with(SUB) && parse(t).is_ok() => {
            Value::text(format!("{SUB}{t}"))
        }
        other => other.clone(),
    })
}

/// The same for `json_extract`, whose result is JSON only when it is an
/// object or array (a string comes back as plain SQL text).
pub fn f_json_sub_container(a: &[Value]) -> Result<Value> {
    Ok(match &a[0] {
        Value::Text(t)
            if t.starts_with(['[', '{']) && matches!(parse(t), Ok(J::Arr(_) | J::Obj(_))) =>
        {
            Value::text(format!("{SUB}{t}"))
        }
        other => other.clone(),
    })
}

// ------------------------------------------------------------ editing

/// One step of a JSON path.
enum Step {
    Key(String),
    /// `[N]`
    At(i64),
    /// `[#-N]` (N may be 0: `[#]`)
    FromEnd(i64),
}

/// Parse a whole path up front (editing functions reject a bad path even
/// where the document has no match).
fn parse_path(path: &str) -> Result<Vec<Step>> {
    let bad = || Error::Runtime(format!("bad JSON path: '{path}'"));
    let rest = path.strip_prefix('$').ok_or_else(bad)?;
    let b = rest.as_bytes();
    let mut i = 0;
    let mut steps = Vec::new();
    while i < b.len() {
        match b[i] {
            b'.' => {
                i += 1;
                if b.get(i) == Some(&b'"') {
                    let end = rest[i + 1..].find('"').ok_or_else(bad)?;
                    steps.push(Step::Key(rest[i + 1..i + 1 + end].to_string()));
                    i += end + 2;
                } else {
                    let start = i;
                    while i < b.len() && b[i] != b'.' && b[i] != b'[' {
                        i += 1;
                    }
                    if start == i {
                        return Err(bad());
                    }
                    steps.push(Step::Key(rest[start..i].to_string()));
                }
            }
            b'[' => {
                let end = rest[i..].find(']').ok_or_else(bad)? + i;
                let inner = rest[i + 1..end].trim();
                i = end + 1;
                if let Some(back) = inner.strip_prefix('#') {
                    let back = back.trim();
                    let n: i64 = if back.is_empty() {
                        0
                    } else {
                        back.strip_prefix('-')
                            .ok_or_else(bad)?
                            .trim()
                            .parse()
                            .map_err(|_| bad())?
                    };
                    steps.push(Step::FromEnd(n));
                } else {
                    steps.push(Step::At(inner.parse().map_err(|_| bad())?));
                }
            }
            _ => return Err(bad()),
        }
    }
    Ok(steps)
}

fn index_of(step: &Step, len: usize) -> Option<i64> {
    match step {
        Step::At(i) => Some(*i),
        Step::FromEnd(n) => Some(len as i64 - n),
        Step::Key(_) => None,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Edit {
    Set,
    Insert,
    Replace,
}

/// A new value for a path that does not exist yet: the containers its
/// remaining steps name, holding `val`. None if a step cannot be created
/// (an array index other than the end of a new, empty array).
fn build(steps: &[Step], val: &J) -> Option<J> {
    let Some((step, rest)) = steps.split_first() else {
        return Some(val.clone());
    };
    match step {
        Step::Key(k) => Some(J::Obj(vec![(escape(k), build(rest, val)?)])),
        other => (index_of(other, 0) == Some(0))
            .then(|| build(rest, val))
            .flatten()
            .map(|v| J::Arr(vec![v])),
    }
}

fn edit(cur: &mut J, steps: &[Step], val: &J, mode: Edit) {
    let Some((step, rest)) = steps.split_first() else {
        if mode != Edit::Insert {
            *cur = val.clone();
        }
        return;
    };
    match (step, cur) {
        (Step::Key(k), J::Obj(o)) => match o.iter().position(|(n, _)| is_key(n, k)) {
            Some(pos) => edit(&mut o[pos].1, rest, val, mode),
            None if mode != Edit::Replace => {
                if let Some(v) = build(rest, val) {
                    o.push((escape(k), v));
                }
            }
            None => {}
        },
        (Step::At(_) | Step::FromEnd(_), J::Arr(a)) => {
            let Some(idx) = index_of(step, a.len()) else {
                return;
            };
            if idx >= 0 && (idx as usize) < a.len() {
                edit(&mut a[idx as usize], rest, val, mode);
            } else if idx == a.len() as i64 && mode != Edit::Replace {
                if let Some(v) = build(rest, val) {
                    a.push(v);
                }
            }
        }
        _ => {}
    }
}

/// Remove the element at `steps` (which is not empty) if it exists.
fn remove(cur: &mut J, steps: &[Step]) {
    let (step, rest) = steps.split_first().expect("non-empty path");
    match (step, cur) {
        (Step::Key(k), J::Obj(o)) => {
            if let Some(pos) = o.iter().position(|(n, _)| is_key(n, k)) {
                if rest.is_empty() {
                    o.remove(pos);
                } else {
                    remove(&mut o[pos].1, rest);
                }
            }
        }
        (Step::At(_) | Step::FromEnd(_), J::Arr(a)) => {
            let Some(idx) = index_of(step, a.len()) else {
                return;
            };
            if idx >= 0 && (idx as usize) < a.len() {
                if rest.is_empty() {
                    a.remove(idx as usize);
                } else {
                    remove(&mut a[idx as usize], rest);
                }
            }
        }
        _ => {}
    }
}

fn edit_fn(name: &str, a: &[Value], mode: Edit) -> Result<Value> {
    if a.len().is_multiple_of(2) {
        return Err(Error::Runtime(format!(
            "{name}() needs an odd number of arguments"
        )));
    }
    let Some(mut j) = arg(&a[0])? else {
        return Ok(Value::Null);
    };
    for pair in a[1..].chunks(2) {
        if pair[0].is_null() {
            continue;
        }
        let steps = parse_path(&pair[0].to_text())?;
        let val = from_sql(&pair[1])?;
        edit(&mut j, &steps, &val, mode);
    }
    Ok(Value::text(to_text(&j)))
}

pub fn f_json_set(a: &[Value]) -> Result<Value> {
    edit_fn("json_set", a, Edit::Set)
}

pub fn f_json_insert(a: &[Value]) -> Result<Value> {
    edit_fn("json_insert", a, Edit::Insert)
}

pub fn f_json_replace(a: &[Value]) -> Result<Value> {
    edit_fn("json_replace", a, Edit::Replace)
}

pub fn f_json_remove(a: &[Value]) -> Result<Value> {
    let Some(mut j) = arg(&a[0])? else {
        return Ok(Value::Null);
    };
    for p in &a[1..] {
        if p.is_null() {
            return Ok(Value::Null);
        }
        let steps = parse_path(&p.to_text())?;
        if steps.is_empty() {
            return Ok(Value::Null);
        }
        remove(&mut j, &steps);
    }
    Ok(Value::text(to_text(&j)))
}

/// RFC 7396 merge patch.
fn merge_patch(target: J, patch: &J) -> J {
    let J::Obj(p) = patch else {
        return patch.clone();
    };
    let mut t = match target {
        J::Obj(o) => o,
        _ => Vec::new(),
    };
    for (k, pv) in p {
        let pos = t.iter().position(|(n, _)| same_text(n, k));
        match (pv, pos) {
            (J::Null, Some(i)) => {
                t.remove(i);
            }
            (J::Null, None) => {}
            (_, Some(i)) => {
                let old = std::mem::replace(&mut t[i].1, J::Null);
                t[i].1 = merge_patch(old, pv);
            }
            (_, None) => t.push((k.clone(), merge_patch(J::Null, pv))),
        }
    }
    J::Obj(t)
}

pub fn f_json_patch(a: &[Value]) -> Result<Value> {
    let (Some(t), Some(p)) = (arg(&a[0])?, arg(&a[1])?) else {
        return Ok(Value::Null);
    };
    Ok(Value::text(to_text(&merge_patch(t, &p))))
}

// ------------------------------------------------------------ json_each / json_tree

/// Columns of json_each and json_tree: eight visible, then the hidden
/// arguments `json` and `root`, then the rowid (a 0-based row counter).
pub const TABLE_COLUMNS: [&str; 11] = [
    "key", "value", "type", "atom", "id", "parent", "fullkey", "path", "json", "root", "rowid",
];

/// Bytes a JSONB header takes for a payload of `n` bytes.
fn jsonb_header(n: usize) -> usize {
    match n {
        0..=11 => 1,
        12..=0xff => 2,
        0x100..=0xffff => 3,
        0x1_0000..=0xffff_ffff => 5,
        _ => 9,
    }
}

/// The payload length of a string as SQLite stores it (its JSON text
/// without the quotes, as SQLite keeps the text it parsed; this is exact
/// for JSON written in canonical form).
fn str_payload(s: &str) -> usize {
    s.len()
}

/// Payload size of a value in SQLite's binary JSON (JSONB) encoding, which
/// is what the `id` column of json_each / json_tree counts in (SQLite 3.45+).
fn jsonb_payload(j: &J) -> usize {
    match j {
        J::Null | J::True | J::False => 0,
        J::Num(n) => n.len(),
        J::Str(s) => str_payload(s),
        J::Arr(a) => a.iter().map(jsonb_size).sum(),
        J::Obj(o) => o
            .iter()
            .map(|(k, v)| {
                let kp = str_payload(k);
                jsonb_header(kp) + kp + jsonb_size(v)
            })
            .sum(),
    }
}

/// Total JSONB size of a value: header plus payload.
fn jsonb_size(j: &J) -> usize {
    let p = jsonb_payload(j);
    jsonb_header(p) + p
}

/// Header size of a value's JSONB encoding.
fn header_of(j: &J) -> usize {
    jsonb_header(jsonb_payload(j))
}

fn path_key(k: &str) -> String {
    let ident = k.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && k.chars().all(|c| c.is_ascii_alphanumeric());
    if ident {
        format!(".{k}")
    } else {
        format!(".\"{k}\"")
    }
}

/// A node reached by a path: the value, its JSONB offset, the offset of
/// its label (object members).
struct Loc<'j> {
    j: &'j J,
    off: usize,
    label: Option<usize>,
}

/// Follow `path` keeping track of JSONB offsets.
fn locate<'j>(root: &'j J, path: &str) -> Result<Option<Loc<'j>>> {
    // Resolve with `lookup` first (it validates the path), then walk the
    // same components again to find offsets.
    if lookup(root, path)?.is_none() {
        return Ok(None);
    }
    let mut loc = Loc {
        j: root,
        off: 0,
        label: None,
    };
    let rest = &path[1..];
    let b = rest.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let body = loc.off + header_of(loc.j);
        match (b[i], loc.j) {
            (b'.', J::Obj(o)) => {
                i += 1;
                let key: String = if b.get(i) == Some(&b'"') {
                    let end = rest[i + 1..].find('"').unwrap_or(0);
                    let k = rest[i + 1..i + 1 + end].to_string();
                    i += end + 2;
                    k
                } else {
                    let start = i;
                    while i < b.len() && b[i] != b'.' && b[i] != b'[' {
                        i += 1;
                    }
                    rest[start..i].to_string()
                };
                let mut at = body;
                let mut found = None;
                for (k, v) in o {
                    let kp = str_payload(k);
                    let ksize = jsonb_header(kp) + kp;
                    if is_key(k, &key) {
                        found = Some((at, at + ksize, v));
                        break;
                    }
                    at += ksize + jsonb_size(v);
                }
                let Some((label, off, v)) = found else {
                    return Ok(None);
                };
                loc = Loc {
                    j: v,
                    off,
                    label: Some(label),
                };
            }
            (b'[', J::Arr(a)) => {
                let end = rest[i..].find(']').unwrap_or(0) + i;
                let inner = rest[i + 1..end].trim();
                i = end + 1;
                let idx: usize = if let Some(back) = inner.strip_prefix('#') {
                    let off: usize = back
                        .trim()
                        .strip_prefix('-')
                        .map_or(0, |n| n.trim().parse().unwrap_or(0));
                    a.len() - off
                } else {
                    inner.parse().unwrap_or(0)
                };
                let off = body + a[..idx].iter().map(jsonb_size).sum::<usize>();
                loc = Loc {
                    j: &a[idx],
                    off,
                    label: None,
                };
            }
            _ => return Ok(None),
        }
    }
    Ok(Some(loc))
}

/// The key and path columns of json_tree's first row when it starts below
/// the root. SQLite finds them by scanning the path text backwards for a
/// `.` or `[` whose prefix names a container that starts with the node;
/// failing that it splits after the `$`. Copied, quirks included.
fn tree_root_key(root: &J, path: &str, start_id: usize) -> (Value, String) {
    let b = path.as_bytes();
    let mut n = b.len();
    if n >= 2 {
        while n > 1 {
            n -= 1;
            if b[n] == b'[' || b[n] == b'.' {
                if let Ok(Some(loc)) = locate(root, &path[..n]) {
                    if loc.off + header_of(loc.j) == start_id {
                        break;
                    }
                }
            }
        }
    }
    let rest = b.len() - n;
    let key = if b.len() == 1 || rest == 0 {
        Value::Null
    } else if b[n] == b'[' {
        let t = &path[n + 1..];
        let (neg, digits) = match t.strip_prefix('-') {
            Some(d) => (true, d),
            None => (false, t.strip_prefix('+').unwrap_or(t)),
        };
        let mut x: i64 = 0;
        for c in digits.bytes().take_while(u8::is_ascii_digit) {
            x = x.wrapping_mul(10).wrapping_add(i64::from(c - b'0'));
        }
        Value::Int(if neg { -x } else { x })
    } else if b.get(n + 1) == Some(&b'"') && rest >= 3 {
        Value::text(&path[n + 2..n + rest - 1])
    } else {
        Value::text(&path[n + 1..])
    };
    (key, path[..n].to_string())
}

fn atom(j: &J) -> Value {
    match j {
        J::Arr(_) | J::Obj(_) => Value::Null,
        other => to_sql(other),
    }
}

/// Rows of `json_each(json [, path])` (or `json_tree` when `tree`), each
/// with the columns in [`TABLE_COLUMNS`].
pub fn table_rows(tree: bool, args: &[Value]) -> Result<Vec<Vec<Value>>> {
    let Some(src) = args.first() else {
        return Err(Error::Sql("wrong number of arguments".into()));
    };
    let Some(root) = arg(src)? else {
        return Ok(Vec::new());
    };
    let path = match args.get(1) {
        None => "$".to_string(),
        Some(p) if p.is_null() => return Ok(Vec::new()),
        Some(p) => p.to_text(),
    };
    if !path.starts_with('$') {
        return Err(Error::Runtime(format!("bad JSON path: '{path}'")));
    }
    let Some(start) = locate(&root, &path)? else {
        return Ok(Vec::new());
    };
    let json = src.clone();
    let root_arg = Value::text(&path);
    let mut out = Vec::new();
    let mut row = |key: Value, j: &J, id: usize, parent: Value, fullkey: String, path: String| {
        out.push(vec![
            key,
            to_sql(j),
            Value::text(type_name(j)),
            atom(j),
            Value::Int(id as i64),
            parent,
            Value::text(fullkey),
            Value::text(path),
            json.clone(),
            root_arg.clone(),
            Value::Int(out.len() as i64),
        ]);
    };
    // Children of the container at `off` (fullkey `fk`).
    fn children<'a>(j: &'a J, off: usize, fk: &str) -> Vec<(Value, &'a J, usize, usize, String)> {
        let body = off + header_of(j);
        let mut v = Vec::new();
        match j {
            J::Arr(a) => {
                let mut at = body;
                for (i, e) in a.iter().enumerate() {
                    v.push((Value::Int(i as i64), e, at, at, format!("{fk}[{i}]")));
                    at += jsonb_size(e);
                }
            }
            J::Obj(o) => {
                let mut at = body;
                for (k, e) in o {
                    let kp = str_payload(k);
                    let ks = jsonb_header(kp) + kp;
                    v.push((
                        Value::text(unescape(k)),
                        e,
                        at,
                        at + ks,
                        format!("{fk}{}", path_key(k)),
                    ));
                    at += ks + jsonb_size(e);
                }
            }
            _ => {}
        }
        v
    }
    let start_id = start.label.unwrap_or(start.off);
    if !tree {
        match start.j {
            J::Arr(_) | J::Obj(_) => {
                for (key, e, id, eoff, fk) in children(start.j, start.off, &path) {
                    let _ = eoff;
                    row(key, e, id, Value::Null, fk, path.clone());
                }
            }
            prim => row(
                Value::Null,
                prim,
                start_id,
                Value::Null,
                path.clone(),
                path.clone(),
            ),
        }
        return Ok(out);
    }
    // json_tree: the start node, then everything below it, depth first.
    let (key, parent) = tree_root_key(&root, &path, start_id);
    row(key, start.j, start_id, Value::Null, path.clone(), parent);
    let mut stack: Vec<(&J, usize, usize, String)> =
        vec![(start.j, start.off, start_id, path.clone())];
    // Depth-first in document order: use an explicit stack of iterators.
    fn walk<'a>(
        j: &'a J,
        off: usize,
        id: usize,
        fk: &str,
        emit: &mut dyn FnMut(Value, &'a J, usize, Value, String, String),
    ) {
        for (key, e, cid, eoff, cfk) in children(j, off, fk) {
            emit(
                key,
                e,
                cid,
                Value::Int(id as i64),
                cfk.clone(),
                fk.to_string(),
            );
            walk(e, eoff, cid, &cfk, emit);
        }
    }
    let (j0, off0, id0, fk0) = stack.pop().unwrap();
    walk(j0, off0, id0, &fk0, &mut |k, e, id, p, fk, pth| {
        row(k, e, id, p, fk, pth)
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_paths() {
        let j = parse(r#" { "a" : [1, 2.50, "xé\n"], "b": {"c": true, "d": null} } "#).unwrap();
        assert_eq!(
            to_text(&j),
            r#"{"a":[1,2.50,"xé\n"],"b":{"c":true,"d":null}}"#
        );
        assert_eq!(lookup(&j, "$.a[1]").unwrap(), Some(&J::Num("2.50".into())));
        assert_eq!(
            lookup(&j, "$.a[#-1]").unwrap(),
            Some(&J::Str("xé\\n".into()))
        );
        assert_eq!(lookup(&j, "$.b.c").unwrap(), Some(&J::True));
        assert_eq!(lookup(&j, "$.zz").unwrap(), None);
        assert!(parse("[1,,]").is_err());
        assert!(parse("{\"a\" 1}").is_err());
    }

    #[test]
    fn json5_and_escapes() {
        let t = |s: &str| to_text(&parse(s).unwrap());
        assert_eq!(
            t("{a:1, 'b':[.5, 5., +3, 0x1f, Infinity, NaN,],}"),
            r#"{"a":1,"b":[0.5,5.0,3,31,9e999,null]}"#
        );
        // Escapes are kept as written; text values decode them.
        let e = |s: &str| s.replace('~', "\\");
        assert_eq!(t(&e(r#"["a~/b", "~u0041"]"#)), e(r#"["a~/b","~u0041"]"#));
        assert_eq!(
            to_sql(&parse(&e(r#""~u0041~n""#)).unwrap()),
            Value::text("A\n")
        );
        assert_eq!(
            t(&e(r#"['say "hi"', "~x41~v"]"#)),
            e(r#"["say ~"hi~"","~u0041~u0009"]"#)
        );
        let pos = |s: &str| parse_text(s).0.unwrap_err();
        assert_eq!(pos("[1 2]"), 3);
        assert_eq!(pos("[1, :2]"), 3);
        assert_eq!(pos("[1]x"), 0);
    }
}
