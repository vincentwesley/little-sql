//! The bound query plan: what the binder produces and the executor runs.
//!
//! Every name has been resolved. A column reference is a *cursor* number
//! (one per table or subquery in any FROM clause of the statement) plus a
//! column index, so evaluating it is two array lookups. Because cursor
//! numbers are unique across the whole statement, a correlated subquery
//! reads its outer query's current row with no extra machinery.

use crate::func::{AggFunc, ScalarFn};
use crate::schema::{Index, Table};
use crate::sql::ast::{BinaryOp, CompoundOp, LikeOp, UnaryOp};
use crate::value::{Affinity, Collation, Value};
use std::rc::Rc;

#[derive(Clone)]
pub enum Expr {
    Lit(Value),
    /// Column `col` of the current row of cursor `cur`. For tables the
    /// index one past the last column is the rowid.
    Col {
        cur: usize,
        col: usize,
        aff: Affinity,
        coll: Collation,
    },
    Unary(UnaryOp, Box<Expr>),
    /// Arithmetic, bitwise and concatenation operators.
    Arith(BinaryOp, Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    /// A comparison with the affinity to apply to both sides first and the
    /// collation to compare text with.
    Cmp {
        op: BinaryOp,
        l: Box<Expr>,
        r: Box<Expr>,
        aff: Affinity,
        coll: Collation,
    },
    IsNull(Box<Expr>, bool),
    Between {
        e: Box<Expr>,
        lo: Box<Expr>,
        hi: Box<Expr>,
        negated: bool,
        lo_cmp: (Affinity, Collation),
        hi_cmp: (Affinity, Collation),
    },
    InList {
        e: Box<Expr>,
        list: Vec<Expr>,
        negated: bool,
        aff: Affinity,
        coll: Collation,
    },
    InSub {
        e: Box<Expr>,
        sub: Rc<Subquery>,
        negated: bool,
        aff: Affinity,
        coll: Collation,
    },
    Like {
        op: LikeOp,
        e: Box<Expr>,
        pat: Box<Expr>,
        esc: Option<Box<Expr>>,
        negated: bool,
    },
    Case {
        operand: Option<Box<Expr>>,
        whens: Vec<CaseWhen>,
        else_: Option<Box<Expr>>,
    },
    Cast(Box<Expr>, Affinity),
    Collate(Box<Expr>, Collation),
    Func {
        name: &'static str,
        f: ScalarFn,
        args: Vec<Expr>,
    },
    /// Result `idx` of the aggregation with the given slot.
    Agg {
        slot: usize,
        idx: usize,
    },
    Subquery(Rc<Subquery>),
    Exists(Rc<Subquery>),
}

#[derive(Clone)]
pub struct CaseWhen {
    pub when: Expr,
    pub then: Expr,
    /// Comparison rules against the CASE operand (unused without one).
    pub aff: Affinity,
    pub coll: Collation,
}

/// A subquery used inside an expression.
pub struct Subquery {
    pub id: usize,
    pub query: Query,
    /// Cursors of enclosing queries that the subquery reads. Empty means
    /// the result is the same for every outer row and can be cached.
    pub outer_refs: Vec<usize>,
    /// The right-hand side of `IN (SELECT ...)`, rather than a scalar or
    /// EXISTS subquery.
    pub list: bool,
}

#[derive(Clone)]
pub struct ColInfo {
    pub name: String,
    pub aff: Affinity,
    pub coll: Collation,
}

pub struct Query {
    pub body: Body,
    pub order_by: Vec<OrderKey>,
    pub limit: Option<Expr>,
    pub offset: Option<Expr>,
    pub cols: Vec<ColInfo>,
}

pub enum Body {
    Core(Box<Core>),
    Values(Vec<Vec<Expr>>),
    Compound {
        op: CompoundOp,
        left: Box<Query>,
        right: Box<Query>,
    },
    /// `WITH RECURSIVE name AS (init UNION [ALL] step)`: rows of `init`
    /// seed a queue; each row taken from the queue is returned and becomes
    /// the whole of work table `work` while `step` runs once.
    Recursive {
        init: Box<Query>,
        step: Box<Query>,
        union_all: bool,
        work: usize,
    },
    /// The current row of a recursive CTE, read inside its step.
    WorkTable(usize),
}

pub enum KeySrc {
    /// Sort by result column `i`.
    Result(usize),
    /// Sort by an expression that is not in the result list.
    Expr(Expr),
}

pub struct OrderKey {
    pub src: KeySrc,
    pub desc: bool,
    pub nulls_first: bool,
    pub coll: Collation,
}

/// One `SELECT ... FROM ... WHERE ... GROUP BY ...`.
pub struct Core {
    /// FROM sources in the order the nested loops run (outermost first).
    pub sources: Vec<Source>,
    /// WHERE terms that mention no table of this query; checked once.
    pub const_filters: Vec<Expr>,
    pub results: Vec<Expr>,
    pub distinct: bool,
    pub agg: Option<Agg>,
}

pub struct Agg {
    pub slot: usize,
    pub group_by: Vec<(Expr, Collation)>,
    /// Rows arrive in GROUP BY order, so each group is finished as soon as
    /// the next one starts (no hash table).
    pub sorted: bool,
    pub calls: Vec<AggCall>,
    pub having: Option<Expr>,
}

pub struct AggCall {
    pub func: AggFunc,
    pub args: Vec<Expr>,
    pub distinct: bool,
    pub filter: Option<Expr>,
    pub coll: Collation,
}

pub struct Source {
    pub cursor: usize,
    /// Name shown in EXPLAIN (alias or table name).
    pub name: String,
    pub kind: SourceKind,
    pub left_join: bool,
    /// Visit rows in descending key order (to deliver an ORDER BY ... DESC
    /// without sorting).
    pub reverse: bool,
    /// LEFT JOIN: ON terms that decide whether a row matches.
    pub on_filters: Vec<Expr>,
    /// WHERE (and inner-join ON) terms that can be checked once this source
    /// has a row.
    pub filters: Vec<Expr>,
    pub ncols: usize,
}

pub enum SourceKind {
    Table {
        table: Rc<Table>,
        access: Access,
    },
    Subquery {
        query: Box<Query>,
        correlated: bool,
        hash: Option<HashProbe>,
    },
}

/// Equality lookup through a transient hash table built over the source
/// (SQLite's "automatic index").
pub struct HashProbe {
    /// Columns of this source that form the key.
    pub cols: Vec<usize>,
    /// Expressions over outer rows, one per key column.
    pub probes: Vec<Expr>,
    pub cmps: Vec<(Affinity, Collation)>,
}

/// The value(s) an equality lookup searches for.
#[derive(Clone)]
pub enum KeyExpr {
    /// `col = expr`
    One(Expr),
    /// `col IN (e1, e2, ...)`: one lookup per distinct value, in key order.
    List(Vec<Expr>),
    /// `col IN (SELECT ...)`: one lookup per distinct value of the result.
    Sub(Rc<Subquery>),
}

impl KeyExpr {
    pub fn is_one(&self) -> bool {
        matches!(self, KeyExpr::One(_))
    }
}

pub enum Access {
    /// Visit every row.
    Scan,
    /// `rowid = expr` or `rowid IN (...)`
    Rowid(KeyExpr),
    /// `rowid` between optional bounds; the flag says "inclusive".
    RowidRange {
        lo: Option<(Expr, bool)>,
        hi: Option<(Expr, bool)>,
    },
    Index(IndexAccess),
    Hash(HashProbe),
}

pub struct IndexAccess {
    pub index: Rc<Index>,
    /// Equality keys for the leading index columns, with the affinity to
    /// apply to each key value.
    pub eq: Vec<(KeyExpr, Affinity)>,
    /// Optional bounds on the next index column: (value, affinity, inclusive).
    pub lo: Option<(Expr, Affinity, bool)>,
    pub hi: Option<(Expr, Affinity, bool)>,
    /// True when the index holds every column the query reads, so the table
    /// itself is never touched.
    pub covering: bool,
}

/// The complete plan for one statement.
pub struct Plan {
    pub query: Query,
    /// For each cursor, which columns the statement reads.
    pub used: Vec<Vec<bool>>,
    pub n_agg_slots: usize,
    pub n_subqueries: usize,
}

// ------------------------------------------------------------------ helpers

/// Affinity of an expression (BLOB means none).
pub fn affinity(e: &Expr) -> Affinity {
    match e {
        Expr::Col { aff, .. } => *aff,
        Expr::Cast(_, aff) => *aff,
        Expr::Collate(inner, _) => affinity(inner),
        Expr::Subquery(s) => s.query.cols.first().map_or(Affinity::Blob, |c| c.aff),
        _ => Affinity::Blob,
    }
}

/// Collation of an expression and whether it was given explicitly.
pub fn collation(e: &Expr) -> Option<(Collation, bool)> {
    match e {
        Expr::Collate(_, c) => Some((*c, true)),
        Expr::Col { coll, .. } => Some((*coll, false)),
        Expr::Cast(inner, _) | Expr::Unary(UnaryOp::Pos, inner) => collation(inner),
        Expr::Unary(_, inner) => collation(inner).filter(|c| c.1),
        Expr::Arith(_, l, r) => {
            // An explicit COLLATE anywhere inside wins; the left side first.
            collation(l)
                .filter(|c| c.1)
                .or_else(|| collation(r).filter(|c| c.1))
        }
        Expr::Subquery(s) => s.query.cols.first().map(|c| (c.coll, false)),
        _ => None,
    }
}

/// Collation used to compare `l` with `r`.
pub fn cmp_collation(l: &Expr, r: &Expr) -> Collation {
    let (cl, cr) = (collation(l), collation(r));
    match (cl, cr) {
        (Some((c, true)), _) => c,
        (_, Some((c, true))) => c,
        (Some((c, _)), _) => c,
        (_, Some((c, _))) => c,
        _ => Collation::Binary,
    }
}

/// Affinity applied to both operands of a comparison.
pub fn cmp_affinity(a: Affinity, b: Affinity) -> Affinity {
    let norm = |x: Affinity| if x.is_numeric() { Affinity::Numeric } else { x };
    if a != Affinity::Blob && b != Affinity::Blob {
        if a.is_numeric() || b.is_numeric() {
            Affinity::Numeric
        } else {
            Affinity::Blob
        }
    } else if a != Affinity::Blob {
        norm(a)
    } else {
        norm(b)
    }
}

/// Collect the cursors an expression reads, including those read from
/// inside its subqueries.
pub fn refs(e: &Expr, out: &mut Vec<usize>) {
    match e {
        Expr::Lit(_) | Expr::Agg { .. } => {}
        Expr::Col { cur, .. } => out.push(*cur),
        Expr::Unary(_, a) | Expr::IsNull(a, _) | Expr::Cast(a, _) | Expr::Collate(a, _) => {
            refs(a, out)
        }
        Expr::Arith(_, a, b) | Expr::And(a, b) | Expr::Or(a, b) => {
            refs(a, out);
            refs(b, out);
        }
        Expr::Cmp { l, r, .. } => {
            refs(l, out);
            refs(r, out);
        }
        Expr::Between { e, lo, hi, .. } => {
            refs(e, out);
            refs(lo, out);
            refs(hi, out);
        }
        Expr::InList { e, list, .. } => {
            refs(e, out);
            list.iter().for_each(|x| refs(x, out));
        }
        Expr::InSub { e, sub, .. } => {
            refs(e, out);
            out.extend_from_slice(&sub.outer_refs);
        }
        Expr::Like { e, pat, esc, .. } => {
            refs(e, out);
            refs(pat, out);
            if let Some(x) = esc {
                refs(x, out);
            }
        }
        Expr::Case {
            operand,
            whens,
            else_,
        } => {
            if let Some(o) = operand {
                refs(o, out);
            }
            for w in whens {
                refs(&w.when, out);
                refs(&w.then, out);
            }
            if let Some(x) = else_ {
                refs(x, out);
            }
        }
        Expr::Func { args, .. } => args.iter().for_each(|x| refs(x, out)),
        Expr::Subquery(s) | Expr::Exists(s) => out.extend_from_slice(&s.outer_refs),
    }
}

/// Visit every subquery that appears directly inside `e`.
pub fn subqueries<'a>(e: &'a Expr, out: &mut Vec<&'a Rc<Subquery>>) {
    match e {
        Expr::Lit(_) | Expr::Agg { .. } | Expr::Col { .. } => {}
        Expr::Unary(_, a) | Expr::IsNull(a, _) | Expr::Cast(a, _) | Expr::Collate(a, _) => {
            subqueries(a, out)
        }
        Expr::Arith(_, a, b) | Expr::And(a, b) | Expr::Or(a, b) => {
            subqueries(a, out);
            subqueries(b, out);
        }
        Expr::Cmp { l, r, .. } => {
            subqueries(l, out);
            subqueries(r, out);
        }
        Expr::Between { e, lo, hi, .. } => {
            subqueries(e, out);
            subqueries(lo, out);
            subqueries(hi, out);
        }
        Expr::InList { e, list, .. } => {
            subqueries(e, out);
            list.iter().for_each(|x| subqueries(x, out));
        }
        Expr::InSub { e, sub, .. } => {
            subqueries(e, out);
            out.push(sub);
        }
        Expr::Like { e, pat, esc, .. } => {
            subqueries(e, out);
            subqueries(pat, out);
            if let Some(x) = esc {
                subqueries(x, out);
            }
        }
        Expr::Case {
            operand,
            whens,
            else_,
        } => {
            if let Some(o) = operand {
                subqueries(o, out);
            }
            for w in whens {
                subqueries(&w.when, out);
                subqueries(&w.then, out);
            }
            if let Some(x) = else_ {
                subqueries(x, out);
            }
        }
        Expr::Func { args, .. } => args.iter().for_each(|x| subqueries(x, out)),
        Expr::Subquery(s) | Expr::Exists(s) => out.push(s),
    }
}

