//! SQLite's date and time functions: `date`, `time`, `datetime`,
//! `julianday`, `unixepoch` and `strftime`, with the common modifiers.
//!
//! Like SQLite, a point in time is held as milliseconds since noon UTC on
//! 24 November 4714 BC (the Julian day epoch).

use crate::error::Result;
use crate::value::{numeric_prefix, Value};
use std::cell::Cell;

const DAY_MS: i64 = 86_400_000;
const UNIX_EPOCH_JD_MS: i64 = 210_866_760_000_000;

thread_local! {
    static NOW_OVERRIDE: Cell<Option<i64>> = const { Cell::new(None) };
}

/// Fix the clock used by `'now'` (milliseconds since the Unix epoch).
/// WebAssembly has no clock of its own, so the host supplies one.
pub fn set_now_unix_ms(ms: Option<i64>) {
    NOW_OVERRIDE.with(|c| c.set(ms));
}

fn now_jd_ms() -> i64 {
    if let Some(ms) = NOW_OVERRIDE.with(|c| c.get()) {
        return ms + UNIX_EPOCH_JD_MS;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
            + UNIX_EPOCH_JD_MS
    }
    #[cfg(target_arch = "wasm32")]
    {
        UNIX_EPOCH_JD_MS
    }
}

fn ymd_to_jd_ms(y: i64, m: i64, d: i64) -> i64 {
    let (mut y, mut m) = (y, m);
    if m <= 2 {
        y -= 1;
        m += 12;
    }
    let a = y / 100;
    let b = 2 - a + a / 4;
    let x1 = 36525 * (y + 4716) / 100;
    let x2 = 306001 * (m + 1) / 10000;
    ((x1 + x2 + d + b) as f64 - 1524.5).mul_add(DAY_MS as f64, 0.0) as i64
}

fn jd_ms_to_ymd(jd: i64) -> (i64, i64, i64) {
    let z = (jd + 43_200_000) / DAY_MS;
    let a = ((z as f64 - 1867216.25) / 36524.25) as i64;
    let a = z + 1 + a - a / 4;
    let b = a + 1524;
    let c = ((b as f64 - 122.1) / 365.25) as i64;
    let d = (36525 * (c & 32767)) / 100;
    let e = ((b - d) as f64 / 30.6001) as i64;
    let x1 = (30.6001 * e as f64) as i64;
    let day = b - d - x1;
    let month = if e < 14 { e - 1 } else { e - 13 };
    let year = if month > 2 { c - 4716 } else { c - 4715 };
    (year, month, day)
}

fn ms_of_day(jd: i64) -> i64 {
    (jd + 43_200_000).rem_euclid(DAY_MS)
}

fn digits(s: &[u8], n: usize) -> Option<i64> {
    if s.len() < n || !s[..n].iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(s[..n].iter().fold(0, |a, c| a * 10 + (c - b'0') as i64))
}

/// Parse `HH:MM[:SS[.fff]]` followed by an optional time zone.
fn parse_time(s: &[u8]) -> Option<i64> {
    let h = digits(s, 2)?;
    if s.get(2) != Some(&b':') {
        return None;
    }
    let m = digits(&s[3..], 2)?;
    let mut ms = (h * 3600 + m * 60) * 1000;
    let mut i = 5;
    if s.get(i) == Some(&b':') {
        let sec = digits(&s[i + 1..], 2)?;
        ms += sec * 1000;
        i += 3;
        if s.get(i) == Some(&b'.') {
            i += 1;
            let mut scale = 100.0;
            let mut frac = 0.0;
            while i < s.len() && s[i].is_ascii_digit() {
                frac += (s[i] - b'0') as f64 * scale;
                scale /= 10.0;
                i += 1;
            }
            ms += frac.round() as i64;
        }
    }
    if h > 24 || m > 59 {
        return None;
    }
    while i < s.len() && s[i] == b' ' {
        i += 1;
    }
    if i < s.len() {
        if s[i] == b'Z' || s[i] == b'z' {
            i += 1;
        } else if s[i] == b'+' || s[i] == b'-' {
            let sign = if s[i] == b'-' { 1 } else { -1 };
            let th = digits(&s[i + 1..], 2)?;
            if s.get(i + 3) != Some(&b':') {
                return None;
            }
            let tm = digits(&s[i + 4..], 2)?;
            ms += sign * (th * 60 + tm) * 60_000;
            i += 6;
        }
        while i < s.len() && s[i] == b' ' {
            i += 1;
        }
        if i < s.len() {
            return None;
        }
    }
    Some(ms)
}

