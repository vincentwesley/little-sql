//! SQLite's floating-point text conversions, reproduced exactly.
//!
//! SQLite 3.45 converts between text and doubles with x87 80-bit "long
//! double" arithmetic (`sqlite3AtoF`, `sqlite3FpDecode`). The results are
//! not always the correctly rounded ones, so a correct conversion would
//! store and print slightly different values than sqlite3 does. This module
//! emulates the long double steps bit for bit (DECISIONS D38).

/// An x87 80-bit "long double": value = m * 2^e with the top bit of `m`
/// set. SQLite 3.45 formats reals with long double arithmetic on x86, so
/// `.dump` digits depend on its rounding; this reproduces it.
#[derive(Clone, Copy)]
pub struct Ext {
    m: u64,
    e: i32,
}

impl Ext {
    pub fn from_f64(r: f64) -> Ext {
        let bits = r.to_bits();
        let exp = ((bits >> 52) & 0x7ff) as i32;
        let frac = bits & ((1u64 << 52) - 1);
        let (m, e) = if exp == 0 {
            (frac, -1074)
        } else {
            (frac | (1u64 << 52), exp - 1075)
        };
        let z = m.leading_zeros() as i32;
        Ext {
            m: m << z,
            e: e - z,
        }
    }

    /// Round a 128-bit product to 64 bits, to nearest, ties to even.
    pub fn times(self, b: Ext) -> Ext {
        let p = self.m as u128 * b.m as u128;
        let shift = if p >> 127 == 1 { 64 } else { 63 };
        let mut m = (p >> shift) as u64;
        let rem = p & ((1u128 << shift) - 1);
        let half = 1u128 << (shift - 1);
        let mut e = self.e + b.e + shift;
        if rem > half || (rem == half && m & 1 == 1) {
            m = m.wrapping_add(1);
            if m == 0 {
                m = 1 << 63;
                e += 1;
            }
        }
        Ext { m, e }
    }

    pub fn lt(self, b: Ext) -> bool {
        (self.e, self.m) < (b.e, b.m)
    }

    /// Truncate toward zero to an integer.
    pub fn to_u64(self) -> u64 {
        if self.e > 0 {
            u64::MAX
        } else if self.e == 0 {
            self.m
        } else if self.e <= -64 {
            0
        } else {
            self.m >> (-self.e)
        }
    }

    /// 10^k rounded to a long double (k may be negative).
    pub fn pow10(k: i32) -> Ext {
        // Big integers in base 2^32, least significant limb first.
        fn mul_small(a: &mut Vec<u32>, x: u32) {
            let mut carry = 0u64;
            for l in a.iter_mut() {
                let v = *l as u64 * x as u64 + carry;
                *l = v as u32;
                carry = v >> 32;
            }
            if carry > 0 {
                a.push(carry as u32);
            }
        }
        fn bits(a: &[u32]) -> u32 {
            let top = *a.last().unwrap();
            (a.len() as u32 - 1) * 32 + (32 - top.leading_zeros())
        }
        fn bit(a: &[u32], i: u32) -> bool {
            a.get((i / 32) as usize)
                .is_some_and(|l| l >> (i % 32) & 1 == 1)
        }
        // Round the big integer `a` (with `extra` known to follow: sticky)
        // to its top 64 bits; returns (mantissa, exponent of its lowest bit).
        fn round64(a: &[u32], sticky_below: bool) -> (u64, i32) {
            let n = bits(a);
            if n <= 64 {
                let mut v = 0u64;
                for (i, l) in a.iter().enumerate() {
                    v |= (*l as u64) << (32 * i);
                }
                let z = v.leading_zeros();
                return (v << z, -(z as i32));
            }
            let low = n - 64;
            let mut m = 0u64;
            for i in 0..64 {
                if bit(a, low + i) {
                    m |= 1 << i;
                }
            }
            let half = bit(a, low - 1);
            let rest = sticky_below || (0..low - 1).any(|i| bit(a, i));
            let mut e = low as i32;
            if half && (rest || m & 1 == 1) {
                m = m.wrapping_add(1);
                if m == 0 {
                    m = 1 << 63;
                    e += 1;
                }
            }
            (m, e)
        }
        let mut p = vec![1u32];
        for _ in 0..k.unsigned_abs() {
            mul_small(&mut p, 10);
        }
        if k >= 0 {
            let (m, e) = round64(&p, false);
            return Ext { m, e };
        }
        // 1 / 10^|k|: long division of 2^s by p, giving 66 quotient bits.
        let pb = bits(&p) as i32;
        let s = pb + 65;
        // Shift-subtract division producing the quotient bit by bit.
        let mut rem: Vec<u32> = vec![0];
        let mut q: Vec<u32> = vec![0];
        let ge = |a: &[u32], b: &[u32]| -> bool {
            let la = a.iter().rposition(|x| *x != 0).map_or(0, |i| i + 1);
            let lb = b.iter().rposition(|x| *x != 0).map_or(0, |i| i + 1);
            if la != lb {
                return la > lb;
            }
            for i in (0..la).rev() {
                if a[i] != b[i] {
                    return a[i] > b[i];
                }
            }
            true
        };
        for i in (0..=s).rev() {
            // rem = rem * 2 + (bit i of 2^s)
            mul_small(&mut rem, 2);
            if i == s {
                rem[0] |= 1;
            }
            mul_small(&mut q, 2);
            if ge(&rem, &p) {
                let mut borrow = 0i64;
                for (j, r) in rem.iter_mut().enumerate() {
                    let v = *r as i64 - *p.get(j).unwrap_or(&0) as i64 - borrow;
                    *r = v.rem_euclid(1 << 32) as u32;
                    borrow = if v < 0 { 1 } else { 0 };
                }
                q[0] |= 1;
            }
        }
        let sticky = rem.iter().any(|x| *x != 0);
        let (m, e) = round64(&q, sticky);
        Ext { m, e: e - s }
    }
}

