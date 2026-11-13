//! Varints and the SQLite record format.
//!
//! A record is a header listing one *serial type* per column, followed by the
//! column bodies. See section 2.1 of <https://www.sqlite.org/fileformat2.html>.

use crate::error::{corrupt, Result};
use crate::value::Value;
use std::rc::Rc;

/// Read a varint (1–9 bytes). Returns the value and the number of bytes used.
#[inline]
pub fn read_varint(buf: &[u8]) -> (u64, usize) {
    let mut v: u64 = 0;
    for (i, &b) in buf.iter().enumerate().take(8) {
        v = (v << 7) | (b & 0x7f) as u64;
        if b & 0x80 == 0 {
            return (v, i + 1);
        }
    }
    match buf.get(8) {
        Some(&b) => ((v << 8) | b as u64, 9),
        None => (v, buf.len().min(8)),
    }
}

/// Append `v` as a varint.
pub fn write_varint(v: u64, out: &mut Vec<u8>) {
    if v <= 0x7f {
        out.push(v as u8);
        return;
    }
    if v > 0x00ff_ffff_ffff_ffff {
        // Nine bytes: eight 7-bit groups and one full byte.
        let mut tmp = [0u8; 9];
        tmp[8] = v as u8;
        let mut x = v >> 8;
        for i in (0..8).rev() {
            tmp[i] = (x & 0x7f) as u8 | 0x80;
            x >>= 7;
        }
        out.extend_from_slice(&tmp);
        return;
    }
    let mut tmp = [0u8; 9];
    let mut n = 0;
    let mut x = v;
    while x != 0 {
        tmp[n] = (x & 0x7f) as u8 | 0x80;
        x >>= 7;
        n += 1;
    }
    tmp[0] &= 0x7f;
    for i in (0..n).rev() {
        out.push(tmp[i]);
    }
}

pub fn varint_len(v: u64) -> usize {
    let mut n = 1;
    let mut x = v >> 7;
    while x != 0 && n < 9 {
        n += 1;
        x >>= 7;
    }
    n
}

/// Text encoding of the database (header offset 56).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

/// Size in bytes of the body for a serial type.
#[inline]
pub fn serial_type_len(t: u64) -> usize {
    match t {
        0 | 8 | 9 | 10 | 11 => 0,
        1 => 1,
        2 => 2,
        3 => 3,
        4 => 4,
        5 => 6,
        6 | 7 => 8,
        n => ((n - 12) / 2) as usize,
    }
}

fn decode_text(bytes: &[u8], enc: TextEncoding) -> Value {
    match enc {
        TextEncoding::Utf8 => match std::str::from_utf8(bytes) {
            Ok(s) => Value::Text(Rc::from(s)),
            Err(_) => Value::Text(Rc::from(String::from_utf8_lossy(bytes).as_ref())),
        },
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| {
                    if enc == TextEncoding::Utf16Le {
                        u16::from_le_bytes([c[0], c[1]])
                    } else {
                        u16::from_be_bytes([c[0], c[1]])
                    }
                })
                .collect();
            Value::Text(Rc::from(String::from_utf16_lossy(&units).as_str()))
        }
    }
}

#[inline]
fn decode_body(t: u64, body: &[u8], enc: TextEncoding) -> Value {
    match t {
        0 => Value::Null,
        1 => Value::Int(body[0] as i8 as i64),
        2 => Value::Int(i16::from_be_bytes([body[0], body[1]]) as i64),
        3 => {
            let v = ((body[0] as i32) << 16) | ((body[1] as i32) << 8) | body[2] as i32;
            Value::Int(((v << 8) >> 8) as i64)
        }
        4 => Value::Int(i32::from_be_bytes([body[0], body[1], body[2], body[3]]) as i64),
        5 => {
            let mut b = [0u8; 8];
            b[2..].copy_from_slice(&body[..6]);
            Value::Int((i64::from_be_bytes(b) << 16) >> 16)
        }
        6 => Value::Int(i64::from_be_bytes(body[..8].try_into().unwrap())),
        7 => {
            let f = f64::from_be_bytes(body[..8].try_into().unwrap());
            if f.is_nan() {
                Value::Null
            } else {
                Value::Real(f)
            }
        }
        8 => Value::Int(0),
        9 => Value::Int(1),
        10 | 11 => Value::Null,
        n if n % 2 == 0 => Value::Blob(Rc::from(body)),
        _ => decode_text(body, enc),
    }
}

