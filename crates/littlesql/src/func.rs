//! Built-in scalar and aggregate functions, with SQLite's semantics.

use crate::error::{Error, Result};
use crate::value::{fmt_real, hash_key, real_to_int, Collation, Value};
use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::HashSet;

pub type ScalarFn = fn(&[Value]) -> Result<Value>;

/// Look up a scalar function. `Ok(None)` means there is no function with
/// that name; an error means the name exists but not with `nargs` arguments.
pub fn lookup_scalar(name: &str, nargs: usize) -> Result<Option<ScalarFn>> {
    // (name, min args, max args, implementation)
    const N: usize = usize::MAX;
    let table: &[(&str, usize, usize, ScalarFn)] = &[
        ("abs", 1, 1, f_abs),
        ("coalesce", 2, N, f_coalesce),
        ("ifnull", 2, 2, f_coalesce),
        ("nullif", 2, 2, f_nullif),
        ("iif", 2, 3, f_iif),
        ("if", 2, 3, f_iif),
        ("length", 1, 1, f_length),
        ("octet_length", 1, 1, f_octet_length),
        ("lower", 1, 1, f_lower),
        ("upper", 1, 1, f_upper),
        ("substr", 2, 3, f_substr),
        ("substring", 2, 3, f_substr),
        ("trim", 1, 2, f_trim),
        ("ltrim", 1, 2, f_ltrim),
        ("rtrim", 1, 2, f_rtrim),
        ("replace", 3, 3, f_replace),
        ("instr", 2, 2, f_instr),
        ("round", 1, 2, f_round),
        ("typeof", 1, 1, f_typeof),
        ("hex", 1, 1, f_hex),
        ("quote", 1, 1, f_quote),
        ("min", 2, N, f_min),
        ("max", 2, N, f_max),
        ("random", 0, 0, f_random),
        ("zeroblob", 1, 1, f_zeroblob),
        ("char", 0, N, f_char),
        ("unicode", 1, 1, f_unicode),
        ("like", 2, 3, f_like),
        ("glob", 2, 2, f_glob),
        ("printf", 1, N, f_printf),
        ("format", 1, N, f_printf),
        ("sign", 1, 1, f_sign),
        ("likely", 1, 1, f_first),
        ("unlikely", 1, 1, f_first),
        ("likelihood", 2, 2, f_first),
        ("concat", 1, N, f_concat),
        ("concat_ws", 2, N, f_concat_ws),
        ("sqlite_version", 0, 0, f_version),
        ("ceil", 1, 1, f_ceil),
        ("ceiling", 1, 1, f_ceil),
        ("floor", 1, 1, f_floor),
        ("trunc", 1, 1, f_trunc),
        ("sqrt", 1, 1, f_sqrt),
        ("exp", 1, 1, f_exp),
        ("ln", 1, 1, f_ln),
        ("log", 1, 2, f_log),
        ("log10", 1, 1, f_log10),
        ("log2", 1, 1, f_log2),
        ("power", 2, 2, f_pow),
        ("pow", 2, 2, f_pow),
        ("mod", 2, 2, f_mod),
        ("pi", 0, 0, f_pi),
        ("sin", 1, 1, f_sin),
        ("cos", 1, 1, f_cos),
        ("tan", 1, 1, f_tan),
        ("asin", 1, 1, f_asin),
        ("acos", 1, 1, f_acos),
        ("atan", 1, 1, f_atan),
        ("atan2", 2, 2, f_atan2),
        ("degrees", 1, 1, f_degrees),
        ("radians", 1, 1, f_radians),
        ("date", 0, N, crate::datetime::f_date),
        ("time", 0, N, crate::datetime::f_time),
        ("datetime", 0, N, crate::datetime::f_datetime),
        ("julianday", 0, N, crate::datetime::f_julianday),
        ("unixepoch", 0, N, crate::datetime::f_unixepoch),
        ("strftime", 1, N, crate::datetime::f_strftime),
        ("current_date", 0, 0, crate::datetime::f_date),
        ("current_time", 0, 0, crate::datetime::f_time),
        ("current_timestamp", 0, 0, crate::datetime::f_datetime),
    ];
    let mut found = false;
    for (n, min, max, f) in table {
        if *n == name {
            found = true;
            if nargs >= *min && nargs <= *max {
                return Ok(Some(*f));
            }
        }
    }
    if found {
        return Err(Error::Sql(format!(
            "wrong number of arguments to function {name}()"
        )));
    }
    Ok(None)
}

