//! SQLite's planner arithmetic: costs and row counts as "LogEst" values
//! (ten times the base-2 logarithm), the row-size estimates it derives
//! from declared types, and the cost it charges for sorting. Ported from
//! SQLite 3.45 (util.c, build.c, where.c) so that the planner weighs plans
//! exactly as SQLite does when it has no statistics.

use crate::schema::{Index, Table};

/// A logarithmic estimate: `10 * log2(x)`.
pub type LogEst = i32;

/// `sqlite3LogEst`: the LogEst of an integer.
pub fn log_est(mut x: u64) -> LogEst {
    const A: [LogEst; 8] = [0, 2, 3, 5, 6, 7, 8, 9];
    let mut y: LogEst = 40;
    if x < 8 {
        if x < 2 {
            return 0;
        }
        while x < 8 {
            y -= 10;
            x <<= 1;
        }
    } else {
        while x > 255 {
            y += 40;
            x >>= 4;
        }
        while x > 15 {
            y += 10;
            x >>= 1;
        }
    }
    A[(x & 7) as usize] + y - 10
}

/// `sqlite3LogEstAdd`: the LogEst of the sum of two LogEst values.
pub fn log_est_add(a: LogEst, b: LogEst) -> LogEst {
    const X: [LogEst; 32] = [
        10, 10, 9, 9, 8, 8, 7, 7, 7, 6, 6, 6, 5, 5, 5, 4, 4, 4, 4, 3, 3, 3, 3, 3, 3, 2, 2, 2, 2, 2,
        2, 2,
    ];
    let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
    if hi > lo + 49 {
        hi
    } else if hi > lo + 31 {
        hi + 1
    } else {
        hi + X[(hi - lo) as usize]
    }
}

/// `estLog`: roughly the log of a row count, for search costs.
pub fn est_log(n: LogEst) -> LogEst {
    if n <= 10 {
        0
    } else {
        log_est(n as u64) - 33
    }
}

/// The table size SQLite assumes without ANALYZE (about a million rows).
pub const TABLE_ROWS: LogEst = 200;

/// `sqlite3AffinityType`'s size estimate for a declared type, scaled so
/// that an integer is 1.
pub fn sz_est(decl: &str) -> u32 {
    if decl.is_empty() {
        return 1;
    }
    #[derive(PartialEq, PartialOrd)]
    enum Aff {
        Blob,
        Text,
        Numeric,
        Integer,
        Real,
    }
    let bytes = decl.as_bytes();
    let mut h: u32 = 0;
    let mut aff = Aff::Numeric;
    let mut z_char: Option<usize> = None;
    let word =
        |s: &[u8; 4]| (s[0] as u32) << 24 | (s[1] as u32) << 16 | (s[2] as u32) << 8 | s[3] as u32;
    let mut i = 0;
    while i < bytes.len() {
        h = (h << 8).wrapping_add(bytes[i].to_ascii_lowercase() as u32);
        i += 1;
        if h == word(b"char") {
            aff = Aff::Text;
            z_char = Some(i);
        } else if h == word(b"clob") || h == word(b"text") {
            aff = Aff::Text;
        } else if h == word(b"blob") && (aff == Aff::Numeric || aff == Aff::Real) {
            aff = Aff::Blob;
            if bytes.get(i) == Some(&b'(') {
                z_char = Some(i);
            }
        } else if (h == word(b"real") || h == word(b"floa") || h == word(b"doub"))
            && aff == Aff::Numeric
        {
            aff = Aff::Real;
        } else if h & 0x00FF_FFFF == (b'i' as u32) << 16 | (b'n' as u32) << 8 | b't' as u32 {
            aff = Aff::Integer;
            break;
        }
    }
    let mut v: u32 = 0;
    if aff < Aff::Numeric {
        match z_char {
            Some(start) => {
                if let Some(p) = bytes[start..].iter().position(u8::is_ascii_digit) {
                    let digits: String = bytes[start + p..]
                        .iter()
                        .take_while(|b| b.is_ascii_digit())
                        .map(|b| *b as char)
                        .collect();
                    v = digits
                        .parse::<u32>()
                        .unwrap_or(u32::MAX)
                        .min(i32::MAX as u32);
                }
            }
            None => v = 16,
        }
    }
    (v / 4 + 1).min(255)
}

