//! SQL values and SQLite's rules for comparing and converting them.
//!
//! SQLite is dynamically typed: every value carries its own storage class
//! (NULL, INTEGER, REAL, TEXT or BLOB) and columns only have an *affinity*, a
//! preferred class that values are nudged towards when stored or compared.
//! The rules implemented here follow <https://www.sqlite.org/datatype3.html>.

use std::cmp::Ordering;
use std::rc::Rc;

/// A single SQL value.
#[derive(Clone, Debug)]
pub enum Value {
    Null,
    Int(i64),
    Real(f64),
    Text(Rc<str>),
    Blob(Rc<[u8]>),
}

/// Column affinity (section 3 of datatype3.html).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Affinity {
    /// No preference; called BLOB affinity (historically NONE).
    Blob,
    Text,
    Numeric,
    Integer,
    Real,
}

impl Affinity {
    /// Derive the affinity from a declared column type using SQLite's five
    /// substring rules, in order.
    pub fn from_decl_type(decl: &str) -> Affinity {
        let t = decl.to_ascii_uppercase();
        if t.contains("INT") {
            Affinity::Integer
        } else if t.contains("CHAR") || t.contains("CLOB") || t.contains("TEXT") {
            Affinity::Text
        } else if t.contains("BLOB") || t.trim().is_empty() {
            Affinity::Blob
        } else if t.contains("REAL") || t.contains("FLOA") || t.contains("DOUB") {
            Affinity::Real
        } else {
            Affinity::Numeric
        }
    }

    pub fn is_numeric(self) -> bool {
        matches!(self, Affinity::Numeric | Affinity::Integer | Affinity::Real)
    }
}

/// The three built-in collating sequences.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Collation {
    Binary,
    NoCase,
    RTrim,
}

impl Collation {
    pub fn from_name(name: &str) -> Option<Collation> {
        match name.to_ascii_uppercase().as_str() {
            "BINARY" => Some(Collation::Binary),
            "NOCASE" => Some(Collation::NoCase),
            "RTRIM" => Some(Collation::RTrim),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Collation::Binary => "BINARY",
            Collation::NoCase => "NOCASE",
            Collation::RTrim => "RTRIM",
        }
    }

    pub fn compare(self, a: &str, b: &str) -> Ordering {
        match self {
            Collation::Binary => a.as_bytes().cmp(b.as_bytes()),
            Collation::NoCase => {
                let x = a.bytes().map(|c| c.to_ascii_lowercase());
                let y = b.bytes().map(|c| c.to_ascii_lowercase());
                x.cmp(y)
            }
            Collation::RTrim => a
                .trim_end_matches(' ')
                .as_bytes()
                .cmp(b.trim_end_matches(' ').as_bytes()),
        }
    }
}

/// Compare an integer with a float without losing precision, the way
/// SQLite's `sqlite3IntFloatCompare` does.
pub fn cmp_int_real(i: i64, r: f64) -> Ordering {
    if r.is_nan() {
        return Ordering::Greater;
    }
    if r < -9223372036854775808.0 {
        return Ordering::Greater;
    }
    if r >= 9223372036854775808.0 {
        return Ordering::Less;
    }
    let y = r as i64;
    match i.cmp(&y) {
        Ordering::Equal => {
            let s = i as f64;
            s.partial_cmp(&r).unwrap_or(Ordering::Equal)
        }
        o => o,
    }
}

impl Value {
    pub fn text(s: impl AsRef<str>) -> Value {
        Value::Text(Rc::from(s.as_ref()))
    }