fn text_arg(v: &Value) -> String {
    v.to_text()
}

fn f_first(a: &[Value]) -> Result<Value> {
    Ok(a[0].clone())
}

fn f_abs(a: &[Value]) -> Result<Value> {
    Ok(match &a[0] {
        Value::Null => Value::Null,
        Value::Int(i) => Value::Int(
            i.checked_abs()
                .ok_or_else(|| Error::Runtime("integer overflow".into()))?,
        ),
        v => Value::Real(v.to_real().abs()),
    })
}

fn f_coalesce(a: &[Value]) -> Result<Value> {
    Ok(a.iter()
        .find(|v| !v.is_null())
        .cloned()
        .unwrap_or(Value::Null))
}

fn f_nullif(a: &[Value]) -> Result<Value> {
    if !a[0].is_null()
        && !a[1].is_null()
        && a[0].compare(&a[1], Collation::Binary) == Ordering::Equal
    {
        Ok(Value::Null)
    } else {
        Ok(a[0].clone())
    }
}

fn f_iif(a: &[Value]) -> Result<Value> {
    if a[0].truth() == Some(true) {
        Ok(a[1].clone())
    } else {
        Ok(a.get(2).cloned().unwrap_or(Value::Null))
    }
}

fn f_length(a: &[Value]) -> Result<Value> {
    Ok(match &a[0] {
        Value::Null => Value::Null,
        Value::Blob(b) => Value::Int(b.len() as i64),
        Value::Text(s) => Value::Int(s.split('\0').next().unwrap_or("").chars().count() as i64),
        v => Value::Int(v.to_text().chars().count() as i64),
    })
}

fn f_octet_length(a: &[Value]) -> Result<Value> {
    Ok(match &a[0] {
        Value::Null => Value::Null,
        Value::Blob(b) => Value::Int(b.len() as i64),
        v => Value::Int(v.to_text().len() as i64),
    })
}

fn f_lower(a: &[Value]) -> Result<Value> {
    Ok(match &a[0] {
        Value::Null => Value::Null,
        v => Value::text(text_arg(v).to_ascii_lowercase()),
    })
}

fn f_upper(a: &[Value]) -> Result<Value> {
    Ok(match &a[0] {
        Value::Null => Value::Null,
        v => Value::text(text_arg(v).to_ascii_uppercase()),
    })
}

fn f_substr(a: &[Value]) -> Result<Value> {
    if a[0].is_null() || a[1].is_null() || a.get(2).is_some_and(|v| v.is_null()) {
        return Ok(Value::Null);
    }
    let blob = matches!(a[0], Value::Blob(_));
    let chars: Vec<char> = if blob {
        Vec::new()
    } else {
        text_arg(&a[0]).chars().collect()
    };
    let bytes: &[u8] = if let Value::Blob(b) = &a[0] { b } else { &[] };
    let len = if blob { bytes.len() } else { chars.len() } as i64;
    let mut p1 = a[1].to_int();
    let mut p2 = a.get(2).map_or(i64::MAX / 2, |v| v.to_int());
    let neg_p2 = p2 < 0;
    if neg_p2 {
        p2 = p2.saturating_neg();
    }
    if p1 < 0 {
        p1 = p1.saturating_add(len);
        if p1 < 0 {
            p2 = p2.saturating_add(p1).max(0);
            p1 = 0;
        }
    } else if p1 > 0 {
        p1 -= 1;
    } else if p2 > 0 {
        p2 -= 1;
    }
    if neg_p2 {
        p1 -= p2;
        if p1 < 0 {
            p2 += p1;
            p1 = 0;
        }
    }
    let start = p1.clamp(0, len) as usize;
    let end = p1.saturating_add(p2.max(0)).clamp(0, len) as usize;
    Ok(if blob {
        Value::blob(&bytes[start..end])
    } else {
        Value::text(chars[start..end].iter().collect::<String>())
    })
}

fn trim_impl(a: &[Value], left: bool, right: bool) -> Result<Value> {
    if a[0].is_null() {
        return Ok(Value::Null);
    }
    let s = text_arg(&a[0]);
    let set: Vec<char> = match a.get(1) {
        Some(Value::Null) => return Ok(Value::Null),
        Some(v) => text_arg(v).chars().collect(),
        None => vec![' '],
    };
    let mut r: &str = &s;
    if left {
        r = r.trim_start_matches(|c| set.contains(&c));
    }
    if right {
        r = r.trim_end_matches(|c| set.contains(&c));
    }
    Ok(Value::text(r))
}