/// Parse a time value: a date, a date and time, a time, `now`, or a number
/// (a Julian day).
fn parse_timevalue(v: &Value) -> Option<(i64, bool)> {
    match v {
        Value::Null => None,
        Value::Int(i) => Some(((*i as f64 * DAY_MS as f64) as i64, true)),
        Value::Real(r) => Some(((*r * DAY_MS as f64).round() as i64, true)),
        _ => {
            let text = v.to_text();
            let t = text.trim();
            if t.eq_ignore_ascii_case("now") {
                return Some((now_jd_ms(), false));
            }
            let b = t.as_bytes();
            if let (Some(y), Some(b'-')) = (digits(b, 4), b.get(4)) {
                let m = digits(&b[5..], 2)?;
                if b.get(7) != Some(&b'-') {
                    return None;
                }
                let d = digits(&b[8..], 2)?;
                if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
                    return None;
                }
                let mut jd = ymd_to_jd_ms(y, m, d);
                if b.len() > 10 {
                    if b[10] != b' ' && b[10] != b'T' {
                        return None;
                    }
                    let rest = t[11..].trim_start();
                    if !rest.is_empty() {
                        jd += parse_time(rest.as_bytes())?;
                    }
                }
                return Some((jd, false));
            }
            if b.len() >= 5 && b[2] == b':' {
                return Some((ymd_to_jd_ms(2000, 1, 1) + parse_time(b)?, false));
            }
            match numeric_prefix(t) {
                (Value::Int(i), true) => Some(((i as f64 * DAY_MS as f64) as i64, true)),
                (Value::Real(r), true) => Some(((r * DAY_MS as f64).round() as i64, true)),
                _ => None,
            }
        }
    }
}

struct Moment {
    jd: i64,
    subsec: bool,
}

fn apply_modifier(m: &mut Moment, raw_numeric: Option<f64>, text: &str) -> Option<()> {
    let t = text.trim().to_ascii_lowercase();
    match t.as_str() {
        "localtime" | "utc" | "auto" => return Some(()),
        "subsec" | "subsecond" => {
            m.subsec = true;
            return Some(());
        }
        "unixepoch" => {
            let r = raw_numeric?;
            m.jd = (r * 1000.0).round() as i64 + UNIX_EPOCH_JD_MS;
            return Some(());
        }
        "julianday" => {
            raw_numeric?;
            return Some(());
        }
        "start of day" => {
            m.jd -= ms_of_day(m.jd);
            return Some(());
        }
        "start of month" => {
            let (y, mo, _) = jd_ms_to_ymd(m.jd);
            m.jd = ymd_to_jd_ms(y, mo, 1);
            return Some(());
        }
        "start of year" => {
            let (y, _, _) = jd_ms_to_ymd(m.jd);
            m.jd = ymd_to_jd_ms(y, 1, 1);
            return Some(());
        }
        _ => {}
    }
    if let Some(rest) = t.strip_prefix("weekday ") {
        let want: i64 = rest.trim().parse().ok()?;
        if !(0..=6).contains(&want) {
            return None;
        }
        let dow = ((m.jd + 129_600_000) / DAY_MS).rem_euclid(7);
        let delta = (want - dow).rem_euclid(7);
        m.jd += delta * DAY_MS;
        return Some(());
    }
    // "+N unit" / "-N unit"
    let (num, unit) = t.split_once(' ')?;
    let n: f64 = num.strip_prefix('+').unwrap_or(num).parse().ok()?;
    let unit = unit.trim().trim_end_matches('s');
    match unit {
        "day" => m.jd += (n * DAY_MS as f64).round() as i64,
        "hour" => m.jd += (n * 3_600_000.0).round() as i64,
        "minute" => m.jd += (n * 60_000.0).round() as i64,
        "second" => m.jd += (n * 1000.0).round() as i64,
        "month" | "year" => {
            let (y, mo, d) = jd_ms_to_ymd(m.jd);
            let tod = ms_of_day(m.jd);
            let whole = n.trunc() as i64;
            let (ny, nm) = if unit == "month" {
                let total = y * 12 + (mo - 1) + whole;
                (total.div_euclid(12), total.rem_euclid(12) + 1)
            } else {
                (y + whole, mo)
            };
            m.jd = ymd_to_jd_ms(ny, nm, d) + tod;
            let frac = n - n.trunc();
            if frac != 0.0 {
                let span = if unit == "month" { 30.0 } else { 365.0 };
                m.jd += (frac * span * DAY_MS as f64).round() as i64;
            }
        }
        _ => return None,
    }
    Some(())
}