impl Ext {
    fn from_u64(v: u64) -> Ext {
        let z = v.leading_zeros() as i32;
        Ext { m: v << z, e: -z }
    }

    /// Round to the nearest double, ties to even (as the x87 does when
    /// storing a long double into a double), with subnormals and
    /// overflow to infinity.
    pub fn to_f64(self) -> f64 {
        // Value = m * 2^e, m in [2^63, 2^64): the leading bit has weight
        // 2^(e+63).
        let top = self.e + 63;
        if top > 1023 {
            return f64::INFINITY;
        }
        // Bits of precision available: 53 for normal numbers, fewer for
        // subnormal ones (whose lowest bit has weight 2^-1074).
        let keep = if top >= -1022 {
            53
        } else {
            (53 - (-1022 - top)).max(0)
        };
        if keep <= 0 {
            // Below half the smallest subnormal rounds to zero; exactly or
            // above half rounds to it (ties to even means zero at exactly
            // half).
            let half_min = top == -1075;
            let above = self.m > 1 << 63;
            return if half_min && above {
                f64::from_bits(1)
            } else {
                0.0
            };
        }
        let drop = 64 - keep as u32;
        let mut mant = self.m >> drop;
        let rem = self.m & ((1u64 << drop) - 1);
        let half = 1u64 << (drop - 1);
        let mut e = self.e + drop as i32;
        if rem > half || (rem == half && mant & 1 == 1) {
            mant += 1;
            if mant >> keep == 1 {
                mant >>= 1;
                e += 1;
            }
        }
        // mant * 2^e as a double.
        let lz = mant.leading_zeros() as i32;
        let top = e + 63 - lz;
        if top > 1023 {
            return f64::INFINITY;
        }
        if top >= -1022 {
            let frac = (mant << (lz + 1)) >> 12;
            f64::from_bits(((top + 1023) as u64) << 52 | frac)
        } else {
            // Subnormal: the bits are mant shifted so the lowest has weight
            // 2^-1074.
            f64::from_bits(mant << (e + 1074))
        }
    }
}