fn f_trim(a: &[Value]) -> Result<Value> {
    trim_impl(a, true, true)
}

fn f_ltrim(a: &[Value]) -> Result<Value> {
    trim_impl(a, true, false)
}

fn f_rtrim(a: &[Value]) -> Result<Value> {
    trim_impl(a, false, true)
}

fn f_replace(a: &[Value]) -> Result<Value> {
    if a.iter().any(|v| v.is_null()) {
        return Ok(Value::Null);
    }
    let (s, from, to) = (text_arg(&a[0]), text_arg(&a[1]), text_arg(&a[2]));
    if from.is_empty() {
        return Ok(Value::text(s));
    }
    Ok(Value::text(s.replace(&from, &to)))
}

fn f_instr(a: &[Value]) -> Result<Value> {
    if a[0].is_null() || a[1].is_null() {
        return Ok(Value::Null);
    }
    if let (Value::Blob(h), Value::Blob(n)) = (&a[0], &a[1]) {
        if n.is_empty() {
            return Ok(Value::Int(1));
        }
        let pos = h.windows(n.len()).position(|w| w == n.as_ref());
        return Ok(Value::Int(pos.map_or(0, |p| p as i64 + 1)));
    }
    let (h, n) = (text_arg(&a[0]), text_arg(&a[1]));
    Ok(Value::Int(match h.find(&n) {
        Some(byte) => h[..byte].chars().count() as i64 + 1,
        None => 0,
    }))
}

fn f_round(a: &[Value]) -> Result<Value> {
    if a[0].is_null() {
        return Ok(Value::Null);
    }
    let n = match a.get(1) {
        Some(Value::Null) => return Ok(Value::Null),
        Some(v) => v.to_int().clamp(0, 30) as usize,
        None => 0,
    };
    let r = a[0].to_real();
    if !(-4503599627370496.0..=4503599627370496.0).contains(&r) {
        return Ok(Value::Real(r));
    }
    if n == 0 {
        return Ok(Value::Real(
            (r + if r < 0.0 { -0.5 } else { 0.5 }) as i64 as f64,
        ));
    }
    Ok(Value::Real(format!("{r:.n$}").parse().unwrap_or(r)))
}

fn f_typeof(a: &[Value]) -> Result<Value> {
    Ok(Value::text(a[0].type_name()))
}

fn f_hex(a: &[Value]) -> Result<Value> {
    let bytes: Vec<u8> = match &a[0] {
        Value::Blob(b) => b.to_vec(),
        v => v.to_text().into_bytes(),
    };
    Ok(Value::text(
        bytes.iter().map(|b| format!("{b:02X}")).collect::<String>(),
    ))
}

fn f_quote(a: &[Value]) -> Result<Value> {
    Ok(Value::text(match &a[0] {
        Value::Null => "NULL".to_string(),
        Value::Int(i) => i.to_string(),
        Value::Real(r) => fmt_real(*r),
        Value::Text(s) => format!("'{}'", s.replace('\'', "''")),
        Value::Blob(b) => format!(
            "X'{}'",
            b.iter().map(|b| format!("{b:02X}")).collect::<String>()
        ),
    }))
}

fn extreme(a: &[Value], want: Ordering) -> Result<Value> {
    if a.iter().any(|v| v.is_null()) {
        return Ok(Value::Null);
    }
    let mut best = &a[0];
    for v in &a[1..] {
        if v.compare(best, Collation::Binary) == want {
            best = v;
        }
    }
    Ok(best.clone())
}

fn f_min(a: &[Value]) -> Result<Value> {
    extreme(a, Ordering::Less)
}

fn f_max(a: &[Value]) -> Result<Value> {
    extreme(a, Ordering::Greater)
}

thread_local! {
    static RNG: Cell<u64> = const { Cell::new(0x2545_f491_4f6c_dd1d) };
}

fn f_random(_: &[Value]) -> Result<Value> {
    Ok(Value::Int(RNG.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        x as i64
    })))
}

fn f_zeroblob(a: &[Value]) -> Result<Value> {
    let n = a[0].to_int().clamp(0, 1_000_000_000) as usize;
    Ok(Value::blob(vec![0u8; n]))
}