/// Decode a record into `out` (cleared first). `want` is the number of
/// leading columns the caller needs; decoding stops there. If `mask` is
/// given, columns whose entry is `false` are left as NULL without being
/// decoded, which skips the allocation for unused text and blob columns.
pub fn decode_record(
    payload: &[u8],
    enc: TextEncoding,
    want: usize,
    mask: Option<&[bool]>,
    out: &mut Vec<Value>,
) -> Result<()> {
    out.clear();
    let (hdr_len, n) = read_varint(payload);
    let hdr_len = hdr_len as usize;
    if hdr_len > payload.len() || n > hdr_len {
        return corrupt("record header longer than payload");
    }
    let mut hp = n;
    let mut bp = hdr_len;
    while hp < hdr_len && out.len() < want {
        let (t, n) = read_varint(&payload[hp..hdr_len]);
        hp += n;
        let len = serial_type_len(t);
        if bp + len > payload.len() {
            return corrupt("record body runs past payload");
        }
        let col = out.len();
        if mask.is_none_or(|m| m.get(col).copied().unwrap_or(true)) {
            out.push(decode_body(t, &payload[bp..bp + len], enc));
        } else {
            out.push(Value::Null);
        }
        bp += len;
    }
    Ok(())
}

/// Decode every column of a record.
pub fn decode_all(payload: &[u8], enc: TextEncoding) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    decode_record(payload, enc, usize::MAX, None, &mut out)?;
    Ok(out)
}

/// Serial type and body for a value, choosing the smallest integer encoding.
fn serial_type_of(v: &Value) -> (u64, usize) {
    match v {
        Value::Null => (0, 0),
        Value::Int(i) => {
            let i = *i;
            if i == 0 {
                (8, 0)
            } else if i == 1 {
                (9, 0)
            } else if (-128..=127).contains(&i) {
                (1, 1)
            } else if (-32768..=32767).contains(&i) {
                (2, 2)
            } else if (-8388608..=8388607).contains(&i) {
                (3, 3)
            } else if (-2147483648..=2147483647).contains(&i) {
                (4, 4)
            } else if (-140737488355328..=140737488355327).contains(&i) {
                (5, 6)
            } else {
                (6, 8)
            }
        }
        Value::Real(_) => (7, 8),
        Value::Text(s) => (13 + 2 * s.len() as u64, s.len()),
        Value::Blob(b) => (12 + 2 * b.len() as u64, b.len()),
    }
}

/// Encode values as a record.
pub fn encode_record(values: &[Value]) -> Vec<u8> {
    let mut types = Vec::with_capacity(values.len());
    let mut hdr_body = 0;
    let mut body_len = 0;
    for v in values {
        let (t, len) = serial_type_of(v);
        hdr_body += varint_len(t);
        body_len += len;
        types.push((t, len));
    }
    // The header length varint counts itself.
    let mut hdr_len = hdr_body + 1;
    if varint_len(hdr_len as u64) > 1 {
        hdr_len = hdr_body + varint_len((hdr_body + 2) as u64);
    }
    let mut out = Vec::with_capacity(hdr_len + body_len);
    write_varint(hdr_len as u64, &mut out);
    for (t, _) in &types {
        write_varint(*t, &mut out);
    }
    for (v, (_, len)) in values.iter().zip(&types) {
        match v {
            Value::Null => {}
            Value::Int(i) => out.extend_from_slice(&i.to_be_bytes()[8 - len..]),
            Value::Real(r) => out.extend_from_slice(&r.to_be_bytes()),
            Value::Text(s) => out.extend_from_slice(s.as_bytes()),
            Value::Blob(b) => out.extend_from_slice(b),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trip() {
        for v in [
            0u64,
            1,
            127,
            128,
            16383,
            16384,
            0xffff_ffff,
            0x00ff_ffff_ffff_ffff,
            0x0100_0000_0000_0000,
            u64::MAX,
        ] {
            let mut buf = Vec::new();
            write_varint(v, &mut buf);
            assert_eq!(buf.len(), varint_len(v), "length of {v}");
            assert_eq!(read_varint(&buf), (v, buf.len()), "value {v}");
        }
    }

    #[test]
    fn record_round_trip() {
        let vals = vec![
            Value::Null,
            Value::Int(0),
            Value::Int(1),
            Value::Int(-1),
            Value::Int(300),
            Value::Int(-70000),
            Value::Int(1 << 31),
            Value::Int(-(1 << 45)),
            Value::Int(i64::MIN),
            Value::Real(1.5),
            Value::text("héllo"),
            Value::blob([1u8, 2, 3]),
            Value::text("x".repeat(200)),
        ];
        let rec = encode_record(&vals);
        let back = decode_all(&rec, TextEncoding::Utf8).unwrap();
        assert_eq!(back.len(), vals.len());
        for (a, b) in vals.iter().zip(&back) {
            assert_eq!(
                a.compare(b, crate::value::Collation::Binary),
                std::cmp::Ordering::Equal
            );
            assert_eq!(a.type_name(), b.type_name());
        }
    }

    #[test]
    fn truncated_record_is_corrupt() {
        let rec = encode_record(&[Value::text("hello")]);
        assert!(decode_all(&rec[..rec.len() - 1], TextEncoding::Utf8).is_err());
    }
}