    pub fn blob(b: impl AsRef<[u8]>) -> Value {
        Value::Blob(Rc::from(b.as_ref()))
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Rank of the storage class in SQLite's sort order.
    fn class(&self) -> u8 {
        match self {
            Value::Null => 0,
            Value::Int(_) | Value::Real(_) => 1,
            Value::Text(_) => 2,
            Value::Blob(_) => 3,
        }
    }

    /// The name `typeof()` reports.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Int(_) => "integer",
            Value::Real(_) => "real",
            Value::Text(_) => "text",
            Value::Blob(_) => "blob",
        }
    }

    /// Total order used by ORDER BY, indexes, DISTINCT and comparisons:
    /// NULL < numbers < text < blobs. Text uses `coll`.
    pub fn compare(&self, other: &Value, coll: Collation) -> Ordering {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Int(a), Value::Real(b)) => cmp_int_real(*a, *b),
            (Value::Real(a), Value::Int(b)) => cmp_int_real(*b, *a).reverse(),
            (Value::Real(a), Value::Real(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            (Value::Text(a), Value::Text(b)) => coll.compare(a, b),
            (Value::Blob(a), Value::Blob(b)) => a.as_ref().cmp(b.as_ref()),
            _ => self.class().cmp(&other.class()),
        }
    }

    /// SQL truth value: NULL is unknown, numbers are true when non-zero and
    /// text/blobs are converted to numbers first.
    pub fn truth(&self) -> Option<bool> {
        match self {
            Value::Null => None,
            Value::Int(i) => Some(*i != 0),
            Value::Real(r) => Some(*r != 0.0),
            Value::Text(_) | Value::Blob(_) => Some(self.to_real() != 0.0),
        }
    }

    /// Numeric interpretation used by arithmetic: text and blobs contribute
    /// their longest numeric prefix (0 if there is none).
    pub fn to_numeric(&self) -> Value {
        match self {
            Value::Null => Value::Null,
            Value::Int(_) | Value::Real(_) => self.clone(),
            Value::Text(s) => numeric_prefix(s).0,
            Value::Blob(b) => numeric_prefix(&String::from_utf8_lossy(b)).0,
        }
    }

    /// `CAST(x AS REAL)` for non-NULL values.
    pub fn to_real(&self) -> f64 {
        match self.to_numeric() {
            Value::Int(i) => i as f64,
            Value::Real(r) => r,
            _ => 0.0,
        }
    }

    /// `CAST(x AS INTEGER)` for non-NULL values: reals truncate towards zero
    /// (saturating) and text contributes its integer prefix.
    pub fn to_int(&self) -> i64 {
        match self {
            Value::Null => 0,
            Value::Int(i) => *i,
            Value::Real(r) => real_to_int(*r),
            Value::Text(s) => integer_prefix(s),
            Value::Blob(b) => integer_prefix(&String::from_utf8_lossy(b)),
        }
    }

    /// Text rendering used by `CAST(x AS TEXT)`, `||` and result display.
    /// NULL renders as the empty string; callers that care check first.
    pub fn to_text(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Int(i) => i.to_string(),
            Value::Real(r) => fmt_real(*r),
            Value::Text(s) => s.to_string(),
            Value::Blob(b) => String::from_utf8_lossy(b).into_owned(),
        }
    }

    /// Apply a column affinity, as done when storing a value and before
    /// comparing against a column with that affinity.
    pub fn apply_affinity(self, aff: Affinity) -> Value {
        match aff {
            Affinity::Blob => self,
            Affinity::Text => match self {
                Value::Int(_) | Value::Real(_) => Value::text(self.to_text()),
                v => v,
            },
            Affinity::Numeric | Affinity::Integer | Affinity::Real => {
                let v = match self {
                    Value::Text(ref s) => match numeric_prefix(s) {
                        (n, true) => n,
                        _ => return self,
                    },
                    v => v,
                };
                match (aff, v) {
                    (Affinity::Real, Value::Int(i)) => Value::Real(i as f64),
                    (Affinity::Real, v) => v,
                    (_, Value::Real(r)) => real_to_int_if_exact(r),
                    (_, v) => v,
                }
            }
        }
    }

    /// Numeric affinity as applied to an operand of a comparison: text that
    /// is a well-formed number becomes that number; nothing else changes.
    pub fn apply_numeric_for_compare(self) -> Value {
        match self {
            Value::Text(ref s) => match numeric_prefix(s) {
                (Value::Real(r), true) => real_to_int_if_exact(r),
                (n, true) => n,
                _ => self,
            },
            v => v,
        }
    }
}

/// Convert a real to an integer when the conversion is exact; used by
/// NUMERIC affinity so that `3.0` is stored as `3`.
pub fn real_to_int_if_exact(r: f64) -> Value {
    if r.is_finite() && r > -9223372036854775808.0 && r < 9223372036854775808.0 {
        let i = r as i64;
        if i as f64 == r && i > i64::MIN && i < i64::MAX {
            return Value::Int(i);
        }
    }
    Value::Real(r)
}

