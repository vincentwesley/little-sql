//! `.dump`: the database as SQL text, in sqlite3's exact format.

use littlesql::{Database, Value};

/// A name as sqlite3's shell writes it: bare when it is a plain identifier,
/// in double quotes otherwise.
pub fn quote_name(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !littlesql::sql::parser::is_reserved(name);
    if plain {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

/// The decimal digits of `r` (positive, finite, non-zero) as sqlite3's
/// `%!.20g` produces them, trailing zeros dropped, with the position of
/// the decimal point counted from the first digit.
fn real_digits(r: f64) -> (Vec<u8>, i32) {
    let (mut d, first) = littlesql::fp::decode(r);
    while d.len() > 1 && *d.last().unwrap() == 0 {
        d.pop();
    }
    (d, first + 1)
}

/// A real number as sqlite3's `.dump` writes it.
pub fn dump_real(r: f64) -> String {
    if r.is_infinite() {
        return if r > 0.0 { "9.0e+999" } else { "-9.0e+999" }.into();
    }
    if r.is_nan() {
        return "NULL".into();
    }
    if r.abs() < 9.223_372_036_854_775e18 && r == (r as i64) as f64 {
        return format!("{}.0", r as i64);
    }
    let neg = r < 0.0;
    let (d, point) = real_digits(r.abs());
    let digit = |i: usize| char::from(b'0' + d[i]);
    let exp10 = point - 1;
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if !(-4..20).contains(&exp10) {
        out.push(digit(0));
        out.push('.');
        if d.len() > 1 {
            (1..d.len()).for_each(|i| out.push(digit(i)));
        } else {
            out.push('0');
        }
        out.push_str(&format!(
            "e{}{:02}",
            if exp10 < 0 { '-' } else { '+' },
            exp10.abs()
        ));
    } else if point <= 0 {
        out.push_str("0.");
        (0..-point).for_each(|_| out.push('0'));
        (0..d.len()).for_each(|i| out.push(digit(i)));
    } else {
        let p = point as usize;
        for i in 0..p {
            out.push(if i < d.len() { digit(i) } else { '0' });
        }
        out.push('.');
        if d.len() > p {
            (p..d.len()).for_each(|i| out.push(digit(i)));
        } else {
            out.push('0');
        }
    }
    out
}

/// A text value as sqlite3's `.dump` writes it: quoted, with CR and LF
/// spelled `\r` / `\n` inside `replace(...)` calls.
fn dump_text(t: &str) -> String {
    let body = t
        .replace('\'', "''")
        .replace('\r', "\\r")
        .replace('\n', "\\n");
    let mut s = format!("'{body}'");
    if t.contains('\r') {
        s = format!("replace({s},'\\r',char(13))");
    }
    if t.contains('\n') {
        s = format!("replace({s},'\\n',char(10))");
    }
    s
}

pub fn dump_value(v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Int(i) => i.to_string(),
        Value::Real(r) => dump_real(*r),
        Value::Text(t) => dump_text(t),
        Value::Blob(b) => format!(
            "X'{}'",
            b.iter().map(|x| format!("{x:02x}")).collect::<String>()
        ),
    }
}

/// The text of `.dump [PATTERN]` for the main database.
pub fn dump(db: &Database, pattern: Option<&str>) -> Result<String, String> {
    let q = |sql: &str| db.query(sql).map(|rs| rs.rows).map_err(|e| e.to_string());
    let like = |col: &str| match pattern {
        Some(p) => format!(" AND {col} LIKE '{}'", p.replace('\'', "''")),
        None => String::new(),
    };
    let mut out = String::from("PRAGMA foreign_keys=OFF;\nBEGIN TRANSACTION;\n");
    let tables = q(&format!(
        "SELECT name, sql FROM sqlite_schema WHERE sql NOT NULL AND type = 'table'{} \
         ORDER BY name = 'sqlite_sequence', rowid",
        like("tbl_name")
    ))?;
    for row in tables {
        let name = row[0].to_text();
        let sql = row[1].to_text();
        if name == "sqlite_sequence" {
            out.push_str("DELETE FROM sqlite_sequence;\n");
        } else if name.starts_with("sqlite_") {
            continue;
        } else if let Some(rest) = sql
            .strip_prefix("CREATE TABLE ")
            .filter(|r| r.starts_with(['"', '\'', '[', '`']))
        {
            out.push_str(&format!("CREATE TABLE IF NOT EXISTS {rest};\n"));
        } else {
            out.push_str(&format!("{sql};\n"));
        }
        let rows = q(&format!("SELECT * FROM {}", quote_name(&name)))?;
        for r in rows {
            let vals: Vec<String> = r.iter().map(dump_value).collect();
            out.push_str(&format!(
                "INSERT INTO {} VALUES({});\n",
                quote_name(&name),
                vals.join(",")
            ));
        }
    }
    let others = q(&format!(
        "SELECT sql FROM sqlite_schema WHERE sql NOT NULL \
         AND type IN ('index', 'trigger', 'view'){} ORDER BY rowid",
        like("tbl_name")
    ))?;
    for row in others {
        out.push_str(&format!("{};\n", row[0].to_text()));
    }
    out.push_str("COMMIT;\n");
    Ok(out)
}