fn f_char(a: &[Value]) -> Result<Value> {
    Ok(Value::text(
        a.iter()
            .map(|v| char::from_u32(v.to_int() as u32).unwrap_or('\u{fffd}'))
            .collect::<String>(),
    ))
}

fn f_unicode(a: &[Value]) -> Result<Value> {
    Ok(match &a[0] {
        Value::Null => Value::Null,
        v => match text_arg(v).chars().next() {
            Some(c) => Value::Int(c as i64),
            None => Value::Null,
        },
    })
}

fn f_like(a: &[Value]) -> Result<Value> {
    // like(pattern, string [, escape])
    if a[0].is_null() || a[1].is_null() {
        return Ok(Value::Null);
    }
    let esc = match a.get(2) {
        Some(Value::Null) => return Ok(Value::Null),
        Some(v) => escape_char(v)?,
        None => None,
    };
    Ok(Value::Int(
        like(&text_arg(&a[0]), &text_arg(&a[1]), esc) as i64
    ))
}

fn f_glob(a: &[Value]) -> Result<Value> {
    if a[0].is_null() || a[1].is_null() {
        return Ok(Value::Null);
    }
    Ok(Value::Int(glob(&text_arg(&a[0]), &text_arg(&a[1])) as i64))
}

fn f_sign(a: &[Value]) -> Result<Value> {
    Ok(match a[0].clone().apply_numeric_for_compare() {
        Value::Int(i) => Value::Int(i.signum()),
        Value::Real(r) => Value::Int(if r > 0.0 {
            1
        } else if r < 0.0 {
            -1
        } else {
            0
        }),
        _ => Value::Null,
    })
}

fn f_concat(a: &[Value]) -> Result<Value> {
    Ok(Value::text(
        a.iter().map(|v| v.to_text()).collect::<String>(),
    ))
}

fn f_concat_ws(a: &[Value]) -> Result<Value> {
    if a[0].is_null() {
        return Ok(Value::Null);
    }
    let sep = text_arg(&a[0]);
    let parts: Vec<String> = a[1..]
        .iter()
        .filter(|v| !v.is_null())
        .map(|v| v.to_text())
        .collect();
    Ok(Value::text(parts.join(&sep)))
}

fn f_version(_: &[Value]) -> Result<Value> {
    Ok(Value::text(concat!(
        "littlesql ",
        env!("CARGO_PKG_VERSION")
    )))
}

fn math1(a: &[Value], f: fn(f64) -> f64) -> Result<Value> {
    Ok(match a[0].clone().apply_numeric_for_compare() {
        Value::Int(i) => finite(f(i as f64)),
        Value::Real(r) => finite(f(r)),
        _ => Value::Null,
    })
}

fn finite(r: f64) -> Value {
    if r.is_nan() {
        Value::Null
    } else {
        Value::Real(r)
    }
}

fn int_preserving(a: &[Value], f: fn(f64) -> f64) -> Result<Value> {
    Ok(match a[0].clone().apply_numeric_for_compare() {
        Value::Int(i) => Value::Int(i),
        Value::Real(r) => Value::Real(f(r)),
        _ => Value::Null,
    })
}

fn f_ceil(a: &[Value]) -> Result<Value> {
    int_preserving(a, f64::ceil)
}

fn f_floor(a: &[Value]) -> Result<Value> {
    int_preserving(a, f64::floor)
}

fn f_trunc(a: &[Value]) -> Result<Value> {
    int_preserving(a, f64::trunc)
}

fn f_sqrt(a: &[Value]) -> Result<Value> {
    math1(a, f64::sqrt)
}

fn f_exp(a: &[Value]) -> Result<Value> {
    math1(a, f64::exp)
}

fn f_ln(a: &[Value]) -> Result<Value> {
    math1(a, |x| if x > 0.0 { x.ln() } else { f64::NAN })
}

fn f_log10(a: &[Value]) -> Result<Value> {
    math1(a, |x| if x > 0.0 { x.log10() } else { f64::NAN })
}

fn f_log2(a: &[Value]) -> Result<Value> {
    math1(a, |x| if x > 0.0 { x.log2() } else { f64::NAN })
}

fn f_log(a: &[Value]) -> Result<Value> {
    if a.len() == 1 {
        return f_log10(a);
    }
    if a.iter().any(|v| v.is_null()) {
        return Ok(Value::Null);
    }
    let (b, x) = (a[0].to_real(), a[1].to_real());
    if b <= 0.0 || x <= 0.0 {
        return Ok(Value::Null);
    }
    Ok(finite(x.ln() / b.ln()))
}

