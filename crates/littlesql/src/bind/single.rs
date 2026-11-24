//! Planning a query over a single table with SQLite's own cost model.
//!
//! SQLite (where.c) lists every way to read the table (a full scan, a
//! scan of an index, searches on any prefix of an index's columns), gives
//! each a cost and an output row count in LogEst units, and picks the
//! cheapest, charging a sort to each plan that does not deliver the
//! ORDER BY by itself (or only delivers its first terms, in which case
//! only the rest is sorted). This module does the same, with the same
//! numbers (see `cost.rs`), so that it trades a sort against walking an
//! index in order exactly where SQLite does.

use super::*;
use crate::cost::{est_log, log_est, log_est_add, LogEst, TABLE_ROWS};

/// What a WHERE term means to SQLite's estimates.
struct TermInfo {
    /// For an `=` or `IS` comparison on an indexable expression: the cut
    /// SQLite assumes when it is not used by the index (10 when compared
    /// with -1, 0 or 1, else 20).
    eq_k: Option<LogEst>,
    /// Columns of the table it reads.
    cols: Vec<usize>,
    /// The extra terms SQLite derives from it (the two halves of BETWEEN,
    /// the commuted copy of `col = col`), each flagged when it is `=`.
    virtuals: Vec<bool>,
}

/// One way into the table: its own b-tree (rowid tables) or an index.
struct Probe<'a> {
    index: Option<Rc<Index>>,
    special: Option<&'a SpecialIndex>,
    /// `aiRowLogEst`: rows in the index, then rows per key prefix.
    row_est: Vec<LogEst>,
    sz_idx: LogEst,
    covering: bool,
    uniq_not_null: bool,
    /// Constraints on its expression keys (as stand-in columns).
    extra: Vec<TCon>,
}

impl Probe<'_> {
    fn nkey(&self) -> usize {
        self.index.as_ref().map_or(1, |i| i.cols.len())
    }
    /// Columns a search can constrain: the key, then (rowid tables) the
    /// rowid every entry ends with.
    fn ncol(&self, rowid_table: bool) -> usize {
        match &self.index {
            Some(ix) if rowid_table && !ix.primary => ix.cols.len() + 1,
            // WITHOUT ROWID: the rest of the primary key follows.
            Some(ix) if !ix.primary => ix.cols.len() + ix.extra.len(),
            _ => self.nkey(),
        }
    }
    fn is_rowid(&self) -> bool {
        self.index.is_none()
    }
}

/// A candidate plan for the table, with SQLite's cost and row estimate.
struct Cand {
    access: Access,
    r_run: LogEst,
    n_out: LogEst,
    probe: usize,
    /// Per equality column: whether it takes several values (IN).
    eq_multi: Vec<bool>,
    one_row: bool,
    /// The WHERE terms (and derived terms) it uses.
    used: Vec<TermKey>,
    /// Reads an index (SQLite's WHERE_INDEXED) / only the index.
    indexed: bool,
    idx_only: bool,
    /// Plans with different values are never pruned against each other
    /// (SQLite keeps an ordered scan of each index that may help the
    /// ORDER BY).
    sort_idx: usize,
}

/// A WHERE term, or one SQLite derives from it: (term, column, operator).
type TermKey = (usize, usize, BinaryOp);

/// A search constraint, with the WHERE term it comes from.
struct TCon {
    term: usize,
    /// Derived by SQLite rather than written (it is listed after the
    /// written terms).
    virt: bool,
    /// The `col > NULL` derived from `col IS NOT NULL`: it narrows
    /// nothing in the estimates.
    vnull: bool,
    /// `col IS NULL` (an equality on NULL).
    isnull: bool,
    /// A bound derived from `col LIKE 'prefix%'`: the two are used as a
    /// pair, never the upper one alone.
    likeopt: bool,
    c: Constraint,
}

impl TCon {
    fn key(&self) -> TermKey {
        (self.term, self.c.col, self.c.op)
    }
}

fn tcon_order(t: &TCon) -> (bool, isize) {
    if t.virt {
        (true, -(t.term as isize))
    } else {
        (false, t.term as isize)
    }
}

fn order_tcons(v: &mut [TCon]) {
    v.sort_by_key(tcon_order);
}

/// The state of a search being extended one index column at a time.
#[derive(Clone)]
struct Search {
    eq: Vec<(KeyExpr, Affinity)>,
    eq_multi: Vec<bool>,
    used: Vec<TermKey>,
    in_mul: LogEst,
    saved_out: LogEst,
    /// The lower bound already chosen for the current column, and whether
    /// it is the `> NULL` derived bound.
    lo: Option<(Expr, Affinity, bool, bool)>,
}

/// What a search step needs to know about its probe.
struct SearchCtx<'a, 'p> {
    p: &'a Probe<'p>,
    pi: usize,
    cons: &'a [&'a TCon],
    table: &'a Table,
    is_rowid_col: &'a dyn Fn(usize) -> bool,
    idx_cost: &'a dyn Fn(LogEst) -> LogEst,
    adjust: &'a dyn Fn(LogEst, &[TermKey], LogEst) -> LogEst,
    r_size: LogEst,
    indexed_loop: bool,
    sort_idx: usize,
}

/// What the wanted order is for: an ORDER BY (terms in order, directions
/// matter), a GROUP BY (any order, any direction) or the rows of a
/// SELECT DISTINCT (any order, one direction).
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Mode {
    Order,
    Group,
    Distinct,
}

/// The outcome of planning a single-table query.
pub(super) struct Planned {
    /// Leading terms of the wanted order the plan delivers.
    pub sat: usize,
    /// The DISTINCT needs no work of its own: a unique index makes it
    /// redundant, or the plan delivers rows grouped by the result.
    pub distinct_free: bool,
    /// In `Mode::Distinct` with all terms delivered: the rows also come
    /// in the keys' order (and direction).
    pub sorted: bool,
}

/// What an index column or ORDER BY term stands for.
#[derive(Clone, Copy)]
enum ColId<'e> {
    Rowid,
    Col(usize),
    Expr(&'e Expr),
}

struct OCol<'e> {
    id: ColId<'e>,
    desc: bool,
    coll: Collation,
    not_null: bool,
}

fn strip_collate(e: &Expr) -> &Expr {
    match e {
        Expr::Collate(inner, _) => strip_collate(inner),
        other => other,
    }
}

/// An integer literal in -1..=1 (SQLite assumes `x = 0` is less selective).
fn small_int(e: &Expr) -> bool {
    match e {
        Expr::Lit(Value::Int(i)) => (-1..=1).contains(i),
        Expr::Unary(ast::UnaryOp::Neg, a) => {
            matches!(**a, Expr::Lit(Value::Int(1)) | Expr::Lit(Value::Int(0)))
        }
        _ => false,
    }
}