// ------------------------------------------------------------------ EXPLAIN

/// Per-cursor counters gathered while a statement runs.
#[derive(Clone, Copy, Default, Debug)]
pub struct CursorStats {
    /// How many times the loop over this source was started.
    pub loops: u64,
    /// How many rows the source produced (before filters).
    pub rows: u64,
}

/// One line of the EXPLAIN tree.
#[derive(Clone, Debug)]
pub struct PlanLine {
    pub depth: usize,
    pub text: String,
    /// The cursor this line describes, when it describes a table access.
    pub cursor: Option<usize>,
}

fn op_text(inclusive: bool, lower: bool) -> &'static str {
    match (lower, inclusive) {
        (true, true) => ">=",
        (true, false) => ">",
        (false, true) => "<=",
        (false, false) => "<",
    }
}

fn describe_source(s: &Source) -> String {
    let mut out = match &s.kind {
        SourceKind::Table { table, access } => {
            // Like SQLite 3.36+, an aliased table is shown by its alias.
            let named = s.name.clone();
            match access {
                Access::Scan => format!("SCAN {named}"),
                Access::Rowid(_) => format!("SEARCH {named} USING INTEGER PRIMARY KEY (rowid=?)"),
                Access::RowidRange { lo, hi } => {
                    // SQLite prints rowid ranges as > and < whether or not
                    // the bounds are inclusive.
                    let mut terms = Vec::new();
                    if lo.is_some() {
                        terms.push("rowid>?".to_string());
                    }
                    if hi.is_some() {
                        terms.push("rowid<?".to_string());
                    }
                    format!(
                        "SEARCH {named} USING INTEGER PRIMARY KEY ({})",
                        terms.join(" AND ")
                    )
                }
                Access::Index(ix) => {
                    let col = |i: usize| table.columns[ix.index.cols[i].col].name.clone();
                    let mut terms: Vec<String> =
                        (0..ix.eq.len()).map(|i| format!("{}=?", col(i))).collect();
                    if let Some((_, _, inc)) = &ix.lo {
                        terms.push(format!("{}{}?", col(ix.eq.len()), op_text(*inc, true)));
                    }
                    if let Some((_, _, inc)) = &ix.hi {
                        terms.push(format!("{}{}?", col(ix.eq.len()), op_text(*inc, false)));
                    }
                    let which = if ix.index.primary {
                        "PRIMARY KEY".to_string()
                    } else {
                        format!(
                            "{}INDEX {}",
                            if ix.covering { "COVERING " } else { "" },
                            ix.index.name
                        )
                    };
                    match (terms.is_empty(), ix.index.primary) {
                        (true, true) => format!("SCAN {named}"),
                        (true, false) => format!("SCAN {named} USING {which}"),
                        _ => format!("SEARCH {named} USING {which} ({})", terms.join(" AND ")),
                    }
                }
                Access::Hash(h) => {
                    let terms: Vec<String> = h
                        .cols
                        .iter()
                        .map(|c| format!("{}=?", table.columns[*c].name))
                        .collect();
                    format!(
                        "SEARCH {named} USING AUTOMATIC HASH INDEX ({})",
                        terms.join(" AND ")
                    )
                }
            }
        }
        SourceKind::Subquery { hash, .. } => match hash {
            Some(h) => format!(
                "SEARCH SUBQUERY {} USING AUTOMATIC HASH INDEX ({} columns)",
                s.name,
                h.cols.len()
            ),
            None => format!("SCAN SUBQUERY {}", s.name),
        },
    };
    if s.left_join {
        out.push_str(" LEFT-JOIN");
    }
    out
}