fn f_pow(a: &[Value]) -> Result<Value> {
    if a.iter().any(|v| v.is_null()) {
        return Ok(Value::Null);
    }
    Ok(finite(a[0].to_real().powf(a[1].to_real())))
}

fn f_mod(a: &[Value]) -> Result<Value> {
    if a.iter().any(|v| v.is_null()) {
        return Ok(Value::Null);
    }
    let (x, y) = (a[0].to_real(), a[1].to_real());
    if y == 0.0 {
        return Ok(Value::Null);
    }
    Ok(finite(x % y))
}

fn f_pi(_: &[Value]) -> Result<Value> {
    Ok(Value::Real(std::f64::consts::PI))
}

fn f_sin(a: &[Value]) -> Result<Value> {
    math1(a, f64::sin)
}

fn f_cos(a: &[Value]) -> Result<Value> {
    math1(a, f64::cos)
}

fn f_tan(a: &[Value]) -> Result<Value> {
    math1(a, f64::tan)
}

fn f_asin(a: &[Value]) -> Result<Value> {
    math1(a, f64::asin)
}

fn f_acos(a: &[Value]) -> Result<Value> {
    math1(a, f64::acos)
}

fn f_atan(a: &[Value]) -> Result<Value> {
    math1(a, f64::atan)
}

fn f_atan2(a: &[Value]) -> Result<Value> {
    if a.iter().any(|v| v.is_null()) {
        return Ok(Value::Null);
    }
    Ok(finite(a[0].to_real().atan2(a[1].to_real())))
}

fn f_degrees(a: &[Value]) -> Result<Value> {
    math1(a, f64::to_degrees)
}

fn f_radians(a: &[Value]) -> Result<Value> {
    math1(a, f64::to_radians)
}

/// A subset of SQLite's printf: `%d %i %u %s %f %e %g %x %X %o %c %q %Q %%`
/// with the `-`, `0`, `+` and space flags, a width and a precision.
fn f_printf(a: &[Value]) -> Result<Value> {
    if a[0].is_null() {
        return Ok(Value::Null);
    }
    let fmt: Vec<char> = text_arg(&a[0]).chars().collect();
    let mut out = String::new();
    let mut arg = 1;
    let mut i = 0;
    let null = Value::Null;
    while i < fmt.len() {
        if fmt[i] != '%' {
            out.push(fmt[i]);
            i += 1;
            continue;
        }
        i += 1;
        let (mut left, mut zero, mut plus, mut space) = (false, false, false, false);
        while i < fmt.len() && "-0+ #,!".contains(fmt[i]) {
            match fmt[i] {
                '-' => left = true,
                '0' => zero = true,
                '+' => plus = true,
                ' ' => space = true,
                _ => {}
            }
            i += 1;
        }
        let mut width = 0usize;
        while i < fmt.len() && fmt[i].is_ascii_digit() {
            width = width * 10 + fmt[i] as usize - '0' as usize;
            i += 1;
        }
        let mut prec: Option<usize> = None;
        if i < fmt.len() && fmt[i] == '.' {
            i += 1;
            let mut p = 0;
            while i < fmt.len() && fmt[i].is_ascii_digit() {
                p = p * 10 + fmt[i] as usize - '0' as usize;
                i += 1;
            }
            prec = Some(p);
        }
        while i < fmt.len() && (fmt[i] == 'l' || fmt[i] == 'h') {
            i += 1;
        }
        let Some(&conv) = fmt.get(i) else { break };
        i += 1;
        if conv == '%' {
            out.push('%');
            continue;
        }
        let v = a.get(arg).unwrap_or(&null);
        arg += 1;
        let sign = |neg: bool| {
            if neg {
                "-"
            } else if plus {
                "+"
            } else if space {
                " "
            } else {
                ""
            }
        };
        let body = match conv {
            'd' | 'i' | 'u' => {
                let n = v.to_int();
                format!("{}{}", sign(n < 0), n.unsigned_abs())
            }
            'f' => {
                let r = v.to_real();
                format!("{}{:.*}", sign(r < 0.0), prec.unwrap_or(6), r.abs())
            }
            'e' | 'E' => {
                let r = v.to_real();
                let s = format!("{:.*e}", prec.unwrap_or(6), r.abs());
                let (m, e) = s.split_once('e').unwrap();
                let e: i32 = e.parse().unwrap_or(0);
                format!(
                    "{}{}e{}{:02}",
                    sign(r < 0.0),
                    m,
                    if e < 0 { '-' } else { '+' },
                    e.abs()
                )
            }
            'g' | 'G' => fmt_real(v.to_real()),
            'x' => format!("{:x}", v.to_int()),
            'X' => format!("{:X}", v.to_int()),
            'o' => format!("{:o}", v.to_int()),
            'c' => text_arg(v)
                .chars()
                .next()
                .map(String::from)
                .unwrap_or_default(),
            's' | 'z' => {
                let s = text_arg(v);
                match prec {
                    Some(p) => s.chars().take(p).collect(),
                    None => s,
                }
            }
            'q' => text_arg(v).replace('\'', "''"),
            'Q' => {
                if v.is_null() {
                    "NULL".into()
                } else {
                    format!("'{}'", text_arg(v).replace('\'', "''"))
                }
            }
            other => format!("%{other}"),
        };
        let pad = width.saturating_sub(body.chars().count());
        if left {
            out.push_str(&body);
            out.push_str(&" ".repeat(pad));
        } else if zero && "dieEfgGuxXo".contains(conv) {
            let (s, digits) = match body.strip_prefix(['-', '+', ' ']) {
                Some(rest) => (&body[..1], rest),
                None => ("", body.as_str()),
            };
            out.push_str(s);
            out.push_str(&"0".repeat(pad));
            out.push_str(digits);
        } else {
            out.push_str(&" ".repeat(pad));
            out.push_str(&body);
        }
    }
    Ok(Value::text(out))
}