thread_local! {
    static POW10: std::cell::RefCell<std::collections::HashMap<i32, Ext>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// 10^k as a long double (cached).
fn p10(k: i32) -> Ext {
    POW10.with(|c| *c.borrow_mut().entry(k).or_insert_with(|| Ext::pow10(k)))
}

/// `sqlite3AtoF` for a well-formed decimal literal: optional sign, digits
/// with an optional point, optional exponent.
pub fn atof(text: &str) -> f64 {
    const LARGEST: u64 = u64::MAX;
    let b = text.as_bytes();
    let mut i = 0;
    let mut neg = false;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        neg = b[i] == b'-';
        i += 1;
    }
    let mut s: u64 = 0;
    let mut d: i32 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        s = s * 10 + (b[i] - b'0') as u64;
        i += 1;
        if s >= (LARGEST - 9) / 10 {
            // Further digits only scale the value.
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
                d += 1;
            }
        }
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            if s < (LARGEST - 9) / 10 {
                s = s * 10 + (b[i] - b'0') as u64;
                d -= 1;
            }
            i += 1;
        }
    }
    let mut e: i32 = 0;
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        let mut esign = 1;
        if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
            if b[i] == b'-' {
                esign = -1;
            }
            i += 1;
        }
        while i < b.len() && b[i].is_ascii_digit() {
            if e < 10000 {
                e = e * 10 + (b[i] - b'0') as i32;
            }
            i += 1;
        }
        e *= esign;
    }
    let sign = if neg { -1.0 } else { 1.0 };
    if s == 0 {
        return sign * 0.0;
    }
    let mut e = e + d;
    while e > 0 && s < LARGEST / 10 {
        s *= 10;
        e -= 1;
    }
    while e < 0 && s.is_multiple_of(10) {
        s /= 10;
        e += 1;
    }
    let mut r = Ext::from_u64(s);
    if e > 0 {
        while e >= 100 {
            e -= 100;
            r = r.times(p10(100));
        }
        while e >= 10 {
            e -= 10;
            r = r.times(p10(10));
        }
        while e >= 1 {
            e -= 1;
            r = r.times(p10(1));
        }
    } else {
        while e <= -100 {
            e += 100;
            r = r.times(p10(-100));
        }
        while e <= -10 {
            e += 10;
            r = r.times(p10(-10));
        }
        while e <= -1 {
            e += 1;
            r = r.times(p10(-1));
        }
    }
    sign * r.to_f64()
}

/// `sqlite3FpDecode`'s digits for a finite, positive `r`: the value scaled
/// into [1e17, 1e19) with long doubles and truncated to an integer.
/// Returns the digits (trailing zeros kept) and the decimal exponent of
/// the first digit (r = d1.d2d3... * 10^exp).
pub fn decode(r: f64) -> (Vec<u8>, i32) {
    let mut rr = Ext::from_f64(r);
    let mut exp = 0i32;
    if !rr.lt(p10(19)) {
        while !rr.lt(p10(119)) {
            exp += 100;
            rr = rr.times(p10(-100));
        }
        while !rr.lt(p10(29)) {
            exp += 10;
            rr = rr.times(p10(-10));
        }
        while !rr.lt(p10(19)) {
            exp += 1;
            rr = rr.times(p10(-1));
        }
    } else {
        while rr.lt(p10(-97)) {
            exp -= 100;
            rr = rr.times(p10(100));
        }
        while rr.lt(p10(7)) {
            exp -= 10;
            rr = rr.times(p10(10));
        }
        while rr.lt(p10(17)) {
            exp -= 1;
            rr = rr.times(p10(1));
        }
    }
    let v = rr.to_u64();
    let d: Vec<u8> = v.to_string().bytes().map(|b| b - b'0').collect();
    let first = d.len() as i32 + exp - 1;
    (d, first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_double_matches_sqlite() {
        // Values sqlite3 3.45 (x86-64) parses or prints differently from a
        // correctly rounded conversion.
        let sqlite: f64 = "4.257750140226443537e+239".parse().unwrap();
        assert_eq!(atof("4.257750140226444e+239").to_bits(), sqlite.to_bits());
        let (d, e) = decode(2.5e-300);
        assert_eq!(
            (d.iter().map(|x| (b'0' + x) as char).collect::<String>(), e),
            ("249999999999999998".into(), -300)
        );
        assert_eq!(atof("0.1"), 0.1);
        assert_eq!(atof("1e400"), f64::INFINITY);
        assert_eq!(atof("-0.0"), 0.0);
        assert_eq!(atof("5e-324").to_bits(), 1);
    }
}