impl Binder<'_> {
    /// Choose how to read the only table of a query, as SQLite would.
    /// `keys` / `exprs` are the ORDER BY (empty for none). Returns how many
    /// leading ORDER BY terms the chosen plan delivers, or None when the
    /// source is not a plain table.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn plan_single(
        &self,
        first: &mut Source,
        keys: &[OrderKey],
        exprs: &[Expr],
        mode: Mode,
        limit: Option<LogEst>,
        distinct: Option<&[Expr]>,
        n_results: usize,
    ) -> Option<Planned> {
        let cursor = first.cursor;
        let SourceKind::Table { table, .. } = &first.kind else {
            return None;
        };
        let table = table.clone();
        let rowid_slot = table.columns.len();
        let is_rowid_col =
            |col: usize| !table.without_rowid && (col == rowid_slot || Some(col) == table.ipk);
        let specials: &[SpecialIndex] = self.specials.get(&cursor).map_or(&[], |v| v.as_slice());
        let owned_filters = first.filters.clone();
        let filters: Vec<&Expr> = owned_filters.iter().collect();
        // SELECT DISTINCT: redundant when a unique index covers the
        // result; without an ORDER BY, SQLite plans for rows ordered by
        // the result, which lets it drop duplicates without a table.
        // Asked for DISTINCT-as-GROUP-BY (the keys are also the ORDER BY),
        // as opposed to planning a plain DISTINCT as an ordering below.
        let sort_by_group = mode == Mode::Distinct;
        let redundant =
            distinct.is_some_and(|rs| self.distinct_redundant(rs, &table, cursor, &filters));
        let rs_keys: Vec<OrderKey>;
        let (keys, exprs, mode) = match distinct {
            Some(rs) if !redundant && keys.is_empty() && mode == Mode::Order => {
                rs_keys = rs
                    .iter()
                    .map(|e| OrderKey {
                        src: KeySrc::Result(0),
                        desc: false,
                        nulls_first: true,
                        coll: collation(e).map_or(Collation::Binary, |c| c.0),
                    })
                    .collect();
                (&rs_keys[..], rs, Mode::Distinct)
            }
            _ => (keys, exprs, mode),
        };

        // What each WHERE term means for the estimates.
        let indexable = |x: &Expr| {
            matches!(col_ref(x), Some((c, _)) if *c == cursor)
                || specials
                    .iter()
                    .any(|s| s.keys.iter().flatten().any(|k| same_expr(k, x)))
        };
        let terms: Vec<TermInfo> = filters
            .iter()
            .map(|f| {
                let mut cols = Vec::new();
                cols_of(f, cursor, &mut cols);
                let (eq_k, virtuals) = match f {
                    Expr::Cmp { op, l, r, .. } => {
                        let eq = matches!(op, BinaryOp::Eq | BinaryOp::Is);
                        let (li, ri) = (indexable(l), indexable(r));
                        let k = if !eq || !(li || ri) {
                            None
                        } else {
                            let right = if li { r } else { l };
                            Some(if small_int(right) { 10 } else { 20 })
                        };
                        let v = if li && ri && matches!(
                            op,
                            BinaryOp::Eq
                                | BinaryOp::Is
                                | BinaryOp::Lt
                                | BinaryOp::Le
                                | BinaryOp::Gt
                                | BinaryOp::Ge
                        ) {
                            vec![eq]
                        } else {
                            Vec::new()
                        };
                        (k, v)
                    }
                    Expr::Between {
                        e, negated: false, ..
                    } if indexable(e) => (None, vec![false, false]),
                    Expr::Like { .. } if like_range(f, cursor, &table).is_some() => (None, vec![false, false]),
                    // The `col > NULL` that STAT4 builds derive.
                    Expr::IsNull(x, true)
                        if matches!(&**x, Expr::Col { cur, col, .. } if *cur == cursor && !is_rowid_col(*col)) =>
                    {
                        (None, vec![false])
                    }
                    _ => (None, Vec::new()),
                };
                TermInfo {
                    eq_k,
                    cols,
                    virtuals,
                }
            })
            .collect();
        // The constraints SQLite can drive a search with, in the order it
        // finds them: the terms as written, then the terms it derives
        // (BETWEEN halves, `x > NULL` from `x IS NOT NULL`, commuted
        // copies), the latter from the last WHERE term to the first.
        let mut tcons: Vec<TCon> = Vec::new();
        for (i, f) in filters.iter().enumerate() {
            let mut out = Vec::new();
            self.constraints(f, cursor, &mut out);
            let derived_all = matches!(f, Expr::Between { .. });
            for (n, c) in out.into_iter().enumerate() {
                tcons.push(TCon {
                    term: i,
                    virt: derived_all || n > 0,
                    vnull: false,
                    isnull: false,
                    likeopt: false,
                    c,
                });
            }
            if let Expr::IsNull(x, false) = f {
                if let Expr::Col { cur, col, coll, .. } = &**x {
                    if *cur == cursor && !is_rowid_col(*col) {
                        tcons.push(TCon {
                            term: i,
                            virt: false,
                            vnull: false,
                            isnull: true,
                            likeopt: false,
                            c: Constraint {
                                col: *col,
                                op: BinaryOp::Eq,
                                rhs: KeyExpr::Null,
                                aff: Affinity::Blob,
                                coll: *coll,
                            },
                        });
                    }
                }
            }
            if let Some((lo, hi, complete)) = like_range(f, cursor, &table) {
                // When the pattern is only the prefix and a final `%`, the
                // range is the whole term; otherwise it is extra.
                let term = if complete { i } else { usize::MAX - i };
                for (op, text) in [(BinaryOp::Ge, lo.0), (BinaryOp::Lt, hi)] {
                    tcons.push(TCon {
                        term,
                        virt: true,
                        vnull: false,
                        isnull: false,
                        likeopt: true,
                        c: Constraint {
                            col: lo.1,
                            op,
                            rhs: KeyExpr::One(Expr::Lit(Value::text(text))),
                            aff: lo.2,
                            coll: lo.3,
                        },
                    });
                }
            }
            // STAT4 builds (such as the reference sqlite3) also turn
            // `col IS NOT NULL` into a virtual `col > NULL`.
            if let Expr::IsNull(x, true) = f {
                if let Expr::Col { cur, col, coll, .. } = &**x {
                    if *cur == cursor && !is_rowid_col(*col) {
                        tcons.push(TCon {
                            term: i,
                            virt: true,
                            vnull: true,
                            isnull: false,
                            likeopt: false,
                            c: Constraint {
                                col: *col,
                                op: BinaryOp::Ge,
                                rhs: KeyExpr::One(Expr::Lit(Value::Real(f64::NEG_INFINITY))),
                                aff: Affinity::Blob,
                                coll: *coll,
                            },
                        });
                    }
                }
            }
        }
        order_tcons(&mut tcons);
        // Rows left after the terms a plan does not use (SQLite's
        // whereLoopOutputAdjust).
        let adjust = |n_out: LogEst, used: &[TermKey], r_size: LogEst| {
            let mut n = n_out;
            let mut reduce = 0;
            for (i, t) in terms.iter().enumerate() {
                if used.iter().any(|u| u.0 == i) {
                    continue;
                }
                n -= 1;
                if let Some(k) = t.eq_k {
                    reduce = reduce.max(k);
                }
            }
            n.min(r_size - reduce)
        };

        // The ways in, in SQLite's order: the table's own b-tree, then
        // its indexes newest first.
        let used = &self.used[cursor];
        let covers = |index: &Index| {
            used.iter()
                .enumerate()
                .all(|(col, u)| !u || is_rowid_col(col) || index.holds(col))
        };
        let sz_tab = crate::cost::table_row_size(&table);
        let probe_for = |ix: &Rc<Index>| -> Option<Probe> {
            let special = specials.iter().find(|s| Rc::ptr_eq(&s.index, ix));
            if ix.cols.is_empty() || !(ix.usable || special.is_some()) {
                return None;
            }
            let mut extra = Vec::new();
            if let Some(sp) = special {
                if !sp.terms.iter().all(|t| implied(t, &filters)) {
                    return None;
                }
                for (k, key) in sp.keys.iter().enumerate() {
                    let Some(key) = key else { continue };
                    for (i, f) in filters.iter().enumerate() {
                        if let Some(sw) = swap_key(f, key, cursor, EXPR_KEY + k) {
                            let mut out = Vec::new();
                            self.constraints(&sw, cursor, &mut out);
                            let derived_all = matches!(f, Expr::Between { .. });
                            extra.extend(out.into_iter().enumerate().map(|(n, c)| TCon {
                                term: i,
                                virt: derived_all || n > 0,
                                vnull: false,
                                isnull: false,
                                likeopt: false,
                                c,
                            }));
                        }
                    }
                }
            }
            let plain = special.is_none_or(|s| s.keys.iter().all(Option::is_none));
            let uniq_not_null = ix.primary
                || (ix.unique
                    && plain
                    && ix.cols.iter().all(|c| {
                        table.columns.get(c.col).is_some_and(|col| col.not_null)
                            || is_rowid_col(c.col)
                    }));
            Some(Probe {
                index: Some(ix.clone()),
                special,
                row_est: crate::cost::index_row_est(ix, ix.cols.len()),
                sz_idx: crate::cost::index_row_size(&table, ix),
                covering: plain && (ix.primary || covers(ix)),
                uniq_not_null,
                extra,
            })
        };
        let indexed = self
            .indexed
            .get(&cursor)
            .cloned()
            .unwrap_or(ast::Indexed::Any);
        let mut newest_first: Vec<&Rc<Index>> = table
            .indexes
            .iter()
            .filter(|i| i.sql.is_some())
            .rev()
            .collect();
        newest_first.extend(table.indexes.iter().filter(|i| i.sql.is_none()).rev());
        let mut probes: Vec<Probe> = Vec::new();
        match &indexed {
            ast::Indexed::By(name) => {
                if let Some(p) = table
                    .indexes
                    .iter()
                    .find(|i| i.name.eq_ignore_ascii_case(name))
                    .and_then(&probe_for)
                {
                    probes.push(p);
                }
            }
            _ => {
                if !table.without_rowid {
                    probes.push(Probe {
                        index: None,
                        special: None,
                        row_est: vec![TABLE_ROWS, 0],
                        sz_idx: sz_tab,
                        covering: true,
                        uniq_not_null: true,
                        extra: Vec::new(),
                    });
                }
                if indexed == ast::Indexed::Any {
                    probes.extend(newest_first.iter().filter_map(|i| probe_for(i)));
                }
                if let Some(pk) = &table.pk_index {
                    probes.extend(probe_for(pk));
                }
            }
        }
        if probes.is_empty() {
            return None;
        }

        // whereShortCut: `rowid = X`, or a unique index whose every key
        // column is fixed by `= X`, is taken at once, unweighed. It then
        // counts as delivering any order and a distinct result.
        if matches!(indexed, ast::Indexed::Any) {
            let ctx = SearchCtx {
                p: &probes[0],
                pi: 0,
                cons: &[],
                table: &table,
                is_rowid_col: &is_rowid_col,
                idx_cost: &|n| n,
                adjust: &|n, _, _| n,
                r_size: 0,
                indexed_loop: false,
                sort_idx: 0,
            };
            let eq_one = |t: &&TCon| {
                t.c.op == BinaryOp::Eq && !t.isnull && matches!(t.c.rhs, KeyExpr::One(_))
            };
            let mut found: Option<Access> = None;
            if !table.without_rowid {
                if let Some(t) = tcons.iter().filter(eq_one).find(|t| fits(&ctx, &t.c, 0)) {
                    found = Some(Access::Rowid(t.c.rhs.clone()));
                }
            }
            if found.is_none() {
                for p in probes.iter().filter(|p| p.index.is_some()) {
                    let ix = p.index.as_ref().unwrap();
                    if !ix.unique || ix.where_.is_some() || ix.cols.len() > 3 {
                        continue;
                    }
                    let pctx = SearchCtx { p, ..ctx };
                    let mut eq = Vec::new();
                    for k in 0..ix.cols.len() {
                        let hit = tcons
                            .iter()
                            .chain(p.extra.iter())
                            .filter(eq_one)
                            .find(|t| fits(&pctx, &t.c, k));
                        match hit {
                            Some(t) => eq.push((t.c.rhs.clone(), t.c.aff)),
                            None => break,
                        }
                    }
                    if eq.len() == ix.cols.len() {
                        found = Some(build_access(p, eq, None, None));
                        break;
                    }
                }
            }
            if let Some(access) = found {
                if let SourceKind::Table { access: a, .. } = &mut first.kind {
                    *a = access;
                }
                first.reverse = false;
                return Some(Planned {
                    sat: keys.len(),
                    distinct_free: distinct.is_some(),
                    sorted: false,
                });
            }
        }

        // Does an index (or the table) help with the ORDER BY at all?
        let order_ids: Vec<ColId> = exprs
            .iter()
            .map(|e| self.col_id(e, &table, cursor))
            .collect();
        let want_distinct = distinct.is_some();
        let might_order =
            |p: &Probe| {
                if keys.is_empty() {
                    return false;
                }
                let Some(ix) = &p.index else {
                    return order_ids.iter().any(|id| matches!(id, ColId::Rowid));
                };
                order_ids.iter().any(|id| match id {
                    ColId::Rowid => true,
                    ColId::Col(c) => ix.cols.iter().enumerate().any(|(k, ic)| {
                        ic.col == *c && p.special.is_none_or(|s| s.keys[k].is_none())
                    }),
                    ColId::Expr(e) => p
                        .special
                        .is_some_and(|s| s.keys.iter().flatten().any(|k| same_expr(k, e))),
                })
            };

        let mut list: Vec<Cand> = Vec::new();
        for (pi, p) in probes.iter().enumerate() {
            let r_size = p.row_est[0];
            let r_log = est_log(r_size);
            let b = might_order(p);
            let sort_idx = if b { pi + 1 } else { 0 };
            let indexed_loop = p.index.is_some();
            let idx_cost = |n_out: LogEst| {
                let rc = if p.is_rowid() {
                    n_out + 16
                } else {
                    n_out + 1 + (15 * p.sz_idx) / sz_tab
                };
                let mut r = log_est_add(r_log, rc);
                if !(p.covering || p.is_rowid()) {
                    r = log_est_add(r, n_out + 16);
                }
                r
            };
            // Full scans.
            let mut search_sort_idx = 0;
            match &p.index {
                None => {
                    search_sort_idx = sort_idx;
                    insert_loop(
                        &mut list,
                        Cand {
                            access: Access::Scan,
                            r_run: r_size + 16,
                            n_out: adjust(r_size, &[], r_size),
                            probe: pi,
                            eq_multi: Vec::new(),
                            one_row: false,
                            used: Vec::new(),
                            indexed: false,
                            idx_only: false,
                            sort_idx,
                        },
                    );
                }
                Some(ix) => {
                    let partial = ix.where_.is_some();
                    let worth = b
                        || table.without_rowid
                        || partial
                        || matches!(indexed, ast::Indexed::By(_))
                        || (p.covering && p.sz_idx < sz_tab);
                    if worth {
                        search_sort_idx = sort_idx;
                        let mut r_run = r_size + 1 + (15 * p.sz_idx) / sz_tab;
                        if !p.covering {
                            let mut n_lookup = r_size + 16;
                            let held = |cols: &[usize]| {
                                cols.iter().all(|c| is_rowid_col(*c) || ix.holds(*c))
                            };
                            let mut all = true;
                            for t in &terms {
                                if !held(&t.cols) {
                                    all = false;
                                    break;
                                }
                                n_lookup -= if t.eq_k.is_some() { 20 } else { 1 };
                            }
                            if all {
                                for t in &terms {
                                    for v in &t.virtuals {
                                        n_lookup -= if *v { 20 } else { 1 };
                                    }
                                }
                            }
                            r_run = log_est_add(r_run, n_lookup);
                        }
                        let access = if ix.primary {
                            Access::Scan
                        } else {
                            Access::Index(IndexAccess {
                                index: ix.clone(),
                                eq: Vec::new(),
                                lo: None,
                                hi: None,
                                covering: p.covering,
                            })
                        };
                        insert_loop(
                            &mut list,
                            Cand {
                                access,
                                r_run,
                                n_out: adjust(r_size, &[], r_size),
                                probe: pi,
                                eq_multi: Vec::new(),
                                one_row: false,
                                used: Vec::new(),
                                indexed: true,
                                idx_only: p.covering,
                                sort_idx,
                            },
                        );
                    }
                }
            }
            // Searches, one index column deeper at each step, as
            // whereLoopAddBtreeIndex recurses.
            let mut cons: Vec<&TCon> = tcons.iter().chain(p.extra.iter()).collect();
            if !p.extra.is_empty() {
                cons.sort_by_key(|t| tcon_order(t));
            }
            let ctx = SearchCtx {
                p,
                pi,
                cons: &cons,
                table: &table,
                is_rowid_col: &is_rowid_col,
                idx_cost: &idx_cost,
                adjust: &adjust,
                r_size,
                indexed_loop,
                sort_idx: search_sort_idx,
            };
            let start = Search {
                eq: Vec::new(),
                eq_multi: Vec::new(),
                used: Vec::new(),
                in_mul: 0,
                saved_out: r_size,
                lo: None,
            };
            search(&ctx, start, &mut list);
        }
        let mut cands = list;
        if cands.is_empty() {
            return None;
        }

        // SQLite's path solver for one table: the cheapest plan, first
        // without considering the ORDER BY (which fixes the row estimate
        // the sort is charged on), then with it.
        let base = |r_run: LogEst| log_est_add(log_est_add(0, r_run), 0);
        let pick = |costs: &[(LogEst, LogEst, LogEst)]| -> usize {
            let mut best = 0;
            for (i, c) in costs.iter().enumerate().skip(1) {
                let b = costs[best];
                let keep = b.0 < c.0 || (b.0 == c.0 && (b.1 < c.1 || (b.1 == c.1 && b.2 <= c.2)));
                if !keep {
                    best = i;
                }
            }
            best
        };
        let first_pass: Vec<(LogEst, LogEst, LogEst)> = cands
            .iter()
            .map(|c| (base(c.r_run), c.n_out, base(c.r_run) - 2))
            .collect();
        let mut chosen = pick(&first_pass);
        let mut sat = (keys.len(), false);
        if !keys.is_empty() {
            let n_row_est = cands[chosen].n_out + 1;
            let n = keys.len();
            let sats: Vec<(usize, bool)> = cands
                .iter()
                .map(|c| {
                    self.order_sat(
                        c,
                        &probes[c.probe],
                        &table,
                        cursor,
                        keys,
                        &order_ids,
                        &filters,
                        mode,
                    )
                })
                .collect();
            let second: Vec<(LogEst, LogEst, LogEst)> = cands
                .iter()
                .zip(&sats)
                .map(|(c, (s, _))| {
                    let unsorted = base(c.r_run);
                    if *s < n {
                        let sort = crate::cost::sorting_cost(
                            n_row_est,
                            n,
                            *s,
                            n_results,
                            limit,
                            want_distinct,
                        );
                        (log_est_add(unsorted, sort) + 3, c.n_out, unsorted)
                    } else {
                        (unsorted, c.n_out, unsorted - 2)
                    }
                })
                .collect();
            chosen = pick(&second);
            sat = sats[chosen];
        }
        // With an ORDER BY too, the DISTINCT is free when the plan also
        // delivers rows grouped by the result.
        let mut distinct_free = redundant;
        if let Some(rs) = distinct {
            if !redundant {
                distinct_free = if mode == Mode::Distinct {
                    sat.0 == keys.len()
                } else {
                    let rs_keys: Vec<OrderKey> = rs
                        .iter()
                        .map(|e| OrderKey {
                            src: KeySrc::Result(0),
                            desc: false,
                            nulls_first: true,
                            coll: collation(e).map_or(Collation::Binary, |c| c.0),
                        })
                        .collect();
                    let ids: Vec<ColId> =
                        rs.iter().map(|e| self.col_id(e, &table, cursor)).collect();
                    let c = &cands[chosen];
                    self.order_sat(
                        c,
                        &probes[c.probe],
                        &table,
                        cursor,
                        &rs_keys,
                        &ids,
                        &filters,
                        Mode::Distinct,
                    )
                    .0 == rs.len()
                };
            }
        }
        // Planning DISTINCT as a GROUP BY that also sorts: do the rows
        // come in the keys' order as well?
        let mut sorted_rev = None;
        if sort_by_group && sat.0 == keys.len() {
            let c = &cands[chosen];
            let (n, rev) = self.order_sat(
                c,
                &probes[c.probe],
                &table,
                cursor,
                keys,
                &order_ids,
                &filters,
                Mode::Order,
            );
            if n == keys.len() {
                sorted_rev = Some(rev);
            }
        }
        let c = cands.swap_remove(chosen);
        if let SourceKind::Table { access, .. } = &mut first.kind {
            *access = c.access;
        }
        // Only a real ORDER BY sets the scan direction (SQLite ignores the
        // direction found when planning for GROUP BY or DISTINCT).
        first.reverse = mode == Mode::Order && sat.0 > 0 && sat.1;
        if let Some(rev) = sorted_rev {
            first.reverse = rev;
        }
        Some(Planned {
            sat: sat.0,
            distinct_free,
            sorted: sorted_rev.is_some(),
        })
    }

    /// SQLite's isDistinctRedundant: the result includes the rowid, or
    /// every column of a unique index (each NOT NULL, or fixed by `= X`).
    fn distinct_redundant(
        &self,
        rs: &[Expr],
        table: &Table,
        cursor: usize,
        filters: &[&Expr],
    ) -> bool {
        let ids: Vec<ColId> = rs.iter().map(|e| self.col_id(e, table, cursor)).collect();
        if ids.iter().any(|i| matches!(i, ColId::Rowid)) {
            return true;
        }
        let fixed = |col: usize, coll: Collation| {
            filters.iter().any(|f| match f {
                Expr::Cmp { op: BinaryOp::Eq, l, r, coll: c, .. } => {
                    let side = |a: &Expr, b: &Expr| {
                        matches!(col_ref(strip_collate(a)), Some((cu, co)) if *cu == cursor && *co == col)
                            && {
                                let mut v = Vec::new();
                                refs(b, &mut v);
                                !v.contains(&cursor)
                            }
                    };
                    *c == coll && (side(l, r) || side(r, l))
                }
                _ => false,
            })
        };
        let in_result = |col: usize, coll: Collation| {
            rs.iter().zip(&ids).any(|(e, id)| {
                matches!(id, ColId::Col(c) if *c == col)
                    && collation(e).map_or(Collation::Binary, |c| c.0) == coll
            })
        };
        table
            .indexes
            .iter()
            .chain(table.pk_index.iter())
            .filter(|ix| ix.unique && ix.where_.is_none() && ix.exprs.iter().all(Option::is_none))
            .any(|ix| {
                ix.cols.iter().all(|ic| {
                    let not_null = table.columns.get(ic.col).is_some_and(|c| c.not_null)
                        || ix.primary
                        || (!table.without_rowid
                            && (ic.col == table.columns.len() || Some(ic.col) == table.ipk));
                    fixed(ic.col, ic.collation) || (in_result(ic.col, ic.collation) && not_null)
                })
            })
    }

    /// SQLite's isSimpleCount plan: count the entries of the narrowest
    /// index that is narrower than the table (or the table itself).
    pub(super) fn plan_simple_count(&self, first: &mut Source) -> bool {
        let SourceKind::Table { table, access } = &mut first.kind else {
            return false;
        };
        if matches!(self.indexed.get(&first.cursor), Some(ast::Indexed::No)) {
            *access = Access::Scan;
            return true;
        }
        let sz_tab = crate::cost::table_row_size(table);
        let mut best: Option<(&Rc<Index>, LogEst)> = table
            .pk_index
            .as_ref()
            .map(|pk| (pk, crate::cost::index_row_size(table, pk)));
        let mut newest_first: Vec<&Rc<Index>> = table
            .indexes
            .iter()
            .filter(|i| i.sql.is_some())
            .rev()
            .collect();
        newest_first.extend(table.indexes.iter().filter(|i| i.sql.is_none()).rev());
        for ix in newest_first {
            if ix.cols.is_empty() || ix.where_.is_some() {
                continue;
            }
            let sz = crate::cost::index_row_size(table, ix);
            if sz < sz_tab && best.is_none_or(|(_, b)| sz < b) {
                best = Some((ix, sz));
            }
        }
        *access = match best {
            Some((ix, _)) if !ix.primary => Access::Index(IndexAccess {
                index: ix.clone(),
                eq: Vec::new(),
                lo: None,
                hi: None,
                covering: true,
            }),
            _ => Access::Scan,
        };
        true
    }

    /// What an ORDER BY expression stands for, relative to the table.
    fn col_id<'e>(&self, e: &'e Expr, table: &Table, cursor: usize) -> ColId<'e> {
        let e = strip_collate(e);
        match col_ref(e) {
            Some((c, col)) if *c == cursor => {
                if !table.without_rowid && (*col == table.columns.len() || Some(*col) == table.ipk)
                {
                    ColId::Rowid
                } else {
                    ColId::Col(*col)
                }
            }
            _ => ColId::Expr(e),
        }
    }

    /// SQLite's wherePathSatisfiesOrderBy for a single loop: how many
    /// leading ORDER BY terms the plan delivers, and whether it must walk
    /// its index backwards to do so.
    #[allow(clippy::too_many_arguments)]
    fn order_sat(
        &self,
        cand: &Cand,
        probe: &Probe,
        table: &Table,
        cursor: usize,
        keys: &[OrderKey],
        ids: &[ColId],
        filters: &[&Expr],
        mode: Mode,
    ) -> (usize, bool) {
        let n = keys.len();
        let mut sat = vec![false; n];
        let same_id = |a: &ColId, b: &ColId| match (a, b) {
            (ColId::Rowid, ColId::Rowid) => true,
            (ColId::Col(x), ColId::Col(y)) => x == y,
            (ColId::Expr(x), ColId::Expr(y)) => same_expr(x, y),
            _ => false,
        };
        // Terms fixed by `col = constant` (or IS / IS NULL).
        for (i, id) in ids.iter().enumerate() {
            if matches!(id, ColId::Expr(_)) {
                continue;
            }
            let other_free = |x: &Expr| {
                let mut r = Vec::new();
                refs(x, &mut r);
                !r.contains(&cursor)
            };
            let fixed = filters.iter().any(|f| match f {
                Expr::Cmp {
                    op: BinaryOp::Eq | BinaryOp::Is,
                    l,
                    r,
                    coll,
                    ..
                } => {
                    let side = |a: &Expr, b: &Expr| {
                        same_id(&self.col_id(a, table, cursor), id)
                            && matches!(col_ref(strip_collate(a)), Some((c, _)) if *c == cursor)
                            && other_free(b)
                    };
                    (side(l, r) || side(r, l))
                        && (matches!(id, ColId::Rowid) || *coll == keys[i].coll)
                }
                Expr::IsNull(x, false) => {
                    matches!(col_ref(strip_collate(x)), Some((c, _)) if *c == cursor)
                        && same_id(&self.col_id(x, table, cursor), id)
                }
                _ => false,
            });
            if fixed {
                sat[i] = true;
            }
        }
        if cand.one_row {
            return (n, false);
        }
        // The columns the plan delivers in order.
        let mut cols: Vec<OCol> = Vec::new();
        let n_key;
        match &probe.index {
            None => {
                n_key = 0;
                cols.push(OCol {
                    id: ColId::Rowid,
                    desc: false,
                    coll: Collation::Binary,
                    not_null: true,
                });
            }
            Some(ix) => {
                n_key = ix.cols.len();
                for (k, ic) in ix.cols.iter().enumerate() {
                    let key = probe
                        .special
                        .and_then(|s| s.keys.get(k))
                        .and_then(Option::as_ref);
                    let id = match key {
                        Some(e) => ColId::Expr(e),
                        None if !table.without_rowid
                            && (ic.col == table.columns.len() || Some(ic.col) == table.ipk) =>
                        {
                            ColId::Rowid
                        }
                        None => ColId::Col(ic.col),
                    };
                    let not_null = match id {
                        ColId::Col(c) => {
                            table.columns[c].not_null
                                || (table.without_rowid && table.pk.iter().any(|p| p.col == c))
                        }
                        _ => true,
                    };
                    cols.push(OCol {
                        id,
                        desc: ic.desc,
                        coll: ic.collation,
                        not_null,
                    });
                }
                if ix.primary {
                    // The primary key of a WITHOUT ROWID table holds every
                    // column, the rest after the key.
                    for c in table.storage_order.iter().skip(ix.cols.len()) {
                        let col = &table.columns[*c];
                        cols.push(OCol {
                            id: ColId::Col(*c),
                            desc: false,
                            coll: col.collation,
                            not_null: col.not_null,
                        });
                    }
                } else {
                    if table.without_rowid {
                        for c in &ix.extra {
                            let p = table.pk.iter().find(|p| p.col == *c);
                            cols.push(OCol {
                                id: ColId::Col(*c),
                                desc: p.is_some_and(|p| p.desc),
                                coll: p.map_or(Collation::Binary, |p| p.collation),
                                not_null: true,
                            });
                        }
                    } else {
                        cols.push(OCol {
                            id: ColId::Rowid,
                            desc: false,
                            coll: Collation::Binary,
                            not_null: true,
                        });
                    }
                }
            }
        }
        let n_eq = cand.eq_multi.len();
        let mut distinct = probe.is_rowid() || probe.index.as_ref().is_some_and(|i| i.unique);
        let mut distinct_columns = false;
        let mut rev: Option<bool> = None;
        let eq_null = |j: usize| match &cand.access {
            Access::Index(ix) => ix
                .eq
                .get(j)
                .is_some_and(|(k, _)| matches!(k, KeyExpr::Null)),
            _ => false,
        };
        for (j, oc) in cols.iter().enumerate() {
            if j < n_eq && !cand.eq_multi[j] {
                if eq_null(j) {
                    distinct = false;
                }
                continue;
            }
            if distinct {
                if matches!(oc.id, ColId::Col(_)) && j >= n_eq && !oc.not_null {
                    distinct = false;
                }
                if matches!(oc.id, ColId::Expr(_)) {
                    distinct = false;
                }
            }
            let mut matched = false;
            // An ORDER BY must take its terms in order; GROUP BY and
            // DISTINCT may match any term not yet delivered.
            let pending: Vec<usize> = (0..n).filter(|i| !sat[*i]).collect();
            let scan: &[usize] = if mode == Mode::Order {
                &pending[..pending.len().min(1)]
            } else {
                &pending
            };
            let hit = scan.iter().copied().find(|&i| {
                let k = &keys[i];
                let id_ok = match (&oc.id, &ids[i]) {
                    (ColId::Expr(x), ColId::Expr(y)) => same_expr(x, y),
                    (ColId::Expr(_), _) => false,
                    (a, b) => same_id(a, b),
                };
                id_ok && (matches!(oc.id, ColId::Rowid) || k.coll == oc.coll)
            });
            if let Some(i) = hit {
                let k = &keys[i];
                // Non-default NULLS placement cannot come from an index.
                let nulls_ok = k.nulls_first != k.desc;
                let r = oc.desc != k.desc;
                let dir_ok = mode == Mode::Group || rev.is_none_or(|rv| rv == r);
                if nulls_ok && dir_ok {
                    if mode != Mode::Group {
                        rev = Some(r);
                    }
                    matched = true;
                    if matches!(oc.id, ColId::Rowid) {
                        distinct_columns = true;
                    }
                    sat[i] = true;
                }
            }
            if !matched {
                if j == 0 || j < n_key {
                    distinct = false;
                }
                break;
            }
        }
        if distinct_columns {
            distinct = true;
        }
        if distinct {
            sat.iter_mut().for_each(|s| *s = true);
        }
        let rev = rev.unwrap_or(false);
        if sat.iter().all(|s| *s) {
            return (n, rev);
        }
        (sat.iter().take_while(|s| **s).count(), rev)
    }
}