/// The escape character for LIKE: exactly one character.
pub fn escape_char(v: &Value) -> Result<Option<char>> {
    let s = v.to_text();
    let mut it = s.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Ok(Some(c)),
        _ => Err(Error::Runtime(
            "ESCAPE expression must be a single character".into(),
        )),
    }
}

/// SQL LIKE: `%` matches any run, `_` one character; ASCII case-insensitive.
pub fn like(pattern: &str, s: &str, esc: Option<char>) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = s.chars().collect();
    like_at(&p, &s, esc)
}

fn like_at(p: &[char], s: &[char], esc: Option<char>) -> bool {
    let (mut pi, mut si) = (0, 0);
    while pi < p.len() {
        let c = p[pi];
        if Some(c) == esc && pi + 1 < p.len() {
            if si >= s.len() || s[si] != p[pi + 1] {
                return false;
            }
            pi += 2;
            si += 1;
        } else if c == '%' {
            while pi < p.len() && (p[pi] == '%' || p[pi] == '_') {
                if p[pi] == '_' {
                    if si >= s.len() {
                        return false;
                    }
                    si += 1;
                }
                pi += 1;
            }
            if pi == p.len() {
                return true;
            }
            return (si..=s.len()).any(|k| like_at(&p[pi..], &s[k..], esc));
        } else if c == '_' {
            if si >= s.len() {
                return false;
            }
            pi += 1;
            si += 1;
        } else {
            if si >= s.len() || !c.eq_ignore_ascii_case(&s[si]) {
                return false;
            }
            pi += 1;
            si += 1;
        }
    }
    si == s.len()
}

/// SQL GLOB: `*`, `?` and `[...]` character classes; case-sensitive.
pub fn glob(pattern: &str, s: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = s.chars().collect();
    glob_at(&p, &s)
}

fn glob_at(p: &[char], s: &[char]) -> bool {
    let (mut pi, mut si) = (0, 0);
    while pi < p.len() {
        match p[pi] {
            '*' => {
                while pi < p.len() && p[pi] == '*' {
                    pi += 1;
                }
                if pi == p.len() {
                    return true;
                }
                return (si..=s.len()).any(|k| glob_at(&p[pi..], &s[k..]));
            }
            '?' => {
                if si >= s.len() {
                    return false;
                }
                pi += 1;
                si += 1;
            }
            '[' => {
                if si >= s.len() {
                    return false;
                }
                let c = s[si];
                let mut j = pi + 1;
                let invert = j < p.len() && p[j] == '^';
                if invert {
                    j += 1;
                }
                let mut matched = false;
                let mut first = true;
                while j < p.len() && (p[j] != ']' || first) {
                    first = false;
                    if j + 2 < p.len() && p[j + 1] == '-' && p[j + 2] != ']' {
                        if p[j] <= c && c <= p[j + 2] {
                            matched = true;
                        }
                        j += 3;
                    } else {
                        if p[j] == c {
                            matched = true;
                        }
                        j += 1;
                    }
                }
                if j >= p.len() || matched == invert {
                    return false;
                }
                pi = j + 1;
                si += 1;
            }
            c => {
                if si >= s.len() || s[si] != c {
                    return false;
                }
                pi += 1;
                si += 1;
            }
        }
    }
    si == s.len()
}