/// Truncate towards zero with saturation, as `CAST(real AS INTEGER)` does.
pub fn real_to_int(r: f64) -> i64 {
    if r.is_nan() {
        0
    } else if r <= -9223372036854775808.0 {
        i64::MIN
    } else if r >= 9223372036854775808.0 {
        i64::MAX
    } else {
        r as i64
    }
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// Parse the longest numeric prefix of `s` (after leading whitespace).
/// Returns the number and whether the *whole* string (ignoring surrounding
/// whitespace) was a well-formed number. With no numeric prefix the result
/// is `(Int(0), false)`.
pub fn numeric_prefix(s: &str) -> (Value, bool) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && is_space(b[i]) {
        i += 1;
    }
    let start = i;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let int_digits = i - int_start;
    let mut is_real = false;
    let mut frac_digits = 0;
    if i < b.len() && b[i] == b'.' {
        let mut j = i + 1;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        frac_digits = j - i - 1;
        if int_digits + frac_digits > 0 {
            is_real = true;
            i = j;
        }
    }
    if int_digits + frac_digits == 0 {
        return (Value::Int(0), false);
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'-' || b[j] == b'+') {
            j += 1;
        }
        let exp_start = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > exp_start {
            is_real = true;
            i = j;
        }
    }
    let end = i;
    while i < b.len() && is_space(b[i]) {
        i += 1;
    }
    let full = i == b.len();
    let text = &s[start..end];
    if !is_real {
        if let Ok(v) = text.parse::<i64>() {
            return (Value::Int(v), full);
        }
    }
    let text = text.strip_prefix('+').unwrap_or(text);
    (Value::Real(text.parse::<f64>().unwrap_or(0.0)), full)
}

/// Parse the longest integer prefix, saturating on overflow.
pub fn integer_prefix(s: &str) -> i64 {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && is_space(b[i]) {
        i += 1;
    }
    let mut neg = false;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        neg = b[i] == b'-';
        i += 1;
    }
    let mut acc: i128 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        acc = acc * 10 + (b[i] - b'0') as i128;
        if acc > i64::MAX as i128 + 1 {
            acc = i64::MAX as i128 + 1;
        }
        i += 1;
    }
    if neg {
        (-acc).max(i64::MIN as i128) as i64
    } else {
        acc.min(i64::MAX as i128) as i64
    }
}

/// Render a float the way SQLite's `%!.15g` does: 15 significant digits,
/// trailing zeros removed, always with a decimal point.
pub fn fmt_real(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Inf".into() } else { "-Inf".into() };
    }
    let s = format!("{:.14e}", f.abs());
    let (mant, exp) = s.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let mut digits: String = mant.chars().filter(|c| *c != '.').collect();
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    let mut out = String::new();
    if f.is_sign_negative() && f != 0.0 {
        out.push('-');
    }
    if !(-4..15).contains(&exp) {
        out.push_str(&digits[..1]);
        out.push('.');
        if digits.len() > 1 {
            out.push_str(&digits[1..]);
        } else {
            out.push('0');
        }
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", exp.abs()));
    } else if exp >= 0 {
        let int_len = exp as usize + 1;
        if digits.len() <= int_len {
            out.push_str(&digits);
            out.push_str(&"0".repeat(int_len - digits.len()));
            out.push_str(".0");
        } else {
            out.push_str(&digits[..int_len]);
            out.push('.');
            out.push_str(&digits[int_len..]);
        }
    } else {
        out.push_str("0.");
        out.push_str(&"0".repeat((-exp - 1) as usize));
        out.push_str(&digits);
    }
    out
}