fn explain_exprs<'a>(exprs: impl Iterator<Item = &'a Expr>, depth: usize, out: &mut Vec<PlanLine>) {
    let mut subs = Vec::new();
    for e in exprs {
        subqueries(e, &mut subs);
    }
    for s in subs {
        let kind = match (s.list, s.outer_refs.is_empty()) {
            (true, true) => "LIST",
            (true, false) => "CORRELATED LIST",
            (false, true) => "SCALAR",
            (false, false) => "CORRELATED SCALAR",
        };
        out.push(PlanLine {
            depth,
            text: format!("{kind} SUBQUERY {}", s.id + 1),
            cursor: None,
        });
        explain_query(&s.query, depth + 1, out);
    }
}

/// Render a query as EXPLAIN QUERY PLAN lines, in SQLite's vocabulary.
pub fn explain_query(q: &Query, depth: usize, out: &mut Vec<PlanLine>) {
    match &q.body {
        Body::Core(core) => {
            for s in &core.sources {
                if let SourceKind::Subquery {
                    query, correlated, ..
                } = &s.kind
                {
                    let how = if *correlated {
                        "CO-ROUTINE"
                    } else {
                        "MATERIALIZE"
                    };
                    out.push(PlanLine {
                        depth,
                        text: format!("{how} {}", s.name),
                        cursor: None,
                    });
                    explain_query(query, depth + 1, out);
                }
                out.push(PlanLine {
                    depth,
                    text: describe_source(s),
                    cursor: Some(s.cursor),
                });
                explain_exprs(s.on_filters.iter().chain(s.filters.iter()), depth, out);
            }
            explain_exprs(
                core.const_filters.iter().chain(core.results.iter()),
                depth,
                out,
            );
            if let Some(agg) = &core.agg {
                explain_exprs(
                    agg.calls
                        .iter()
                        .flat_map(|c| c.args.iter())
                        .chain(agg.having.iter()),
                    depth,
                    out,
                );
                if !agg.group_by.is_empty() && !agg.sorted {
                    out.push(PlanLine {
                        depth,
                        text: "USE TEMP HASH TABLE FOR GROUP BY".into(),
                        cursor: None,
                    });
                }
            }
            if core.distinct {
                out.push(PlanLine {
                    depth,
                    text: "USE TEMP HASH TABLE FOR DISTINCT".into(),
                    cursor: None,
                });
            }
        }
        Body::Values(rows) => {
            out.push(PlanLine {
                depth,
                text: format!(
                    "SCAN {} CONSTANT ROW{}",
                    rows.len(),
                    if rows.len() == 1 { "" } else { "S" }
                ),
                cursor: None,
            });
        }
        Body::Compound { op, left, right } => {
            out.push(PlanLine {
                depth,
                text: "COMPOUND QUERY".into(),
                cursor: None,
            });
            out.push(PlanLine {
                depth: depth + 1,
                text: "LEFT-MOST SUBQUERY".into(),
                cursor: None,
            });
            explain_query(left, depth + 2, out);
            let name = match op {
                CompoundOp::Union => "UNION USING TEMP B-TREE",
                CompoundOp::UnionAll => "UNION ALL",
                CompoundOp::Intersect => "INTERSECT USING TEMP B-TREE",
                CompoundOp::Except => "EXCEPT USING TEMP B-TREE",
            };
            out.push(PlanLine {
                depth: depth + 1,
                text: name.into(),
                cursor: None,
            });
            explain_query(right, depth + 2, out);
        }
        Body::Recursive {
            init,
            step,
            union_all,
            ..
        } => {
            out.push(PlanLine {
                depth,
                text: "SETUP".into(),
                cursor: None,
            });
            explain_query(init, depth + 1, out);
            out.push(PlanLine {
                depth,
                text: format!("RECURSIVE STEP{}", if *union_all { "" } else { " (UNION)" }),
                cursor: None,
            });
            explain_query(step, depth + 1, out);
        }
        Body::WorkTable(_) => {}
    }
    if !q.order_by.is_empty() {
        out.push(PlanLine {
            depth,
            text: "USE TEMP B-TREE FOR ORDER BY".into(),
            cursor: None,
        });
    }
}

/// Draw plan lines as an indented tree, optionally with run statistics.
pub fn render_plan(lines: &[PlanLine], stats: Option<&[CursorStats]>) -> String {
    let mut out = String::from("QUERY PLAN\n");
    for (i, line) in lines.iter().enumerate() {
        let mut prefix = String::new();
        for d in 0..line.depth {
            // Is there a later line at depth d before the tree returns above it?
            let more = lines[i + 1..]
                .iter()
                .take_while(|l| l.depth >= d)
                .any(|l| l.depth == d);
            prefix.push_str(if more { "|  " } else { "   " });
        }
        let last = !lines[i + 1..]
            .iter()
            .take_while(|l| l.depth >= line.depth)
            .any(|l| l.depth == line.depth);
        out.push_str(&prefix);
        out.push_str(if last { "`--" } else { "|--" });
        out.push_str(&line.text);
        if let (Some(stats), Some(cur)) = (stats, line.cursor) {
            if let Some(s) = stats.get(cur) {
                out.push_str(&format!("  (loops={} rows={})", s.loops, s.rows));
            }
        }
        out.push('\n');
    }
    out
}