/// Is index column `k` of the probe declared NOT NULL?
fn column_not_null(ctx: &SearchCtx, k: usize) -> bool {
    let Some(ix) = &ctx.p.index else { return true };
    if k >= ix.cols.len() {
        return true;
    }
    if ctx
        .p
        .special
        .and_then(|s| s.keys.get(k))
        .is_some_and(Option::is_some)
    {
        return false;
    }
    let c = ix.cols[k].col;
    ctx.table.columns.get(c).is_some_and(|col| col.not_null)
        || (ctx.table.without_rowid && ctx.table.pk.iter().any(|p| p.col == c))
}

/// Can constraint `c` drive index column `k` of the probe?
fn fits(ctx: &SearchCtx, c: &Constraint, k: usize) -> bool {
    let p = ctx.p;
    match &p.index {
        None => (ctx.is_rowid_col)(c.col) && c.aff != Affinity::Text,
        // The rowid after an index's key.
        Some(ix) if k >= ix.cols.len() && !ctx.table.without_rowid => {
            (ctx.is_rowid_col)(c.col) && c.aff != Affinity::Text
        }
        // The rest of a WITHOUT ROWID primary key.
        Some(ix) if k >= ix.cols.len() => {
            let j = k - ix.cols.len();
            let (Some(col), Some((coll, _))) = (ix.extra.get(j), ix.extra_order.get(j)) else {
                return false;
            };
            let col_aff = ctx.table.columns[*col].affinity;
            let aff_ok = match c.aff {
                Affinity::Blob => true,
                Affinity::Text => col_aff == Affinity::Text,
                _ => col_aff.is_numeric(),
            };
            c.col == *col && c.coll == *coll && aff_ok
        }
        Some(ix) => {
            let ic = &ix.cols[k];
            let key = p
                .special
                .and_then(|s| s.keys.get(k))
                .and_then(Option::as_ref);
            let (col, col_aff) = match key {
                Some(e) => (EXPR_KEY + k, affinity(e)),
                None => (ic.col, ctx.table.columns[ic.col].affinity),
            };
            let aff_ok = match c.aff {
                Affinity::Blob => true,
                Affinity::Text => col_aff == Affinity::Text,
                _ => col_aff.is_numeric(),
            };
            let same_col = c.col == col
                || (key.is_none() && (ctx.is_rowid_col)(c.col) && (ctx.is_rowid_col)(col));
            same_col && c.coll == ic.collation && aff_ok
        }
    }
}