/// Append a canonical byte encoding of `v` to `out` such that two values
/// compare equal (under `coll`) exactly when their encodings are equal.
/// Used as the key for hash joins, DISTINCT, GROUP BY and IN sets.
pub fn hash_key(v: &Value, coll: Collation, out: &mut Vec<u8>) {
    match v {
        Value::Null => out.push(0),
        Value::Int(i) => {
            out.push(1);
            out.extend_from_slice(&i.to_be_bytes());
        }
        Value::Real(r) => match real_to_int_if_exact(*r) {
            Value::Int(i) => {
                out.push(1);
                out.extend_from_slice(&i.to_be_bytes());
            }
            _ => {
                out.push(2);
                out.extend_from_slice(&r.to_bits().to_be_bytes());
            }
        },
        Value::Text(s) => {
            out.push(3);
            let t: &str = if coll == Collation::RTrim {
                s.trim_end_matches(' ')
            } else {
                s
            };
            out.extend_from_slice(&(t.len() as u32).to_be_bytes());
            if coll == Collation::NoCase {
                out.extend(t.bytes().map(|c| c.to_ascii_lowercase()));
            } else {
                out.extend_from_slice(t.as_bytes());
            }
        }
        Value::Blob(b) => {
            out.push(4);
            out.extend_from_slice(&(b.len() as u32).to_be_bytes());
            out.extend_from_slice(b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_formatting_matches_sqlite() {
        assert_eq!(fmt_real(0.0), "0.0");
        assert_eq!(fmt_real(1.0), "1.0");
        assert_eq!(fmt_real(-2.5), "-2.5");
        assert_eq!(fmt_real(1e15), "1.0e+15");
        assert_eq!(fmt_real(123456789012345.0), "123456789012345.0");
        assert_eq!(fmt_real(0.0001), "0.0001");
        assert_eq!(fmt_real(0.00001), "1.0e-05");
        assert_eq!(fmt_real(123456789012345678.0), "1.23456789012346e+17");
        assert_eq!(fmt_real(1.0 / 3.0), "0.333333333333333");
        assert_eq!(fmt_real(1e100), "1.0e+100");
    }

    #[test]
    fn numeric_prefixes() {
        assert!(matches!(numeric_prefix("12"), (Value::Int(12), true)));
        assert!(matches!(numeric_prefix(" 12abc"), (Value::Int(12), false)));
        assert!(matches!(numeric_prefix("abc"), (Value::Int(0), false)));
        assert!(matches!(numeric_prefix("1e2"), (Value::Real(r), true) if r == 100.0));
        assert!(matches!(numeric_prefix(".5 "), (Value::Real(r), true) if r == 0.5));
        assert!(matches!(numeric_prefix("1e"), (Value::Int(1), false)));
        assert_eq!(integer_prefix("1e3"), 1);
        assert_eq!(integer_prefix("-99999999999999999999"), i64::MIN);
    }

    #[test]
    fn ordering_across_classes() {
        let c = Collation::Binary;
        assert_eq!(Value::Null.compare(&Value::Int(0), c), Ordering::Less);
        assert_eq!(Value::Int(5).compare(&Value::text("1"), c), Ordering::Less);
        assert_eq!(
            Value::text("z").compare(&Value::blob([0u8]), c),
            Ordering::Less
        );
        assert_eq!(Value::Int(1).compare(&Value::Real(1.0), c), Ordering::Equal);
        assert_eq!(
            Value::Int(9007199254740993).compare(&Value::Real(9007199254740992.0), c),
            Ordering::Greater
        );
        assert_eq!(
            Value::text("ABC").compare(&Value::text("abc"), Collation::NoCase),
            Ordering::Equal
        );
    }

    #[test]
    fn affinity() {
        assert_eq!(Affinity::from_decl_type("VARCHAR(30)"), Affinity::Text);
        assert_eq!(Affinity::from_decl_type("BIGINT"), Affinity::Integer);
        assert_eq!(Affinity::from_decl_type("FLOAT"), Affinity::Real);
        assert_eq!(Affinity::from_decl_type(""), Affinity::Blob);
        assert_eq!(Affinity::from_decl_type("DECIMAL(10,2)"), Affinity::Numeric);
        assert!(matches!(
            Value::text("3.0").apply_affinity(Affinity::Numeric),
            Value::Int(3)
        ));
        assert!(matches!(
            Value::text("x").apply_affinity(Affinity::Integer),
            Value::Text(_)
        ));
        assert!(matches!(
            Value::Int(3).apply_affinity(Affinity::Text),
            Value::Text(_)
        ));
        assert!(matches!(
            Value::Int(3).apply_affinity(Affinity::Real),
            Value::Real(_)
        ));
    }
}
