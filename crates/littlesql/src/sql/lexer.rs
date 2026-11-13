//! SQL tokenizer.

use crate::error::{Error, Result};

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    /// A bare word: keyword or identifier, decided by the parser.
    Word(String),
    /// A quoted identifier. `dq` is true for double quotes, which SQLite
    /// lets fall back to a string literal when no such column exists.
    Quoted {
        name: String,
        dq: bool,
    },
    Str(String),
    /// A numeric literal, kept as written.
    Num(String),
    Blob(Vec<u8>),
    /// Operator or punctuation.
    Op(&'static str),
    /// A bound parameter (`?`, `?1`, `:name`, ...). Parsed so that the error
    /// message is helpful; parameters are not supported.
    Param,
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    /// Byte offset where the token starts.
    pub start: usize,
    /// Byte offset just past the token.
    pub end: usize,
}

const OPS: &[&str] = &[
    "||", "<<", ">>", "<=", ">=", "==", "!=", "<>", "->>", "->", "(", ")", ",", ";", ".", "+", "-",
    "*", "/", "%", "&", "|", "~", "<", ">", "=",
];

pub fn tokenize(sql: &str) -> Result<Vec<Token>> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'-' && b.get(i + 1) == Some(&b'-') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(b.len());
            continue;
        }
        let start = i;
        let tok = if c == b'\'' {
            let (s, n) = quoted(sql, i, b'\'')?;
            i = n;
            Tok::Str(s)
        } else if c == b'"' || c == b'`' {
            let (s, n) = quoted(sql, i, c)?;
            i = n;
            Tok::Quoted {
                name: s,
                dq: c == b'"',
            }
        } else if c == b'[' {
            let end = sql[i..]
                .find(']')
                .ok_or_else(|| Error::Parse("unterminated [identifier]".into()))?;
            let s = sql[i + 1..i + end].to_string();
            i += end + 1;
            Tok::Quoted { name: s, dq: false }
        } else if (c == b'x' || c == b'X') && b.get(i + 1) == Some(&b'\'') {
            let (s, n) = quoted(sql, i + 1, b'\'')?;
            i = n;
            if s.len() % 2 != 0 || !s.bytes().all(|h| h.is_ascii_hexdigit()) {
                return Err(Error::Parse(format!("malformed blob literal x'{s}'")));
            }
            let bytes = (0..s.len())
                .step_by(2)
                .map(|k| u8::from_str_radix(&s[k..k + 2], 16).unwrap())
                .collect();
            Tok::Blob(bytes)
        } else if c.is_ascii_digit()
            || (c == b'.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
        {
            i = number_end(b, i);
            Tok::Num(sql[start..i].to_string())
        } else if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            while i < b.len()
                && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$' || b[i] >= 0x80)
            {
                i += 1;
            }
            Tok::Word(sql[start..i].to_string())
        } else if c == b'?' || c == b':' || c == b'@' || c == b'$' {
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            Tok::Param
        } else {
            let rest = &sql[i..];
            match OPS.iter().find(|op| rest.starts_with(**op)) {
                Some(op) => {
                    i += op.len();
                    Tok::Op(op)
                }
                None => {
                    let ch = rest.chars().next().unwrap();
                    return Err(Error::Parse(format!("unrecognized token: \"{ch}\"")));
                }
            }
        };
        out.push(Token { tok, start, end: i });
    }
    out.push(Token {
        tok: Tok::Eof,
        start: sql.len(),
        end: sql.len(),
    });
    Ok(out)
}

/// Read a quoted token starting at `start` (which holds the quote). A
/// doubled quote character stands for itself. Returns the unescaped content
/// and the offset just past the closing quote.
fn quoted(sql: &str, start: usize, q: u8) -> Result<(String, usize)> {
    let b = sql.as_bytes();
    let mut i = start + 1;
    let mut seg = i;
    let mut out = String::new();
    loop {
        if i >= b.len() {
            return Err(Error::Parse("unterminated quoted string".into()));
        }
        if b[i] == q {
            out.push_str(&sql[seg..i]);
            if b.get(i + 1) == Some(&q) {
                out.push(q as char);
                i += 2;
                seg = i;
            } else {
                return Ok((out, i + 1));
            }
        } else {
            i += 1;
        }
    }
}

fn number_end(b: &[u8], mut i: usize) -> usize {
    if b[i] == b'0' && matches!(b.get(i + 1), Some(b'x') | Some(b'X')) {
        i += 2;
        while i < b.len() && b[i].is_ascii_hexdigit() {
            i += 1;
        }
        return i;
    }
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(sql: &str) -> Vec<Tok> {
        tokenize(sql).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn basics() {
        assert_eq!(
            toks("SELECT a, 'it''s' FROM \"t\" -- c\n WHERE x<=1.5e3"),
            vec![
                Tok::Word("SELECT".into()),
                Tok::Word("a".into()),
                Tok::Op(","),
                Tok::Str("it's".into()),
                Tok::Word("FROM".into()),
                Tok::Quoted {
                    name: "t".into(),
                    dq: true
                },
                Tok::Word("WHERE".into()),
                Tok::Word("x".into()),
                Tok::Op("<="),
                Tok::Num("1.5e3".into()),
                Tok::Eof
            ]
        );
        assert_eq!(toks("x'0aFF'"), vec![Tok::Blob(vec![10, 255]), Tok::Eof]);
        assert_eq!(
            toks("1 /* c */ .5"),
            vec![Tok::Num("1".into()), Tok::Num(".5".into()), Tok::Eof]
        );
        assert!(tokenize("'abc").is_err());
    }
}
