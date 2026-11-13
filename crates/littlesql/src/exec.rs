//! The executor: interprets a bound [`Plan`].
//!
//! A `SELECT` runs as nested loops over its FROM sources, outermost first.
//! Each joined row flows through WHERE filters into either the result
//! projection or the aggregator, then through DISTINCT, ORDER BY and LIMIT.

use crate::btree::Cursor;
use crate::error::{corrupt, Error, Result};
use crate::func::{escape_char, glob, like, AggFunc, AggState};
use crate::pager::Pager;
use crate::plan::*;
use crate::record::{decode_all, decode_record, TextEncoding};
use crate::schema::{Index, Table};
use crate::sql::ast::{BinaryOp, CompoundOp, LikeOp, UnaryOp};
use crate::value::{hash_key, real_to_int, Affinity, Collation, Value};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// The current row of a cursor. An empty slice stands for "all NULL" (the
/// right side of a LEFT JOIN without a match).
pub type Row = Rc<[Value]>;

struct InSet {
    keys: HashSet<Vec<u8>>,
    has_null: bool,
    empty: bool,
}

enum SubCache {
    Scalar(Value),
    Exists(bool),
    Set(Rc<InSet>),
}

struct HashTable {
    rows: Vec<Row>,
    map: HashMap<Vec<u8>, Vec<u32>>,
}

struct Group {
    key: Vec<Value>,
    states: Vec<AggState>,
    /// Snapshot of the source rows that bare columns are read from.
    rows: Vec<Row>,
}

pub struct Exec<'a> {
    pager: Rc<Pager>,
    used: &'a [Vec<bool>],
    enc: TextEncoding,
    rows: Vec<Row>,
    aggs: Vec<Vec<Value>>,
    sub_cache: HashMap<usize, SubCache>,
    mat_cache: HashMap<usize, Rc<Vec<Row>>>,
    hash_cache: HashMap<usize, Rc<HashTable>>,
    /// Current contents of each recursive CTE's work table.
    work: HashMap<usize, Vec<Value>>,
    empty_row: Row,
    pub stats: Vec<CursorStats>,
    steps: u64,
    limits: Limits,
    #[cfg(not(target_arch = "wasm32"))]
    deadline: Option<std::time::Instant>,
}

/// Guards against runaway statements.
#[derive(Clone, Copy, Debug, Default)]
pub struct Limits {
    /// Stop after visiting this many rows (0 = unlimited).
    pub max_steps: u64,
    /// Stop after this many milliseconds (0 = unlimited; ignored in
    /// WebAssembly, which has no clock).
    pub max_millis: u64,
}

/// Row callback: return `Ok(false)` to stop the query early.
type Sink<'s> = &'s mut dyn FnMut(Vec<Value>) -> Result<bool>;

fn apply_cmp_aff(v: Value, aff: Affinity) -> Value {
    match aff {
        Affinity::Blob => v,
        Affinity::Text => v.apply_affinity(Affinity::Text),
        _ => v.apply_numeric_for_compare(),
    }
}

fn bool_value(b: Option<bool>) -> Value {
    match b {
        Some(true) => Value::Int(1),
        Some(false) => Value::Int(0),
        None => Value::Null,
    }
}

/// Binary arithmetic with SQLite's rules: integer overflow falls back to
/// floating point, division by zero gives NULL.
pub fn arith(op: BinaryOp, a: &Value, b: &Value) -> Value {
    if a.is_null() || b.is_null() {
        return Value::Null;
    }
    if op == BinaryOp::Concat {
        let mut s = a.to_text();
        s.push_str(&b.to_text());
        return Value::text(s);
    }
    if matches!(
        op,
        BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::Shl | BinaryOp::Shr
    ) {
        let (x, y) = (a.to_int(), b.to_int());
        return Value::Int(match op {
            BinaryOp::BitAnd => x & y,
            BinaryOp::BitOr => x | y,
            _ => {
                // A negative shift count shifts the other way.
                let left = (op == BinaryOp::Shl) == (y >= 0);
                let n = y.unsigned_abs();
                if left {
                    if n >= 64 {
                        0
                    } else {
                        ((x as u64) << n) as i64
                    }
                } else if n >= 64 {
                    if x < 0 {
                        -1
                    } else {
                        0
                    }
                } else {
                    x >> n
                }
            }
        });
    }
    let (x, y) = (a.to_numeric(), b.to_numeric());
    let real = |r: f64| {
        if r.is_nan() {
            Value::Null
        } else {
            Value::Real(r)
        }
    };
    match (op, &x, &y) {
        (BinaryOp::Rem, _, _) => {
            let (i, j) = (x.to_int(), y.to_int());
            if j == 0 {
                return Value::Null;
            }
            let r = i.wrapping_rem(j);
            if matches!(x, Value::Real(_)) || matches!(y, Value::Real(_)) {
                Value::Real(r as f64)
            } else {
                Value::Int(r)
            }
        }
        (_, Value::Int(i), Value::Int(j)) => {
            let (i, j) = (*i, *j);
            match op {
                BinaryOp::Add => i
                    .checked_add(j)
                    .map(Value::Int)
                    .unwrap_or_else(|| real(i as f64 + j as f64)),
                BinaryOp::Sub => i
                    .checked_sub(j)
                    .map(Value::Int)
                    .unwrap_or_else(|| real(i as f64 - j as f64)),
                BinaryOp::Mul => i
                    .checked_mul(j)
                    .map(Value::Int)
                    .unwrap_or_else(|| real(i as f64 * j as f64)),
                _ => {
                    if j == 0 {
                        Value::Null
                    } else {
                        i.checked_div(j)
                            .map(Value::Int)
                            .unwrap_or_else(|| real(i as f64 / j as f64))
                    }
                }
            }
        }
        _ => {
            let (p, q) = (x.to_real(), y.to_real());
            match op {
                BinaryOp::Add => real(p + q),
                BinaryOp::Sub => real(p - q),
                BinaryOp::Mul => real(p * q),
                _ => {
                    if q == 0.0 {
                        Value::Null
                    } else {
                        real(p / q)
                    }
                }
            }
        }
    }
}