/// `szTabRow`: the estimated width of a table row.
pub fn table_row_size(t: &Table) -> LogEst {
    let mut w: u64 = t.columns.iter().map(|c| sz_est(&c.decl_type) as u64).sum();
    if t.ipk.is_none() {
        w += 1;
    }
    log_est(w * 4)
}

/// `szIdxRow`: the estimated width of an index entry (key columns, then
/// the rowid or the rest of a WITHOUT ROWID table's primary key).
pub fn index_row_size(t: &Table, ix: &Index) -> LogEst {
    let col = |c: usize| t.columns.get(c).map_or(1, |c| sz_est(&c.decl_type) as u64);
    let mut w: u64 = ix.cols.iter().map(|c| col(c.col)).sum();
    if t.without_rowid {
        w += ix.extra.iter().map(|c| col(*c)).sum::<u64>();
    } else {
        w += 1;
    }
    log_est(w * 4)
}

/// `sqlite3DefaultRowEst`: rows in the index, then rows per distinct
/// prefix of 1, 2, ... key columns.
pub fn index_row_est(ix: &Index, nkey: usize) -> Vec<LogEst> {
    const VAL: [LogEst; 5] = [33, 32, 30, 28, 26];
    let mut a = vec![if ix.where_.is_some() {
        TABLE_ROWS - 10
    } else {
        TABLE_ROWS
    }];
    for i in 0..nkey {
        a.push(*VAL.get(i).unwrap_or(&23));
    }
    if ix.unique && nkey > 0 {
        a[nkey] = 0;
    }
    a
}

/// `whereSortingCost`: the cost of sorting `n_row` rows whose first
/// `n_sorted` of `n_order_by` keys are already in order.
pub fn sorting_cost(
    n_row: LogEst,
    n_order_by: usize,
    n_sorted: usize,
    n_result_cols: usize,
    limit: Option<LogEst>,
    distinct: bool,
) -> LogEst {
    let n_col = log_est(((n_result_cols + 59) / 30) as u64);
    let mut cost = n_row + n_col;
    if n_sorted > 0 {
        cost += log_est(((n_order_by - n_sorted) * 100 / n_order_by) as u64) - 66;
    }
    let mut n = n_row;
    if let Some(l) = limit {
        cost += 10;
        if n_sorted != 0 {
            cost += 6;
        }
        if l < n {
            n = l;
        }
    } else if distinct && n > 10 {
        n -= 10;
    }
    cost + est_log(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_est_matches_sqlite() {
        assert_eq!(log_est(0), 0);
        assert_eq!(log_est(1), 0);
        assert_eq!(log_est(2), 10);
        assert_eq!(log_est(10), 33);
        assert_eq!(log_est(25), 46);
        assert_eq!(log_est(1000), 99);
        assert_eq!(log_est(1_048_576), 200);
        assert_eq!(log_est(200), 76);
        assert_eq!(est_log(200), 43);
    }

    #[test]
    fn log_est_add_matches_sqlite() {
        assert_eq!(log_est_add(0, 0), 10);
        assert_eq!(log_est_add(43, 16), 45);
        assert_eq!(log_est_add(100, 100), 110);
        assert_eq!(log_est_add(0, 43), 44);
    }

    #[test]
    fn size_estimates() {
        assert_eq!(sz_est(""), 1);
        assert_eq!(sz_est("INTEGER"), 1);
        assert_eq!(sz_est("TEXT"), 5);
        assert_eq!(sz_est("BLOB"), 5);
        assert_eq!(sz_est("VARCHAR(100)"), 26);
        assert_eq!(sz_est("VARCHAR"), 1);
        assert_eq!(sz_est("REAL"), 1);
    }
}