fn moment(args: &[Value]) -> Option<Moment> {
    if args.is_empty() {
        return Some(Moment {
            jd: now_jd_ms(),
            subsec: false,
        });
    }
    let (jd, numeric) = parse_timevalue(&args[0])?;
    let raw = if numeric {
        Some(args[0].to_real())
    } else {
        None
    };
    let mut m = Moment { jd, subsec: false };
    for a in &args[1..] {
        if a.is_null() {
            return None;
        }
        apply_modifier(&mut m, raw, &a.to_text())?;
    }
    // Valid range: years 0000 to 9999.
    if m.jd < 148_699_540_800_000 - 366 * DAY_MS || m.jd > 464_269_060_799_999 {
        return None;
    }
    Some(m)
}

fn fmt_date(jd: i64) -> String {
    let (y, m, d) = jd_ms_to_ymd(jd);
    format!("{y:04}-{m:02}-{d:02}")
}

fn fmt_time(jd: i64, subsec: bool) -> String {
    let ms = ms_of_day(jd);
    let (h, mi, s) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60);
    if subsec {
        format!("{h:02}:{mi:02}:{s:02}.{:03}", ms % 1000)
    } else {
        format!("{h:02}:{mi:02}:{s:02}")
    }
}

pub fn f_date(a: &[Value]) -> Result<Value> {
    Ok(moment(a).map_or(Value::Null, |m| Value::text(fmt_date(m.jd))))
}

pub fn f_time(a: &[Value]) -> Result<Value> {
    Ok(moment(a).map_or(Value::Null, |m| Value::text(fmt_time(m.jd, m.subsec))))
}

pub fn f_datetime(a: &[Value]) -> Result<Value> {
    Ok(moment(a).map_or(Value::Null, |m| {
        Value::text(format!("{} {}", fmt_date(m.jd), fmt_time(m.jd, m.subsec)))
    }))
}

pub fn f_julianday(a: &[Value]) -> Result<Value> {
    Ok(moment(a).map_or(Value::Null, |m| Value::Real(m.jd as f64 / DAY_MS as f64)))
}

pub fn f_unixepoch(a: &[Value]) -> Result<Value> {
    Ok(moment(a).map_or(Value::Null, |m| {
        if m.subsec {
            Value::Real((m.jd - UNIX_EPOCH_JD_MS) as f64 / 1000.0)
        } else {
            Value::Int((m.jd - UNIX_EPOCH_JD_MS).div_euclid(1000))
        }
    }))
}