/// `CAST(v AS <affinity>)`.
pub fn cast(v: Value, aff: Affinity) -> Value {
    if v.is_null() {
        return v;
    }
    match aff {
        Affinity::Text => match v {
            Value::Text(_) => v,
            other => Value::text(other.to_text()),
        },
        Affinity::Real => Value::Real(v.to_real()),
        Affinity::Integer => Value::Int(v.to_int()),
        Affinity::Numeric => match v {
            Value::Int(_) | Value::Real(_) => v,
            other => match other.to_numeric() {
                Value::Real(r) => crate::value::real_to_int_if_exact(r),
                n => n,
            },
        },
        Affinity::Blob => match v {
            Value::Blob(_) => v,
            other => Value::blob(crate::value::text_bytes(&other.to_text())),
        },
    }
}

fn compare_keys(a: &[Value], b: &[Value], order: &[OrderKey]) -> Ordering {
    for (i, k) in order.iter().enumerate() {
        let (x, y) = (&a[i], &b[i]);
        let o = match (x.is_null(), y.is_null()) {
            (true, true) => Ordering::Equal,
            (true, false) => {
                if k.nulls_first {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (false, true) => {
                if k.nulls_first {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (false, false) => {
                let o = x.compare(y, k.coll);
                if k.desc {
                    o.reverse()
                } else {
                    o
                }
            }
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}

fn row_key(row: &[Value], colls: &[Collation]) -> Vec<u8> {
    let mut key = Vec::with_capacity(row.len() * 9);
    for (i, v) in row.iter().enumerate() {
        hash_key(
            v,
            colls.get(i).copied().unwrap_or(Collation::Binary),
            &mut key,
        );
    }
    key
}

/// Order an index entry (its leading fields) against a search key.
fn compare_entry(
    payload: &[u8],
    key: &[Value],
    index: &Index,
    enc: TextEncoding,
) -> Result<Ordering> {
    let mut vals = Vec::with_capacity(key.len());
    decode_record(payload, enc, key.len(), None, &mut vals)?;
    for (i, k) in key.iter().enumerate() {
        let v = vals.get(i).unwrap_or(&Value::Null);
        let ic = &index.cols[i];
        let o = v.compare(k, ic.collation);
        if o != Ordering::Equal {
            return Ok(if ic.desc { o.reverse() } else { o });
        }
    }
    Ok(Ordering::Equal)
}

/// Decode one table row into the executor's layout: one value per column
/// followed by the rowid.
pub fn decode_table_row(
    table: &Table,
    payload: &[u8],
    rowid: i64,
    enc: TextEncoding,
    mask: Option<&[bool]>,
) -> Result<Row> {
    let ncols = table.columns.len();
    let mut vals = Vec::with_capacity(ncols + 1);
    if table.without_rowid {
        let stored = decode_all(payload, enc)?;
        vals.resize(ncols, Value::Null);
        for (pos, col) in table.storage_order.iter().enumerate() {
            vals[*col] = stored
                .get(pos)
                .cloned()
                .unwrap_or_else(|| table.columns[*col].default_value.clone());
        }
    } else {
        let want = match mask {
            Some(m) => m.iter().take(ncols).rposition(|u| *u).map_or(0, |p| p + 1),
            None => ncols,
        };
        decode_record(payload, enc, want, mask, &mut vals)?;
        // Columns added by ALTER TABLE are missing from older rows.
        for col in vals.len()..ncols {
            let needed = col < want && mask.is_none_or(|m| m[col]);
            vals.push(if needed {
                table.columns[col].default_value.clone()
            } else {
                Value::Null
            });
        }
    }
    for (v, c) in vals.iter_mut().zip(&table.columns) {
        if c.affinity == Affinity::Real {
            if let Value::Int(i) = v {
                *v = Value::Real(*i as f64);
            }
        }
    }
    if let Some(ipk) = table.ipk {
        vals[ipk] = Value::Int(rowid);
    }
    vals.push(if table.without_rowid {
        Value::Null
    } else {
        Value::Int(rowid)
    });
    Ok(Rc::from(vals))
}

impl<'a> Exec<'a> {
    pub fn new(pager: Rc<Pager>, plan: &'a Plan) -> Exec<'a> {
        let n = plan.used.len();
        let empty_row: Row = Rc::from(Vec::new());
        crate::value::set_text_encoding(pager.encoding());
        Exec {
            enc: pager.encoding(),
            pager,
            used: &plan.used,
            rows: vec![empty_row.clone(); n],
            aggs: vec![Vec::new(); plan.n_agg_slots],
            sub_cache: HashMap::new(),
            mat_cache: HashMap::new(),
            hash_cache: HashMap::new(),
            work: HashMap::new(),
            empty_row,
            stats: vec![CursorStats::default(); n],
            steps: 0,
            limits: Limits::default(),
            #[cfg(not(target_arch = "wasm32"))]
            deadline: None,
        }
    }

    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.deadline = if limits.max_millis > 0 {
                Some(
                    std::time::Instant::now() + std::time::Duration::from_millis(limits.max_millis),
                )
            } else {
                None
            };
        }
    }

    /// Count one visited row and stop the statement if it exceeded a limit.
    #[inline]
    fn tick(&mut self, cur: usize) -> Result<()> {
        self.stats[cur].rows += 1;
        self.tick_free()
    }

    /// Count one unit of work against the limits, without a cursor.
    fn tick_free(&mut self) -> Result<()> {
        self.steps += 1;
        if self.limits.max_steps > 0 && self.steps > self.limits.max_steps {
            return Err(Error::Runtime(
                "interrupted: statement exceeded the row-visit limit".into(),
            ));
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.steps.is_multiple_of(1024) {
            if let Some(d) = self.deadline {
                if std::time::Instant::now() > d {
                    return Err(Error::Runtime(
                        "interrupted: statement exceeded the time limit".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Run a query, handing each result row to `sink`.
    pub fn run_query(&mut self, q: &Query, sink: Sink) -> Result<()> {
        let limit = match &q.limit {
            Some(e) => match self.eval(e)? {
                Value::Null => None,
                v => Some(v.to_int()).filter(|n| *n >= 0),
            },
            None => None,
        };
        let mut offset = match &q.offset {
            Some(e) => self.eval(e)?.to_int().max(0),
            None => 0,
        };
        if limit == Some(0) {
            return Ok(());
        }
        if q.order_by.is_empty() {
            let mut left = limit;
            return self.run_body(q, &mut |row, _| {
                if offset > 0 {
                    offset -= 1;
                    return Ok(true);
                }
                if !sink(row)? {
                    return Ok(false);
                }
                if let Some(n) = &mut left {
                    *n -= 1;
                    if *n == 0 {
                        return Ok(false);
                    }
                }
                Ok(true)
            });
        }
        let mut buf: Vec<(Vec<Value>, Vec<Value>)> = Vec::new();
        self.run_body(q, &mut |row, keys| {
            buf.push((keys, row));
            Ok(true)
        })?;
        buf.sort_by(|a, b| compare_keys(&a.0, &b.0, &q.order_by));
        let take = limit.map_or(usize::MAX, |n| n as usize);
        for (_, row) in buf.into_iter().skip(offset as usize).take(take) {
            if !sink(row)? {
                break;
            }
        }
        Ok(())
    }

    /// Run a query and collect its rows.
    pub fn collect(&mut self, q: &Query) -> Result<Vec<Vec<Value>>> {
        let mut rows = Vec::new();
        self.run_query(q, &mut |row| {
            rows.push(row);
            Ok(true)
        })?;
        Ok(rows)
    }

    /// Produce the rows of the query body together with their sort keys.
    fn run_body(
        &mut self,
        q: &Query,
        emit: &mut dyn FnMut(Vec<Value>, Vec<Value>) -> Result<bool>,
    ) -> Result<()> {
        let result_keys = |row: &[Value]| -> Vec<Value> {
            q.order_by
                .iter()
                .map(|k| match &k.src {
                    KeySrc::Result(i) => row[*i].clone(),
                    KeySrc::Expr(_) => Value::Null,
                })
                .collect()
        };
        match &q.body {
            Body::Core(core) => self.run_core(core, &q.order_by, emit),
            Body::Values(rows) => {
                for r in rows {
                    let row = r.iter().map(|e| self.eval(e)).collect::<Result<Vec<_>>>()?;
                    let keys = result_keys(&row);
                    if !emit(row, keys)? {
                        break;
                    }
                }
                Ok(())
            }
            Body::WorkTable(id) => match self.work.get(id).cloned() {
                Some(row) => {
                    let keys = result_keys(&row);
                    emit(row, keys).map(|_| ())
                }
                None => Ok(()),
            },
            Body::Recursive {
                init,
                step,
                union_all,
                work,
            } => {
                let colls: Vec<Collation> = q.cols.iter().map(|c| c.coll).collect();
                let mut seen: HashSet<Vec<u8>> = HashSet::new();
                let mut queue: std::collections::VecDeque<Vec<Value>> = Default::default();
                for row in self.collect(init)? {
                    if *union_all || seen.insert(row_key(&row, &colls)) {
                        queue.push_back(row);
                    }
                }
                let saved = self.work.remove(work);
                let mut result = Ok(());
                while let Some(row) = queue.pop_front() {
                    if let Err(e) = self.tick_free() {
                        result = Err(e);
                        break;
                    }
                    let keys = result_keys(&row);
                    match emit(row.clone(), keys) {
                        Ok(true) => {}
                        Ok(false) => break,
                        Err(e) => {
                            result = Err(e);
                            break;
                        }
                    }
                    self.work.insert(*work, row);
                    match self.collect(step) {
                        Ok(rows) => {
                            for r in rows {
                                if *union_all || seen.insert(row_key(&r, &colls)) {
                                    queue.push_back(r);
                                }
                            }
                        }
                        Err(e) => {
                            result = Err(e);
                            break;
                        }
                    }
                }
                self.work.remove(work);
                if let Some(s) = saved {
                    self.work.insert(*work, s);
                }
                result
            }
            Body::Compound { op, left, right } => {
                let colls: Vec<Collation> = q.cols.iter().map(|c| c.coll).collect();
                if *op == CompoundOp::UnionAll {
                    let mut go = true;
                    self.run_query(left, &mut |row| {
                        let keys = result_keys(&row);
                        go = emit(row, keys)?;
                        Ok(go)
                    })?;
                    if go {
                        self.run_query(right, &mut |row| {
                            let keys = result_keys(&row);
                            emit(row, keys)
                        })?;
                    }
                    return Ok(());
                }
                let l = self.collect(left)?;
                let r = self.collect(right)?;
                // SQLite builds these in a temporary index where inserting a
                // key that compares equal replaces the earlier row, so of
                // two equal rows (1 and 1.0, or 'a' and 'A' under NOCASE)
                // the later one is the one returned.
                let mut slot: HashMap<Vec<u8>, usize> = HashMap::new();
                let mut out: Vec<Vec<Value>> = Vec::new();
                let mut add = |row: Vec<Value>, key: Vec<u8>| match slot.get(&key) {
                    Some(&i) => out[i] = row,
                    None => {
                        slot.insert(key, out.len());
                        out.push(row);
                    }
                };
                match op {
                    CompoundOp::Union => {
                        for row in l.into_iter().chain(r) {
                            let key = row_key(&row, &colls);
                            add(row, key);
                        }
                    }
                    _ => {
                        let right_keys: HashSet<Vec<u8>> =
                            r.iter().map(|row| row_key(row, &colls)).collect();
                        let keep = *op == CompoundOp::Intersect;
                        for row in l {
                            let key = row_key(&row, &colls);
                            if right_keys.contains(&key) == keep {
                                add(row, key);
                            }
                        }
                    }
                }
                // SQLite computes these with a sorted temporary index, so
                // the natural output order is ascending.
                out.sort_by(|a, b| {
                    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                        let o = x.compare(y, colls[i]);
                        if o != Ordering::Equal {
                            return o;
                        }
                    }
                    Ordering::Equal
                });
                for row in out {
                    let keys = result_keys(&row);
                    if !emit(row, keys)? {
                        break;
                    }
                }
                Ok(())
            }
        }
    }

    fn passes(&mut self, filters: &[Expr]) -> Result<bool> {
        for f in filters {
            if self.eval(f)?.truth() != Some(true) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn run_core(
        &mut self,
        core: &Core,
        order_by: &[OrderKey],
        emit: &mut dyn FnMut(Vec<Value>, Vec<Value>) -> Result<bool>,
    ) -> Result<()> {
        let const_ok = self.passes(&core.const_filters)?;
        if !const_ok && core.agg.is_none() {
            return Ok(());
        }
        let colls: Vec<Collation> = if core.distinct {
            core.results
                .iter()
                .map(|e| collation(e).map_or(Collation::Binary, |c| c.0))
                .collect()
        } else {
            Vec::new()
        };
        let mut seen: HashSet<Vec<u8>> = HashSet::new();
        // Shared tail: project, compute sort keys, apply DISTINCT, emit.
        let mut project = |me: &mut Exec<'a>| -> Result<bool> {
            let mut row = Vec::with_capacity(core.results.len());
            for e in &core.results {
                row.push(me.eval(e)?);
            }
            if core.distinct && !seen.insert(row_key(&row, &colls)) {
                return Ok(true);
            }
            let mut keys = Vec::with_capacity(order_by.len());
            for k in order_by {
                keys.push(match &k.src {
                    KeySrc::Result(i) => row[*i].clone(),
                    KeySrc::Expr(e) => me.eval(e)?,
                });
            }
            emit(row, keys)
        };

        let Some(agg) = &core.agg else {
            self.loop_level(core, 0, &mut project)?;
            return Ok(());
        };

        // Aggregation: one pass over the joined rows, grouped by a hash of
        // the GROUP BY values.
        let cursors: Vec<usize> = core.sources.iter().map(|s| s.cursor).collect();
        let minmax_only =
            agg.calls.len() == 1 && matches!(agg.calls[0].func, AggFunc::Min | AggFunc::Max);
        let new_states = |agg: &Agg| -> Vec<AggState> {
            agg.calls
                .iter()
                .map(|c| AggState::new(c.func, c.distinct, c.coll))
                .collect()
        };
        let mut index: HashMap<Vec<u8>, usize> = HashMap::new();
        let mut groups: Vec<Group> = Vec::new();
        if const_ok {
            self.loop_level(core, 0, &mut |me| {
                let mut key_vals = Vec::with_capacity(agg.group_by.len());
                let mut key = Vec::new();
                for (e, coll) in &agg.group_by {
                    let v = me.eval(e)?;
                    hash_key(&v, *coll, &mut key);
                    key_vals.push(v);
                }
                let gi = match index.get(&key) {
                    Some(i) => *i,
                    None => {
                        groups.push(Group {
                            key: key_vals,
                            states: new_states(agg),
                            rows: Vec::new(),
                        });
                        index.insert(key, groups.len() - 1);
                        groups.len() - 1
                    }
                };
                let mut snapshot = !minmax_only || groups[gi].rows.is_empty();
                let mut args = Vec::new();
                for (ci, call) in agg.calls.iter().enumerate() {
                    if let Some(f) = &call.filter {
                        if me.eval(f)?.truth() != Some(true) {
                            continue;
                        }
                    }
                    args.clear();
                    for a in &call.args {
                        args.push(me.eval(a)?);
                    }
                    if groups[gi].states[ci].step(&args) && minmax_only {
                        snapshot = true;
                    }
                }
                if snapshot {
                    let g = &mut groups[gi];
                    g.rows.clear();
                    g.rows.extend(cursors.iter().map(|c| me.rows[*c].clone()));
                }
                Ok(true)
            })?;
        }
        if groups.is_empty() && agg.group_by.is_empty() {
            // An aggregate without GROUP BY always yields one row.
            groups.push(Group {
                key: Vec::new(),
                states: new_states(agg),
                rows: cursors.iter().map(|_| self.empty_row.clone()).collect(),
            });
        }
        if !agg.group_by.is_empty() {
            groups.sort_by(|a, b| {
                for (i, (_, coll)) in agg.group_by.iter().enumerate() {
                    let o = a.key[i].compare(&b.key[i], *coll);
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                Ordering::Equal
            });
        }
        for g in groups {
            let mut vals = Vec::with_capacity(g.states.len());
            for s in &g.states {
                vals.push(s.finish()?);
            }
            self.aggs[agg.slot] = vals;
            for (c, row) in cursors.iter().zip(g.rows) {
                self.rows[*c] = row;
            }
            if let Some(h) = &agg.having {
                if self.eval(h)?.truth() != Some(true) {
                    continue;
                }
            }
            if !project(self)? {
                break;
            }
        }
        Ok(())
    }

    /// Run the nested loops from `level` inwards, calling `body` for every
    /// joined row. Returns `Ok(false)` if the query asked to stop.
    fn loop_level(
        &mut self,
        core: &Core,
        level: usize,
        body: &mut dyn FnMut(&mut Exec<'a>) -> Result<bool>,
    ) -> Result<bool> {
        let Some(src) = core.sources.get(level) else {
            return body(self);
        };
        let cur = src.cursor;
        let mut matched = false;
        let go = self.scan_source(src, &mut |me, row| {
            me.rows[cur] = row;
            if !src.on_filters.is_empty() && !me.passes(&src.on_filters)? {
                return Ok(true);
            }
            matched = true;
            if !me.passes(&src.filters)? {
                return Ok(true);
            }
            me.loop_level(core, level + 1, body)
        })?;
        if !go {
            return Ok(false);
        }
        if src.left_join && !matched {
            self.rows[cur] = self.empty_row.clone();
            if self.passes(&src.filters)? {
                return self.loop_level(core, level + 1, body);
            }
        }
        Ok(true)
    }

    fn key_value(&mut self, e: &Expr, aff: Affinity) -> Result<Value> {
        Ok(apply_cmp_aff(self.eval(e)?, aff))
    }

    fn probe_key(&mut self, h: &HashProbe) -> Result<Option<Vec<u8>>> {
        let mut key = Vec::new();
        for (e, (aff, coll)) in h.probes.iter().zip(&h.cmps) {
            let v = self.key_value(e, *aff)?;
            if v.is_null() {
                return Ok(None);
            }
            hash_key(&v, *coll, &mut key);
        }
        Ok(Some(key))
    }

    fn build_hash(&self, rows: Vec<Row>, h: &HashProbe) -> HashTable {
        let mut map: HashMap<Vec<u8>, Vec<u32>> = HashMap::new();
        'rows: for (i, row) in rows.iter().enumerate() {
            let mut key = Vec::new();
            for (col, (aff, coll)) in h.cols.iter().zip(&h.cmps) {
                let v = apply_cmp_aff(row.get(*col).cloned().unwrap_or(Value::Null), *aff);
                if v.is_null() {
                    continue 'rows;
                }
                hash_key(&v, *coll, &mut key);
            }
            map.entry(key).or_default().push(i as u32);
        }
        HashTable { rows, map }
    }

    /// Produce the rows of one FROM source for the current outer rows.
    fn scan_source(
        &mut self,
        src: &Source,
        f: &mut dyn FnMut(&mut Exec<'a>, Row) -> Result<bool>,
    ) -> Result<bool> {
        let cur = src.cursor;
        self.stats[cur].loops += 1;
        match &src.kind {
            SourceKind::Table { table, access } => self.scan_table(cur, table, access, f),
            SourceKind::Subquery {
                query,
                correlated,
                hash,
            } => {
                let rows = match self.mat_cache.get(&cur) {
                    Some(r) if !*correlated => r.clone(),
                    _ => {
                        let rows: Vec<Row> =
                            self.collect(query)?.into_iter().map(Rc::from).collect();
                        let rows = Rc::new(rows);
                        if !*correlated {
                            self.mat_cache.insert(cur, rows.clone());
                        }
                        rows
                    }
                };
                if let Some(h) = hash {
                    let Some(key) = self.probe_key(h)? else {
                        return Ok(true);
                    };
                    let table = match self.hash_cache.get(&cur) {
                        Some(t) if !*correlated => t.clone(),
                        _ => {
                            let t = Rc::new(self.build_hash(rows.as_ref().clone(), h));
                            if !*correlated {
                                self.hash_cache.insert(cur, t.clone());
                            }
                            t
                        }
                    };
                    if let Some(hits) = table.map.get(&key) {
                        for i in hits {
                            self.tick(cur)?;
                            if !f(self, table.rows[*i as usize].clone())? {
                                return Ok(false);
                            }
                        }
                    }
                    return Ok(true);
                }
                for row in rows.iter() {
                    self.tick(cur)?;
                    if !f(self, row.clone())? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
        }
    }

    fn scan_table(
        &mut self,
        cur: usize,
        table: &Rc<Table>,
        access: &Access,
        f: &mut dyn FnMut(&mut Exec<'a>, Row) -> Result<bool>,
    ) -> Result<bool> {
        let mask = self.used.get(cur).map(|m| m.as_slice());
        let enc = self.enc;
        match access {
            Access::Scan => {
                let mut c = Cursor::new(self.pager.clone(), table.root);
                c.first()?;
                while c.valid() {
                    let rowid = if table.without_rowid { 0 } else { c.rowid()? };
                    let row = decode_table_row(table, c.payload()?.as_slice(), rowid, enc, mask)?;
                    self.tick(cur)?;
                    if !f(self, row)? {
                        return Ok(false);
                    }
                    c.next()?;
                }
                Ok(true)
            }
            Access::Rowid(e) => {
                let target = match self.eval(e)?.apply_numeric_for_compare() {
                    Value::Int(i) => i,
                    Value::Real(r) if r == r.trunc() && r.abs() < 9.2e18 => r as i64,
                    _ => return Ok(true),
                };
                let mut c = Cursor::new(self.pager.clone(), table.root);
                c.seek_rowid(target)?;
                if c.valid() && c.rowid()? == target {
                    let row = decode_table_row(table, c.payload()?.as_slice(), target, enc, mask)?;
                    self.tick(cur)?;
                    return f(self, row);
                }
                Ok(true)
            }
            Access::RowidRange { lo, hi } => {
                let mut start = i64::MIN;
                let mut end = i64::MAX;
                if let Some((e, inclusive)) = lo {
                    match self.eval(e)?.apply_numeric_for_compare() {
                        Value::Int(i) => {
                            start = if *inclusive {
                                i
                            } else if i == i64::MAX {
                                return Ok(true);
                            } else {
                                i + 1
                            }
                        }
                        Value::Real(r) => {
                            if r >= 9223372036854775808.0 {
                                return Ok(true);
                            }
                            let c = r.ceil();
                            start = real_to_int(c);
                            if c == r && !*inclusive {
                                start = start.saturating_add(1);
                            }
                        }
                        // rowid > NULL, rowid > 'text' and rowid > blob are never true.
                        _ => return Ok(true),
                    }
                }
                if let Some((e, inclusive)) = hi {
                    match self.eval(e)?.apply_numeric_for_compare() {
                        Value::Int(i) => {
                            end = if *inclusive {
                                i
                            } else if i == i64::MIN {
                                return Ok(true);
                            } else {
                                i - 1
                            }
                        }
                        Value::Real(r) => {
                            if r < -9223372036854775808.0 {
                                return Ok(true);
                            }
                            let fl = r.floor();
                            end = real_to_int(fl);
                            if fl == r && !*inclusive {
                                end = end.saturating_sub(1);
                            }
                        }
                        Value::Null => return Ok(true),
                        // Every rowid is less than any text or blob.
                        _ => {}
                    }
                }
                let mut c = Cursor::new(self.pager.clone(), table.root);
                c.seek_rowid(start)?;
                while c.valid() {
                    let rowid = c.rowid()?;
                    if rowid > end {
                        break;
                    }
                    let row = decode_table_row(table, c.payload()?.as_slice(), rowid, enc, mask)?;
                    self.tick(cur)?;
                    if !f(self, row)? {
                        return Ok(false);
                    }
                    c.next()?;
                }
                Ok(true)
            }
            Access::Index(ix) => self.scan_index(cur, table, ix, f),
            Access::Hash(h) => {
                let Some(key) = self.probe_key(h)? else {
                    return Ok(true);
                };
                let ht = match self.hash_cache.get(&cur) {
                    Some(t) => t.clone(),
                    None => {
                        let mut rows = Vec::new();
                        let mut c = Cursor::new(self.pager.clone(), table.root);
                        c.first()?;
                        while c.valid() {
                            let rowid = if table.without_rowid { 0 } else { c.rowid()? };
                            rows.push(decode_table_row(
                                table,
                                c.payload()?.as_slice(),
                                rowid,
                                enc,
                                mask,
                            )?);
                            c.next()?;
                        }
                        let t = Rc::new(self.build_hash(rows, h));
                        self.hash_cache.insert(cur, t.clone());
                        t
                    }
                };
                if let Some(hits) = ht.map.get(&key) {
                    for i in hits {
                        self.tick(cur)?;
                        if !f(self, ht.rows[*i as usize].clone())? {
                            return Ok(false);
                        }
                    }
                }
                Ok(true)
            }
        }
    }

    fn scan_index(
        &mut self,
        cur: usize,
        table: &Rc<Table>,
        ix: &IndexAccess,
        f: &mut dyn FnMut(&mut Exec<'a>, Row) -> Result<bool>,
    ) -> Result<bool> {
        let index = &ix.index;
        let enc = self.enc;
        let mask = self.used.get(cur).map(|m| m.as_slice());
        let mut key = Vec::with_capacity(ix.eq.len() + 1);
        for (e, aff) in &ix.eq {
            let v = self.key_value(e, *aff)?;
            if v.is_null() {
                return Ok(true);
            }
            key.push(v);
        }
        let bound = |me: &mut Exec<'a>,
                     b: &Option<(Expr, Affinity, bool)>|
         -> Result<Option<Option<Value>>> {
            match b {
                None => Ok(Some(None)),
                Some((e, aff, _)) => {
                    let v = me.key_value(e, *aff)?;
                    // A comparison with NULL matches nothing.
                    Ok(if v.is_null() { None } else { Some(Some(v)) })
                }
            }
        };
        let Some(lo) = bound(self, &ix.lo)? else {
            return Ok(true);
        };
        let Some(hi) = bound(self, &ix.hi)? else {
            return Ok(true);
        };
        let m = key.len();
        let range_col = index.cols.get(m);
        let desc = range_col.is_some_and(|c| c.desc);
        // In index order a DESC column runs from high values to low.
        let (start, end) = if desc { (hi, lo) } else { (lo, hi) };
        let mut seek_key = key.clone();
        if let Some(v) = start {
            seek_key.push(v);
        }
        let mut icur = Cursor::new(self.pager.clone(), index.root);
        if seek_key.is_empty() {
            icur.first()?;
        } else {
            icur.seek(&mut |payload| compare_entry(payload, &seek_key, index, enc))?;
        }
        let mut tcur = Cursor::new(self.pager.clone(), table.root);
        let ncols = table.columns.len();
        while icur.valid() {
            let payload = icur.payload()?;
            let vals = decode_all(payload.as_slice(), enc)?;
            if vals.len() < index.cols.len() + 1 {
                return corrupt(format!("index {} entry has too few fields", index.name));
            }
            for (i, k) in key.iter().enumerate() {
                if vals[i].compare(k, index.cols[i].collation) != Ordering::Equal {
                    return Ok(true);
                }
            }
            if let (Some(end), Some(rc)) = (&end, range_col) {
                let o = vals[m].compare(end, rc.collation);
                if (if desc { o.reverse() } else { o }) == Ordering::Greater {
                    return Ok(true);
                }
            }
            let Value::Int(rowid) = vals[vals.len() - 1] else {
                return corrupt(format!("index {} entry has no rowid", index.name));
            };
            let row = if ix.covering {
                let mut out = vec![Value::Null; ncols + 1];
                for (i, ic) in index.cols.iter().enumerate() {
                    let mut v = vals[i].clone();
                    if table.columns[ic.col].affinity == Affinity::Real {
                        if let Value::Int(n) = v {
                            v = Value::Real(n as f64);
                        }
                    }
                    out[ic.col] = v;
                }
                if let Some(ipk) = table.ipk {
                    out[ipk] = Value::Int(rowid);
                }
                out[ncols] = Value::Int(rowid);
                Rc::from(out)
            } else {
                tcur.seek_rowid(rowid)?;
                if !tcur.valid() || tcur.rowid()? != rowid {
                    return corrupt(format!(
                        "index {} points at missing row {rowid}",
                        index.name
                    ));
                }
                decode_table_row(table, tcur.payload()?.as_slice(), rowid, enc, mask)?
            };
            self.tick(cur)?;
            if !f(self, row)? {
                return Ok(false);
            }
            icur.next()?;
        }
        Ok(true)
    }

    // ------------------------------------------------------------ expressions

    fn compare(
        &mut self,
        op: BinaryOp,
        l: Value,
        r: Value,
        aff: Affinity,
        coll: Collation,
    ) -> Value {
        let (l, r) = (apply_cmp_aff(l, aff), apply_cmp_aff(r, aff));
        if matches!(op, BinaryOp::Is | BinaryOp::IsNot) {
            let same = match (l.is_null(), r.is_null()) {
                (true, true) => true,
                (false, false) => l.compare(&r, coll) == Ordering::Equal,
                _ => false,
            };
            return Value::Int((same == (op == BinaryOp::Is)) as i64);
        }
        if l.is_null() || r.is_null() {
            return Value::Null;
        }
        let o = l.compare(&r, coll);
        Value::Int(match op {
            BinaryOp::Eq => o == Ordering::Equal,
            BinaryOp::Ne => o != Ordering::Equal,
            BinaryOp::Lt => o == Ordering::Less,
            BinaryOp::Le => o != Ordering::Greater,
            BinaryOp::Gt => o == Ordering::Greater,
            _ => o != Ordering::Less,
        } as i64)
    }

    fn in_set(&mut self, sub: &Rc<Subquery>, aff: Affinity, coll: Collation) -> Result<Rc<InSet>> {
        if sub.outer_refs.is_empty() {
            if let Some(SubCache::Set(s)) = self.sub_cache.get(&sub.id) {
                return Ok(s.clone());
            }
        }
        let mut set = InSet {
            keys: HashSet::new(),
            has_null: false,
            empty: true,
        };
        self.run_query(&sub.query, &mut |row| {
            set.empty = false;
            let v = apply_cmp_aff(row.into_iter().next().unwrap_or(Value::Null), aff);
            if v.is_null() {
                set.has_null = true;
            } else {
                let mut key = Vec::new();
                hash_key(&v, coll, &mut key);
                set.keys.insert(key);
            }
            Ok(true)
        })?;
        let set = Rc::new(set);
        if sub.outer_refs.is_empty() {
            self.sub_cache.insert(sub.id, SubCache::Set(set.clone()));
        }
        Ok(set)
    }

    pub fn eval(&mut self, e: &Expr) -> Result<Value> {
        Ok(match e {
            Expr::Lit(v) => v.clone(),
            Expr::Col { cur, col, .. } => self.rows[*cur].get(*col).cloned().unwrap_or(Value::Null),
            Expr::Agg { slot, idx } => self.aggs[*slot].get(*idx).cloned().unwrap_or(Value::Null),
            Expr::Collate(inner, _) => self.eval(inner)?,
            Expr::Unary(op, a) => {
                let v = self.eval(a)?;
                match op {
                    UnaryOp::Pos => v,
                    UnaryOp::Not => bool_value(v.truth().map(|b| !b)),
                    UnaryOp::BitNot => {
                        if v.is_null() {
                            Value::Null
                        } else {
                            Value::Int(!v.to_int())
                        }
                    }
                    UnaryOp::Neg => match v.to_numeric() {
                        Value::Int(i) => i
                            .checked_neg()
                            .map(Value::Int)
                            .unwrap_or(Value::Real(-(i as f64))),
                        Value::Real(r) => Value::Real(-r),
                        other => other,
                    },
                }
            }
            Expr::Arith(op, a, b) => {
                let l = self.eval(a)?;
                let r = self.eval(b)?;
                arith(*op, &l, &r)
            }
            Expr::And(a, b) => {
                let l = self.eval(a)?.truth();
                if l == Some(false) {
                    return Ok(Value::Int(0));
                }
                let r = self.eval(b)?.truth();
                bool_value(match (l, r) {
                    (_, Some(false)) => Some(false),
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                })
            }
            Expr::Or(a, b) => {
                let l = self.eval(a)?.truth();
                if l == Some(true) {
                    return Ok(Value::Int(1));
                }
                let r = self.eval(b)?.truth();
                bool_value(match (l, r) {
                    (_, Some(true)) => Some(true),
                    (Some(false), Some(false)) => Some(false),
                    _ => None,
                })
            }
            Expr::Cmp {
                op,
                l,
                r,
                aff,
                coll,
            } => {
                let lv = self.eval(l)?;
                let rv = self.eval(r)?;
                self.compare(*op, lv, rv, *aff, *coll)
            }
            Expr::IsNull(a, negated) => Value::Int((self.eval(a)?.is_null() != *negated) as i64),
            Expr::Between {
                e,
                lo,
                hi,
                negated,
                lo_cmp,
                hi_cmp,
            } => {
                let v = self.eval(e)?;
                let lov = self.eval(lo)?;
                let hiv = self.eval(hi)?;
                let a = self
                    .compare(BinaryOp::Ge, v.clone(), lov, lo_cmp.0, lo_cmp.1)
                    .truth();
                let b = self
                    .compare(BinaryOp::Le, v, hiv, hi_cmp.0, hi_cmp.1)
                    .truth();
                let both = match (a, b) {
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                };
                bool_value(both.map(|x| x != *negated))
            }
            Expr::InList {
                e,
                list,
                negated,
                aff,
                coll,
            } => {
                let v = self.eval(e)?;
                if list.is_empty() {
                    return Ok(Value::Int(*negated as i64));
                }
                if v.is_null() {
                    return Ok(Value::Null);
                }
                let v = apply_cmp_aff(v, *aff);
                let mut saw_null = false;
                let mut found = false;
                for item in list {
                    let x = apply_cmp_aff(self.eval(item)?, *aff);
                    if x.is_null() {
                        saw_null = true;
                    } else if v.compare(&x, *coll) == Ordering::Equal {
                        found = true;
                        break;
                    }
                }
                if found {
                    Value::Int(!*negated as i64)
                } else if saw_null {
                    Value::Null
                } else {
                    Value::Int(*negated as i64)
                }
            }
            Expr::InSub {
                e,
                sub,
                negated,
                aff,
                coll,
            } => {
                let v = self.eval(e)?;
                let set = self.in_set(sub, *aff, *coll)?;
                if set.empty {
                    return Ok(Value::Int(*negated as i64));
                }
                if v.is_null() {
                    return Ok(Value::Null);
                }
                let mut key = Vec::new();
                hash_key(&apply_cmp_aff(v, *aff), *coll, &mut key);
                if set.keys.contains(&key) {
                    Value::Int(!*negated as i64)
                } else if set.has_null {
                    Value::Null
                } else {
                    Value::Int(*negated as i64)
                }
            }
            Expr::Like {
                op,
                e,
                pat,
                esc,
                negated,
            } => {
                let v = self.eval(e)?;
                let p = self.eval(pat)?;
                let esc = match esc {
                    Some(x) => {
                        let ev = self.eval(x)?;
                        if ev.is_null() {
                            return Ok(Value::Null);
                        }
                        escape_char(&ev)?
                    }
                    None => None,
                };
                if v.is_null() || p.is_null() {
                    return Ok(Value::Null);
                }
                let hit = match op {
                    LikeOp::Glob => glob(&p.to_text(), &v.to_text()),
                    _ => like(&p.to_text(), &v.to_text(), esc),
                };
                Value::Int((hit != *negated) as i64)
            }
            Expr::Case {
                operand,
                whens,
                else_,
            } => {
                let base = match operand {
                    Some(o) => Some(self.eval(o)?),
                    None => None,
                };
                for w in whens {
                    let hit = match &base {
                        Some(b) => {
                            let wv = self.eval(&w.when)?;
                            self.compare(BinaryOp::Eq, b.clone(), wv, w.aff, w.coll)
                                .truth()
                                == Some(true)
                        }
                        None => self.eval(&w.when)?.truth() == Some(true),
                    };
                    if hit {
                        return self.eval(&w.then);
                    }
                }
                match else_ {
                    Some(x) => self.eval(x)?,
                    None => Value::Null,
                }
            }
            Expr::Cast(a, aff) => cast(self.eval(a)?, *aff),
            Expr::Func { f, args, .. } => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval(a)?);
                }
                f(&vals)?
            }
            Expr::Subquery(sub) => {
                let cacheable = sub.outer_refs.is_empty();
                if cacheable {
                    if let Some(SubCache::Scalar(v)) = self.sub_cache.get(&sub.id) {
                        return Ok(v.clone());
                    }
                }
                let mut out = Value::Null;
                self.run_query(&sub.query, &mut |row| {
                    out = row.into_iter().next().unwrap_or(Value::Null);
                    Ok(false)
                })?;
                if cacheable {
                    self.sub_cache.insert(sub.id, SubCache::Scalar(out.clone()));
                }
                out
            }
            Expr::Exists(sub) => {
                let cacheable = sub.outer_refs.is_empty();
                if cacheable {
                    if let Some(SubCache::Exists(b)) = self.sub_cache.get(&sub.id) {
                        return Ok(Value::Int(*b as i64));
                    }
                }
                let mut any = false;
                self.run_query(&sub.query, &mut |_| {
                    any = true;
                    Ok(false)
                })?;
                if cacheable {
                    self.sub_cache.insert(sub.id, SubCache::Exists(any));
                }
                Value::Int(any as i64)
            }
        })
    }
}

/// Evaluate an expression that reads no rows (a DEFAULT, a LIMIT, ...).
pub fn eval_const(pager: &Rc<Pager>, e: &Expr) -> Result<Value> {
    let plan = Plan {
        query: Query {
            body: Body::Values(Vec::new()),
            order_by: Vec::new(),
            limit: None,
            offset: None,
            cols: Vec::new(),
        },
        used: Vec::new(),
        n_agg_slots: 0,
        n_subqueries: 0,
    };
    let mut ex = Exec::new(pager.clone(), &plan);
    match e {
        Expr::Col { .. } => Err(Error::Sql("expression is not constant".into())),
        _ => ex.eval(e),
    }
}