/// The access path for a search on the probe.
fn build_access(
    p: &Probe,
    eq: Vec<(KeyExpr, Affinity)>,
    lo: Option<(Expr, Affinity, bool)>,
    hi: Option<(Expr, Affinity, bool)>,
) -> Access {
    match &p.index {
        None => {
            if let Some((k, _)) = eq.into_iter().next() {
                Access::Rowid(k)
            } else {
                Access::RowidRange {
                    lo: lo.map(|(e, _, i)| (e, i)),
                    hi: hi.map(|(e, _, i)| (e, i)),
                }
            }
        }
        Some(ix) => Access::Index(IndexAccess {
            index: ix.clone(),
            eq,
            lo,
            hi,
            covering: ix.primary || p.covering,
        }),
    }
}

/// whereLoopAddBtreeIndex: extend a search by one constraint on the next
/// index column (or a bound on the current one), record it, and recurse.
fn search(ctx: &SearchCtx, s: Search, list: &mut Vec<Cand>) {
    let p = ctx.p;
    let k = s.eq.len();
    if k >= p.ncol(!ctx.table.without_rowid) {
        return;
    }
    for t in ctx.cons.iter() {
        if !fits(ctx, &t.c, k) {
            continue;
        }
        // IS [NOT] NULL tells nothing about a NOT NULL column.
        if (t.vnull || t.isnull) && column_not_null(ctx, k) {
            continue;
        }
        // The LIKE range's upper bound only comes with its lower one.
        if t.likeopt && t.c.op == BinaryOp::Lt {
            continue;
        }
        let c = &t.c;
        let mut used = s.used.clone();
        used.push(t.key());
        let cand = |access: Access, r_run, n_out, eq_multi: Vec<bool>, one_row| Cand {
            access,
            r_run,
            n_out,
            probe: ctx.pi,
            eq_multi,
            one_row,
            used: used.clone(),
            indexed: ctx.indexed_loop,
            idx_only: ctx.indexed_loop && p.covering,
            sort_idx: ctx.sort_idx,
        };
        match c.op {
            BinaryOp::Eq if s.lo.is_none() => {
                let n_in = match &c.rhs {
                    KeyExpr::One(_) => 0,
                    KeyExpr::List(l) => log_est(l.len() as u64),
                    KeyExpr::Sub(_) => 46,
                    KeyExpr::Null => 0,
                };
                let est = |i: usize| p.row_est.get(i).copied().unwrap_or(0);
                let mut n_out0 = s.saved_out + (est(k + 1) - est(k));
                if t.isnull {
                    // SQLite assumes IS NULL matches twice as many rows.
                    n_out0 += 10;
                }
                let one = matches!(c.rhs, KeyExpr::One(_))
                    && (p.is_rowid()
                        || (k >= p.nkey() && !ctx.table.without_rowid)
                        || (s.in_mul == 0
                            && k + 1 == p.nkey()
                            && (p.uniq_not_null
                                || (p.nkey() == 1 && p.index.as_ref().is_some_and(|i| i.unique)))));
                let r_run = (ctx.idx_cost)(n_out0) + s.in_mul + n_in;
                let n_out = (ctx.adjust)(n_out0 + s.in_mul + n_in, &used, ctx.r_size);
                let mut eq = s.eq.clone();
                eq.push((c.rhs.clone(), c.aff));
                let mut eq_multi = s.eq_multi.clone();
                eq_multi.push(!c.rhs.is_one());
                insert_loop(
                    list,
                    cand(
                        build_access(p, eq.clone(), None, None),
                        r_run,
                        n_out,
                        eq_multi.clone(),
                        one,
                    ),
                );
                if !p.is_rowid() && k + 1 < p.ncol(!ctx.table.without_rowid) {
                    search(
                        ctx,
                        Search {
                            eq,
                            eq_multi,
                            used: used.clone(),
                            in_mul: s.in_mul + n_in,
                            saved_out: n_out0,
                            lo: None,
                        },
                        list,
                    );
                }
            }
            BinaryOp::Ge if s.lo.is_none() && t.likeopt => {
                let Some(rhs) = c.one() else { continue };
                let Some(up) = ctx.cons.iter().find(|u| {
                    u.likeopt && u.term == t.term && u.c.op == BinaryOp::Lt && u.c.col == c.col
                }) else {
                    continue;
                };
                let Some(urhs) = up.c.one() else { continue };
                used.push(up.key());
                let n_out0 = range_est(s.saved_out, Some(false), true);
                let lo = (rhs.clone(), c.aff, true);
                let hi = (urhs.clone(), up.c.aff, false);
                let cand = |access: Access, r_run, n_out| Cand {
                    access,
                    r_run,
                    n_out,
                    probe: ctx.pi,
                    eq_multi: s.eq_multi.clone(),
                    one_row: false,
                    used: used.clone(),
                    indexed: ctx.indexed_loop,
                    idx_only: ctx.indexed_loop && p.covering,
                    sort_idx: ctx.sort_idx,
                };
                insert_loop(
                    list,
                    cand(
                        build_access(p, s.eq.clone(), Some(lo), Some(hi)),
                        (ctx.idx_cost)(n_out0) + s.in_mul,
                        (ctx.adjust)(n_out0 + s.in_mul, &used, ctx.r_size),
                    ),
                );
            }
            BinaryOp::Gt | BinaryOp::Ge if s.lo.is_none() => {
                let Some(rhs) = c.one() else { continue };
                let n_out0 = range_est(s.saved_out, Some(t.vnull), false);
                let lo = (rhs.clone(), c.aff, c.op == BinaryOp::Ge);
                insert_loop(
                    list,
                    cand(
                        build_access(p, s.eq.clone(), Some(lo.clone()), None),
                        (ctx.idx_cost)(n_out0) + s.in_mul,
                        (ctx.adjust)(n_out0 + s.in_mul, &used, ctx.r_size),
                        s.eq_multi.clone(),
                        false,
                    ),
                );
                search(
                    ctx,
                    Search {
                        lo: Some((lo.0, lo.1, lo.2, t.vnull)),
                        used: used.clone(),
                        ..s.clone()
                    },
                    list,
                );
            }
            BinaryOp::Lt | BinaryOp::Le => {
                let Some(rhs) = c.one() else { continue };
                let hi = (rhs.clone(), c.aff, c.op == BinaryOp::Le);
                let lo_vnull = s.lo.as_ref().map(|l| l.3);
                let lo = s.lo.clone().map(|(e, a, i, _)| (e, a, i));
                let n_out0 = range_est(s.saved_out, lo_vnull, true);
                insert_loop(
                    list,
                    cand(
                        build_access(p, s.eq.clone(), lo, Some(hi)),
                        (ctx.idx_cost)(n_out0) + s.in_mul,
                        (ctx.adjust)(n_out0 + s.in_mul, &used, ctx.r_size),
                        s.eq_multi.clone(),
                        false,
                    ),
                );
            }
            _ => {}
        }
    }
}