// ------------------------------------------------------------------ aggregates

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggFunc {
    CountStar,
    Count,
    Sum,
    Total,
    Avg,
    Min,
    Max,
    GroupConcat,
}

/// Look up an aggregate function by name and argument count.
pub fn lookup_agg(name: &str, nargs: usize, star: bool) -> Option<AggFunc> {
    Some(match (name, nargs) {
        ("count", 0) if star => AggFunc::CountStar,
        ("count", 0) => AggFunc::CountStar,
        ("count", 1) => AggFunc::Count,
        ("sum", 1) => AggFunc::Sum,
        ("total", 1) => AggFunc::Total,
        ("avg", 1) => AggFunc::Avg,
        ("min", 1) => AggFunc::Min,
        ("max", 1) => AggFunc::Max,
        ("group_concat", 1 | 2) | ("string_agg", 2) => AggFunc::GroupConcat,
        _ => return None,
    })
}

/// Kahan–Babuska–Neumaier compensated summation, as SQLite 3.44+ uses for
/// `sum()`, `total()` and `avg()`.
#[derive(Clone, Debug, Default)]
struct Kbn {
    sum: f64,
    err: f64,
}

impl Kbn {
    fn add(&mut self, r: f64) {
        let s = self.sum;
        let t = s + r;
        if s.abs() > r.abs() {
            self.err += (s - t) + r;
        } else {
            self.err += (r - t) + s;
        }
        self.sum = t;
    }

    fn add_int(&mut self, i: i64) {
        if i <= -4503599627370496 || i >= 4503599627370496 {
            let small = i % 16384;
            self.add((i - small) as f64);
            self.add(small as f64);
        } else {
            self.add(i as f64);
        }
    }

    fn value(&self) -> f64 {
        let v = self.sum + self.err;
        if v.is_nan() {
            self.sum
        } else {
            v
        }
    }
}

/// Running state of one aggregate call for one group.
#[derive(Clone, Debug)]
pub struct AggState {
    func: AggFunc,
    count: i64,
    isum: i64,
    kbn: Kbn,
    approx: bool,
    overflow: bool,
    best: Option<Value>,
    text: Option<String>,
    distinct: Option<HashSet<Vec<u8>>>,
    coll: Collation,
}

impl AggState {
    pub fn new(func: AggFunc, distinct: bool, coll: Collation) -> AggState {
        AggState {
            func,
            count: 0,
            isum: 0,
            kbn: Kbn::default(),
            approx: false,
            overflow: false,
            best: None,
            text: None,
            distinct: if distinct { Some(HashSet::new()) } else { None },
            coll,
        }
    }