pub fn f_strftime(a: &[Value]) -> Result<Value> {
    if a[0].is_null() {
        return Ok(Value::Null);
    }
    let Some(m) = moment(&a[1..]) else {
        return Ok(Value::Null);
    };
    let fmt = a[0].to_text();
    let (y, mo, d) = jd_ms_to_ymd(m.jd);
    let ms = ms_of_day(m.jd);
    let (h, mi, s) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60);
    let day_of_year = (m.jd - ymd_to_jd_ms(y, 1, 1)) / DAY_MS + 1;
    let dow = ((m.jd + 129_600_000) / DAY_MS).rem_euclid(7);
    let mut out = String::new();
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('d') => out.push_str(&format!("{d:02}")),
            Some('e') => out.push_str(&format!("{d:2}")),
            Some('f') => out.push_str(&format!("{s:02}.{:03}", ms % 1000)),
            Some('F') => out.push_str(&fmt_date(m.jd)),
            Some('H') => out.push_str(&format!("{h:02}")),
            Some('I') => out.push_str(&format!("{:02}", if h % 12 == 0 { 12 } else { h % 12 })),
            Some('j') => out.push_str(&format!("{day_of_year:03}")),
            Some('J') => out.push_str(&crate::value::fmt_real(m.jd as f64 / DAY_MS as f64)),
            Some('k') => out.push_str(&format!("{h:2}")),
            Some('l') => out.push_str(&format!("{:2}", if h % 12 == 0 { 12 } else { h % 12 })),
            Some('m') => out.push_str(&format!("{mo:02}")),
            Some('M') => out.push_str(&format!("{mi:02}")),
            Some('p') => out.push_str(if h < 12 { "AM" } else { "PM" }),
            Some('P') => out.push_str(if h < 12 { "am" } else { "pm" }),
            Some('R') => out.push_str(&format!("{h:02}:{mi:02}")),
            Some('s') => out.push_str(&(m.jd - UNIX_EPOCH_JD_MS).div_euclid(1000).to_string()),
            Some('S') => out.push_str(&format!("{s:02}")),
            Some('T') => out.push_str(&fmt_time(m.jd, false)),
            Some('u') => out.push_str(&(if dow == 0 { 7 } else { dow }).to_string()),
            Some('w') => out.push_str(&dow.to_string()),
            Some('W') => out.push_str(&format!("{:02}", (day_of_year + 6 - (dow + 6) % 7) / 7)),
            Some('Y') => out.push_str(&format!("{y:04}")),
            Some('%') => out.push('%'),
            _ => return Ok(Value::Null),
        }
    }
    Ok(Value::text(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(f: fn(&[Value]) -> Result<Value>, args: &[&str]) -> String {
        let v: Vec<Value> = args.iter().map(Value::text).collect();
        f(&v).unwrap().to_text()
    }

    #[test]
    fn formats_and_modifiers() {
        assert_eq!(t(f_date, &["2024-02-28", "+2 days"]), "2024-03-01");
        assert_eq!(
            t(f_datetime, &["2024-01-31 10:30:00", "+1 month"]),
            "2024-03-02 10:30:00"
        );
        assert_eq!(
            t(f_date, &["2024-05-17", "start of month", "-1 day"]),
            "2024-04-30"
        );
        assert_eq!(t(f_time, &["12:34:56.789"]), "12:34:56");
        assert_eq!(
            t(f_strftime, &["%Y/%m/%d %H:%M %j %w", "2023-12-31T23:59:00"]),
            "2023/12/31 23:59 365 0"
        );
        assert_eq!(t(f_date, &["2024-03-15", "weekday 0"]), "2024-03-17");
        assert_eq!(
            f_unixepoch(&[Value::text("1970-01-02")]).unwrap().to_text(),
            "86400"
        );
        assert_eq!(
            f_date(&[Value::Int(86400), Value::text("unixepoch")])
                .unwrap()
                .to_text(),
            "1970-01-02"
        );
        assert!(f_date(&[Value::text("garbage")]).unwrap().is_null());
        assert_eq!(
            f_julianday(&[Value::text("2000-01-01 12:00:00")])
                .unwrap()
                .to_text(),
            "2451545.0"
        );
    }
}