/// whereLoopCheaperProperSubset: `x` uses fewer terms than `y`, all of
/// them also used by `y`, and is not plainly worse.
fn cheaper_subset(x: &Cand, y: &Cand) -> bool {
    if x.used.len() >= y.used.len() {
        return false;
    }
    if x.r_run > y.r_run && x.n_out > y.n_out {
        return false;
    }
    if !x.used.iter().all(|t| y.used.contains(t)) {
        return false;
    }
    !(x.idx_only && !y.idx_only)
}

/// whereLoopInsert: adjust the new plan's cost against the plans already
/// listed (whereLoopAdjustCost), then keep it unless a listed plan of the
/// same sort slot is at least as good, replacing those it beats.
fn insert_loop(list: &mut Vec<Cand>, mut t: Cand) {
    if t.indexed {
        for p in list.iter().filter(|p| p.indexed) {
            if cheaper_subset(p, &t) {
                t.r_run = t.r_run.min(p.r_run);
                t.n_out = t.n_out.min(p.n_out - 1);
            } else if cheaper_subset(&t, p) {
                t.r_run = t.r_run.max(p.r_run);
                t.n_out = t.n_out.max(p.n_out + 1);
            }
        }
    }
    let better_or_equal = |p: &Cand, t: &Cand| p.r_run <= t.r_run && p.n_out <= t.n_out;
    let beaten = |p: &Cand, t: &Cand| p.r_run >= t.r_run && p.n_out >= t.n_out;
    let mut at = None;
    for (i, p) in list.iter().enumerate() {
        if p.sort_idx != t.sort_idx {
            continue;
        }
        if better_or_equal(p, &t) {
            return;
        }
        if beaten(p, &t) {
            at = Some(i);
            break;
        }
    }
    let Some(i) = at else {
        list.push(t);
        return;
    };
    // Drop later plans of the same slot that the new one also beats,
    // until one is found that is at least as good.
    let mut j = i + 1;
    while j < list.len() {
        let p = &list[j];
        if p.sort_idx != t.sort_idx {
            j += 1;
            continue;
        }
        if better_or_equal(p, &t) {
            break;
        }
        if beaten(p, &t) {
            list.remove(j);
            continue;
        }
        j += 1;
    }
    list[i] = t;
}