    /// Feed one row. Returns true when a min/max aggregate found a new
    /// extreme (the caller then remembers that row for bare columns).
    pub fn step(&mut self, args: &[Value]) -> bool {
        if self.func == AggFunc::CountStar {
            self.count += 1;
            return false;
        }
        let v = &args[0];
        if v.is_null() {
            return false;
        }
        if let Some(seen) = &mut self.distinct {
            let mut key = Vec::new();
            hash_key(v, self.coll, &mut key);
            if !seen.insert(key) {
                return false;
            }
        }
        self.count += 1;
        match self.func {
            AggFunc::Sum | AggFunc::Total | AggFunc::Avg => {
                match v.clone().apply_numeric_for_compare() {
                    Value::Int(i) => {
                        if !self.approx {
                            match self.isum.checked_add(i) {
                                Some(s) => self.isum = s,
                                None => {
                                    self.overflow = true;
                                    self.approx = true;
                                    self.kbn = Kbn::default();
                                    self.kbn.add_int(self.isum);
                                    self.kbn.add_int(i);
                                }
                            }
                        } else {
                            self.kbn.add_int(i);
                        }
                    }
                    other => {
                        if !self.approx {
                            self.approx = true;
                            self.kbn = Kbn::default();
                            self.kbn.add_int(self.isum);
                        }
                        self.kbn.add(other.to_real());
                    }
                }
            }
            AggFunc::Min | AggFunc::Max => {
                let want = if self.func == AggFunc::Min {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
                let better = match &self.best {
                    None => true,
                    Some(b) => v.compare(b, self.coll) == want,
                };
                if better {
                    self.best = Some(v.clone());
                    return true;
                }
            }
            AggFunc::GroupConcat => {
                let sep = match args.get(1) {
                    Some(Value::Null) => String::new(),
                    Some(s) => s.to_text(),
                    None => ",".to_string(),
                };
                match &mut self.text {
                    Some(t) => {
                        t.push_str(&sep);
                        t.push_str(&v.to_text());
                    }
                    None => self.text = Some(v.to_text()),
                }
            }
            AggFunc::Count | AggFunc::CountStar => {}
        }
        false
    }

    pub fn finish(&self) -> Result<Value> {
        Ok(match self.func {
            AggFunc::Count | AggFunc::CountStar => Value::Int(self.count),
            AggFunc::Sum => {
                if self.count == 0 {
                    Value::Null
                } else if self.approx {
                    if self.overflow {
                        return Err(Error::Runtime("integer overflow".into()));
                    }
                    Value::Real(self.kbn.value())
                } else {
                    Value::Int(self.isum)
                }
            }
            AggFunc::Total => Value::Real(if self.approx {
                self.kbn.value()
            } else {
                self.isum as f64
            }),
            AggFunc::Avg => {
                if self.count == 0 {
                    Value::Null
                } else {
                    let s = if self.approx {
                        self.kbn.value()
                    } else {
                        self.isum as f64
                    };
                    Value::Real(s / self.count as f64)
                }
            }
            AggFunc::Min | AggFunc::Max => self.best.clone().unwrap_or(Value::Null),
            AggFunc::GroupConcat => self.text.as_ref().map(Value::text).unwrap_or(Value::Null),
        })
    }
}

/// `CAST(real AS INTEGER)` re-exported for the executor.
pub fn cast_real_to_int(r: f64) -> i64 {
    real_to_int(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_and_glob() {
        assert!(like("a%c", "ABBBC", None));
        assert!(like("_b%", "abc", None));
        assert!(!like("a_c", "abbc", None));
        assert!(like("10!%", "10%", Some('!')));
        assert!(!like("10!%", "100", Some('!')));
        assert!(like("%", "", None));
        assert!(glob("a*c", "abbbc"));
        assert!(!glob("a*c", "ABC"));
        assert!(glob("[a-c]?[^x]", "bzz"));
        assert!(!glob("[a-c]?[^x]", "bzx"));
    }

    #[test]
    fn substr_matches_sqlite() {
        let s = |a: &[Value]| f_substr(a).unwrap().to_text();
        let h = Value::text("hello");
        assert_eq!(s(&[h.clone(), Value::Int(2), Value::Int(3)]), "ell");
        assert_eq!(s(&[h.clone(), Value::Int(-3)]), "llo");
        assert_eq!(s(&[h.clone(), Value::Int(0), Value::Int(2)]), "h");
        assert_eq!(s(&[h.clone(), Value::Int(3), Value::Int(-2)]), "he");
        assert_eq!(s(&[h, Value::Int(10)]), "");
    }

    #[test]
    fn sum_semantics() {
        let mut s = AggState::new(AggFunc::Sum, false, Collation::Binary);
        assert!(s.finish().unwrap().is_null());
        s.step(&[Value::Int(1)]);
        s.step(&[Value::Null]);
        s.step(&[Value::Int(2)]);
        assert!(matches!(s.finish().unwrap(), Value::Int(3)));
        s.step(&[Value::Real(0.5)]);
        assert!(matches!(s.finish().unwrap(), Value::Real(r) if r == 3.5));
        let mut o = AggState::new(AggFunc::Sum, false, Collation::Binary);
        o.step(&[Value::Int(i64::MAX)]);
        o.step(&[Value::Int(1)]);
        assert!(o.finish().is_err());
    }

    #[test]
    fn printf_subset() {
        let p = |a: &[Value]| f_printf(a).unwrap().to_text();
        assert_eq!(
            p(&[
                Value::text("%05d|%-4s|%.2f|%x"),
                Value::Int(42),
                Value::text("ab"),
                Value::Real(3.14159),
                Value::Int(255)
            ]),
            "00042|ab  |3.14|ff"
        );
    }
}
