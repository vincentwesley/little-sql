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
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn value(&mut self) -> Result<J> {
        self.depth += 1;
        if self.depth > 1000 {
            return Err(malformed());
        }
        self.ws();
        let c = *self.b.get(self.i).ok_or_else(malformed)?;
        let v = match c {
            b'{' => {
                self.i += 1;
                let mut obj = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                } else {
                    loop {
                        self.ws();
                        let J::Str(k) = self.string()? else {
                            unreachable!()
                        };
                        self.ws();
                        if self.b.get(self.i) != Some(&b':') {
                            return Err(malformed());
                        }
                        self.i += 1;
                        let v = self.value()?;
                        obj.push((k, v));
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b'}') => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(malformed()),
                        }
                    }
                }
                J::Obj(obj)
            }
            b'[' => {
                self.i += 1;
                let mut arr = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                } else {
                    loop {
                        arr.push(self.value()?);
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b']') => {
                                self.i += 1;
                                break;
                            }
                            _ => return Err(malformed()),
                        }
                    }
                }
                J::Arr(arr)
            }
            b'"' => self.string()?,
            b't' if self.b[self.i..].starts_with(b"true") => {
                self.i += 4;
                J::True
            }
            b'f' if self.b[self.i..].starts_with(b"false") => {
                self.i += 5;
                J::False
            }
            b'n' if self.b[self.i..].starts_with(b"null") => {
                self.i += 4;
                J::Null
            }
            b'-' | b'0'..=b'9' => self.number()?,
            _ => return Err(malformed()),
        };
        self.depth -= 1;
        Ok(v)
    }

    fn number(&mut self) -> Result<J> {
        let start = self.i;
        if self.b[self.i] == b'-' {
            self.i += 1;
        }
        let digits = |p: &mut Self| {
            let s = p.i;
            while p.i < p.b.len() && p.b[p.i].is_ascii_digit() {
                p.i += 1;
            }
            p.i - s
        };
        if digits(self) == 0 {
            return Err(malformed());
        }
        if self.b.get(self.i) == Some(&b'.') {
            self.i += 1;
            if digits(self) == 0 {
                return Err(malformed());
            }
        }
        if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if digits(self) == 0 {
                return Err(malformed());
            }
        }
        Ok(J::Num(
            String::from_utf8_lossy(&self.b[start..self.i]).into_owned(),
        ))
    }

    fn string(&mut self) -> Result<J> {
        if self.b.get(self.i) != Some(&b'"') {
            return Err(malformed());
        }
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or_else(malformed)?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or_else(malformed)?;
                    self.i += 1;
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp)
                                && self.b[self.i..].starts_with(b"\\u")
                            {
                                self.i += 2;
                                let lo = self.hex4()?;
                                cp = 0x10000
                                    + ((cp - 0xD800) << 10)
                                    + (lo.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return Err(malformed()),
                    }
                }
                c if c < 0x20 => return Err(malformed()),
                c => out.push(c),
            }
        }
        Ok(J::Str(String::from_utf8_lossy(&out).into_owned()))
    }

    fn hex4(&mut self) -> Result<u32> {
        let h = self.b.get(self.i..self.i + 4).ok_or_else(malformed)?;
        let s = std::str::from_utf8(h).map_err(|_| malformed())?;
        self.i += 4;
        u32::from_str_radix(s, 16).map_err(|_| malformed())
    }
}

pub fn parse(text: &str) -> Result<J> {
    let mut p = P {
        b: text.as_bytes(),
        i: 0,
        depth: 0,
    };
    let v = p.value()?;
    p.ws();
    if p.i != p.b.len() {
        return Err(malformed());
    }
    Ok(v)
}

fn write_str(s: &str, out: &mut String) {
    out.push('"');
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
        J::Str(s) => Value::text(s),
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
            None => J::Str(t.to_string()),
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
        3..=6 => J::Num(text()?),
        7 | 10 => J::Str(text()?),
        8 | 9 => match parse(&format!("\"{}\"", text()?))? {
            J::Str(s) => J::Str(s),
            _ => return Err(malformed()),
        },
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
                    J::Obj(o) => match o.iter().find(|(k, _)| *k == key) {
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

pub fn f_json_valid(a: &[Value]) -> Result<Value> {
    Ok(match &a[0] {
        Value::Null => Value::Null,
        Value::Text(t) => Value::Int(parse(t).is_ok() as i64),
        Value::Int(_) | Value::Real(_) => Value::Int(1),
        Value::Blob(_) => Value::Int(0),
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
        o.push((k.to_string(), from_sql(&pair[1])?));
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
            Some(&J::Str("xé\n".into()))
        );
        assert_eq!(lookup(&j, "$.b.c").unwrap(), Some(&J::True));
        assert_eq!(lookup(&j, "$.zz").unwrap(), None);
        assert!(parse("[1,]").is_err());
        assert!(parse("{\"a\" 1}").is_err());
    }
}