/// The lower bound (with the column, affinity and collation of both
/// bounds), the upper bound, and whether the range is the whole term.
type LikeRange = ((String, usize, Affinity, Collation), String, bool);

/// SQLite's LIKE optimization (isLikeOrGlob): for `col LIKE 'abc%'` (or
/// GLOB), the range `col >= 'ABC' AND col < 'abd'` under NOCASE (BINARY
/// for GLOB). Returns ((lower, column, affinity, collation), upper, and
/// whether the pattern is exactly the prefix and one trailing wildcard).
fn like_range(f: &Expr, cursor: usize, table: &Table) -> Option<LikeRange> {
    let Expr::Like {
        op,
        e,
        pat,
        esc,
        negated: false,
    } = f
    else {
        return None;
    };
    let esc_byte = match esc.as_deref() {
        None => None,
        Some(Expr::Lit(Value::Text(t))) if t.len() == 1 => Some(t.as_bytes()[0]),
        Some(_) => return None,
    };
    let (wild, no_case): (&[u8], bool) = match op {
        LikeOp::Like => (b"%_", true),
        LikeOp::Glob => (b"*?[", false),
        _ => return None,
    };
    let Expr::Col { cur, col, aff, .. } = &**e else {
        return None;
    };
    if *cur != cursor {
        return None;
    }
    let Expr::Lit(Value::Text(p)) = &**pat else {
        return None;
    };
    let z = p.as_bytes();
    // The prefix before the first wildcard, escapes removed.
    let mut cnt = 0;
    let mut raw = Vec::new();
    while cnt < z.len() && !wild.contains(&z[cnt]) {
        let c = z[cnt];
        cnt += 1;
        if Some(c) == esc_byte && cnt < z.len() {
            raw.push(z[cnt]);
            cnt += 1;
        } else {
            raw.push(c);
        }
    }
    if cnt == 0 || z[cnt - 1] == 0xFF || (cnt == 1 && Some(z[0]) == esc_byte) {
        return None;
    }
    let complete = cnt + 1 == z.len() && z[cnt] == wild[0];
    let prefix = std::str::from_utf8(&raw).ok()?;
    if prefix.is_empty() {
        return None;
    }
    // A prefix that reads as a number would compare as one with a
    // column of numeric affinity.
    let is_rowid = !table.without_rowid && (*col == table.columns.len() || Some(*col) == table.ipk);
    let col_aff = if is_rowid { Affinity::Integer } else { *aff };
    if col_aff != Affinity::Text {
        let looks_numeric = |t: &str| {
            let t = t.trim();
            !t.is_empty()
                && t.bytes()
                    .all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b))
                && t.parse::<f64>().is_ok()
        };
        let mut bumped = prefix.as_bytes().to_vec();
        *bumped.last_mut().unwrap() += 1;
        let bumped = String::from_utf8(bumped).ok()?;
        if looks_numeric(prefix) || prefix == "-" || looks_numeric(&bumped) {
            return None;
        }
    }
    let (lo, mut hi) = if no_case {
        (
            prefix.to_ascii_uppercase(),
            prefix.to_ascii_lowercase().into_bytes(),
        )
    } else {
        (prefix.to_string(), prefix.as_bytes().to_vec())
    };
    let mut complete = complete;
    let last = hi.last_mut().unwrap();
    let mut c = *last;
    if no_case {
        if c == b'A' - 1 {
            complete = false;
        }
        c = c.to_ascii_lowercase();
    }
    *last = c + 1;
    let hi = String::from_utf8(hi).ok()?;
    let coll = if no_case {
        Collation::NoCase
    } else {
        Collation::Binary
    };
    let cmp = cmp_affinity(col_aff, Affinity::Blob);
    Some(((lo, *col, cmp, coll), hi, complete))
}

/// SQLite's whereRangeScanEst without statistics: each bound keeps a
/// quarter of the rows, and a closed range a further quarter.
/// `lo` is None without a lower bound, else whether it is the derived
/// `> NULL` bound (which narrows nothing).
fn range_est(saved: LogEst, lo: Option<bool>, hi: bool) -> LogEst {
    let mut n_new = saved;
    if lo == Some(false) {
        n_new -= 20;
    }
    if hi {
        n_new -= 20;
    }
    if lo.is_some() && hi {
        n_new -= 20;
    }
    let n_out = saved - lo.is_some() as LogEst - hi as LogEst;
    n_new.max(10).min(n_out)
}
