//! Name resolution and query planning: AST in, [`Plan`] out.
//!
//! Binding resolves every identifier against the tables in scope and
//! rewrites the AST into the executor's expression form. Planning then
//! decides, for each `SELECT`, in which order the tables are joined and how
//! each one is reached: by rowid, through an index, through a transient
//! hash table, or by scanning.

use crate::error::{Error, Result};
use crate::func::{lookup_agg, lookup_scalar};
use crate::pager::Pager;
use crate::plan::*;
use crate::schema::{Schema, Table};
use crate::sql::ast::{
    self, BinaryOp, FromItem, JoinKind, LikeOp, ResultColumn, SelectBody, TableSource,
};
use crate::value::{Affinity, Collation, Value};
use std::collections::HashMap;
use std::rc::Rc;

struct ScopeCol {
    name: String,
    aff: Affinity,
    coll: Collation,
    /// Hidden from `*` and from unqualified lookups: the right-hand copy of
    /// a column merged by USING or NATURAL.
    hidden: bool,
}

struct ScopeSource {
    cursor: usize,
    /// Lower-cased alias or table name; empty for an unnamed subquery.
    name: String,
    cols: Vec<ScopeCol>,
    /// Column index that yields the rowid, for tables that have one.
    rowid: Option<usize>,
}

#[derive(Default)]
struct Scope {
    sources: Vec<ScopeSource>,
    /// Result-column aliases, usable in WHERE / GROUP BY / HAVING.
    aliases: Vec<(String, ast::Expr)>,
}

struct CteDef {
    name: String,
    columns: Vec<String>,
    select: Rc<ast::Select>,
    /// How many CTEs and scopes were visible where this one was defined.
    ctes_len: usize,
    scopes_len: usize,
    recursive: bool,
}

/// A recursive CTE whose step is being bound: inside it, the CTE's name
/// means the current row (the work table).
struct WorkDef {
    name: String,
    id: usize,
    cols: Vec<ColInfo>,
    depth: usize,
}

struct AggFrame {
    slot: usize,
    calls: Vec<AggCall>,
    allowed: bool,
}

enum ItemKind {
    Table(Rc<Table>),
    Sub { query: Box<Query>, correlated: bool },
}

struct Item {
    cursor: usize,
    name: String,
    kind: ItemKind,
    left_join: bool,
    ncols: usize,
}

struct Conj {
    expr: Expr,
    /// For a LEFT JOIN's ON term: the item it belongs to.
    left_on: Option<usize>,
    /// Items of this query that the term reads.
    items: Vec<usize>,
}

/// A WHERE term of the shape `column op value` usable to drive an access.
struct Constraint {
    col: usize,
    op: BinaryOp,
    rhs: Expr,
    aff: Affinity,
    coll: Collation,
}

pub struct Binder<'a> {
    schema: &'a Schema,
    pager: &'a Pager,
    next_sub: usize,
    next_slot: usize,
    /// Log of every cursor referenced, in binding order.
    refs: Vec<usize>,
    used: Vec<Vec<bool>>,
    scopes: Vec<Scope>,
    ctes: Vec<CteDef>,
    aggs: Vec<AggFrame>,
    view_depth: usize,
    /// Nesting depth of SELECTs being bound.
    query_depth: usize,
    work_tables: Vec<WorkDef>,
    next_work: usize,
}

fn lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

fn split_and<'e>(e: &'e ast::Expr, out: &mut Vec<&'e ast::Expr>) {
    match e {
        ast::Expr::Binary(BinaryOp::And, a, b) => {
            split_and(a, out);
            split_and(b, out);
        }
        other => out.push(other),
    }
}

fn is_rowid_name(name: &str) -> bool {
    ["rowid", "oid", "_rowid_"]
        .iter()
        .any(|n| name.eq_ignore_ascii_case(n))
}

/// Bind a SELECT statement into an executable plan.
pub fn bind_select(schema: &Schema, pager: &Pager, select: &ast::Select) -> Result<Plan> {
    let mut b = Binder::new(schema, pager);
    let query = b.bind_query(select)?;
    Ok(b.finish(query))
}

impl<'a> Binder<'a> {
    pub fn new(schema: &'a Schema, pager: &'a Pager) -> Binder<'a> {
        Binder {
            schema,
            pager,
            next_sub: 0,
            next_slot: 0,
            refs: Vec::new(),
            used: Vec::new(),
            scopes: Vec::new(),
            ctes: Vec::new(),
            aggs: Vec::new(),
            view_depth: 0,
            query_depth: 0,
            work_tables: Vec::new(),
            next_work: 0,
        }
    }

    pub fn finish(self, query: Query) -> Plan {
        Plan {
            query,
            used: self.used,
            n_agg_slots: self.next_slot,
            n_subqueries: self.next_sub,
        }
    }

    fn new_cursor(&mut self, width: usize) -> usize {
        self.used.push(vec![false; width]);
        self.used.len() - 1
    }

    // ------------------------------------------------------------ queries

    pub fn bind_query(&mut self, sel: &ast::Select) -> Result<Query> {
        let ctes_mark = self.ctes.len();
        for cte in &sel.with {
            self.ctes.push(CteDef {
                name: lower(&cte.name),
                columns: cte.columns.clone(),
                select: Rc::new(cte.select.clone()),
                ctes_len: self.ctes.len(),
                scopes_len: self.scopes.len(),
                recursive: cte.recursive && select_mentions(&cte.select, &cte.name),
            });
        }
        self.query_depth += 1;
        let result = self.bind_query_inner(sel);
        self.query_depth -= 1;
        self.ctes.truncate(ctes_mark);
        result
    }

    /// Bind the body of a recursive CTE: `init UNION [ALL] step`, where only
    /// `step` names the CTE itself.
    fn bind_recursive(
        &mut self,
        sel: &ast::Select,
        name: &str,
        rename: &[String],
    ) -> Result<Query> {
        let unsupported = |what: &str| Err(Error::Unsupported(what.into()));
        if !sel.with.is_empty() {
            return unsupported("a WITH clause inside a recursive common table expression");
        }
        if !sel.order_by.is_empty() {
            return unsupported("ORDER BY in a recursive common table expression");
        }
        let SelectBody::Compound { op, left, right } = &sel.body else {
            return Err(Error::Sql(format!("circular reference: {name}")));
        };
        let union_all = match op {
            ast::CompoundOp::UnionAll => true,
            ast::CompoundOp::Union => false,
            _ => return unsupported("a recursive common table expression that is not a UNION"),
        };
        let probe = |b: &SelectBody| {
            select_mentions(
                &ast::Select {
                    with: Vec::new(),
                    body: b.clone(),
                    order_by: Vec::new(),
                    limit: None,
                    offset: None,
                },
                name,
            )
        };
        if probe(left) {
            return unsupported("a recursive common table expression with several recursive steps");
        }
        self.query_depth += 1;
        let result = (|| {
            let init = self.bind_body(left)?;
            if !rename.is_empty() && rename.len() != init.cols.len() {
                return Err(Error::Sql(format!(
                    "table {name} has {} values for {} columns",
                    init.cols.len(),
                    rename.len()
                )));
            }
            let mut work_cols = init.cols.clone();
            for (c, n) in work_cols.iter_mut().zip(rename) {
                c.name = n.clone();
            }
            let id = self.next_work;
            self.next_work += 1;
            self.work_tables.push(WorkDef {
                name: lower(name),
                id,
                cols: work_cols,
                depth: self.query_depth,
            });
            let step = self.bind_body(right);
            self.work_tables.pop();
            let step = step?;
            if step.cols.len() != init.cols.len() {
                return Err(Error::Sql(format!(
                    "SELECTs to the left and right of {} do not have the same number of result columns",
                    if union_all { "UNION ALL" } else { "UNION" }
                )));
            }
            let cols = init.cols.clone();
            Ok(Query {
                body: Body::Recursive {
                    init: Box::new(init),
                    step: Box::new(step),
                    union_all,
                    work: id,
                },
                order_by: Vec::new(),
                limit: self.bind_opt_const(&sel.limit)?,
                offset: self.bind_opt_const(&sel.offset)?,
                cols,
            })
        })();
        self.query_depth -= 1;
        result
    }

    fn bind_query_inner(&mut self, sel: &ast::Select) -> Result<Query> {
        if let SelectBody::Core(core) = &sel.body {
            return self.bind_core(core, &sel.order_by, &sel.limit, &sel.offset);
        }
        let mut q = self.bind_body(&sel.body)?;
        let asts = leftmost_columns(&sel.body);
        for t in &sel.order_by {
            let (inner, explicit) = match &t.expr {
                ast::Expr::Collate { expr, collation } => {
                    (expr.as_ref(), Some(parse_collation(collation)?))
                }
                e => (e, None),
            };
            let n = q.cols.len();
            let idx = match inner {
                ast::Expr::Literal(Value::Int(k)) => {
                    if *k < 1 || *k as usize > n {
                        return Err(Error::Sql(format!(
                            "ORDER BY term out of range - should be between 1 and {n}"
                        )));
                    }
                    *k as usize - 1
                }
                ast::Expr::Name {
                    table: None, name, ..
                } if q.cols.iter().any(|c| c.name.eq_ignore_ascii_case(name)) => q
                    .cols
                    .iter()
                    .position(|c| c.name.eq_ignore_ascii_case(name))
                    .unwrap(),
                other => match asts.iter().position(|a| a.as_ref() == Some(other)) {
                    Some(i) => i,
                    None => {
                        return Err(Error::Sql(
                            "ORDER BY term does not match any column in the result set".into(),
                        ))
                    }
                },
            };
            q.order_by.push(OrderKey {
                src: KeySrc::Result(idx),
                desc: t.desc,
                nulls_first: t.nulls_first.unwrap_or(!t.desc),
                coll: explicit.unwrap_or(q.cols[idx].coll),
            });
        }
        q.limit = self.bind_opt_const(&sel.limit)?;
        q.offset = self.bind_opt_const(&sel.offset)?;
        Ok(q)
    }

    fn bind_opt_const(&mut self, e: &Option<ast::Expr>) -> Result<Option<Expr>> {
        match e {
            Some(e) => Ok(Some(self.bind_expr(e)?)),
            None => Ok(None),
        }
    }

    fn bind_body(&mut self, body: &SelectBody) -> Result<Query> {
        match body {
            SelectBody::Core(core) => self.bind_core(core, &[], &None, &None),
            SelectBody::Values(rows) => {
                let n = rows[0].len();
                let mut out = Vec::new();
                for row in rows {
                    if row.len() != n {
                        return Err(Error::Sql(
                            "all VALUES must have the same number of terms".into(),
                        ));
                    }
                    out.push(
                        row.iter()
                            .map(|e| self.bind_expr(e))
                            .collect::<Result<Vec<_>>>()?,
                    );
                }
                let cols = (1..=n)
                    .map(|i| ColInfo {
                        name: format!("column{i}"),
                        aff: Affinity::Blob,
                        coll: Collation::Binary,
                    })
                    .collect();
                Ok(Query {
                    body: Body::Values(out),
                    order_by: Vec::new(),
                    limit: None,
                    offset: None,
                    cols,
                })
            }
            SelectBody::Compound { op, left, right } => {
                let l = self.bind_body(left)?;
                let r = self.bind_body(right)?;
                if l.cols.len() != r.cols.len() {
                    let name = match op {
                        ast::CompoundOp::Union => "UNION",
                        ast::CompoundOp::UnionAll => "UNION ALL",
                        ast::CompoundOp::Intersect => "INTERSECT",
                        ast::CompoundOp::Except => "EXCEPT",
                    };
                    return Err(Error::Sql(format!(
                        "SELECTs to the left and right of {name} do not have the same number of result columns"
                    )));
                }
                let cols = l.cols.clone();
                Ok(Query {
                    body: Body::Compound {
                        op: *op,
                        left: Box::new(l),
                        right: Box::new(r),
                    },
                    order_by: Vec::new(),
                    limit: None,
                    offset: None,
                    cols,
                })
            }
        }
    }

    // ------------------------------------------------------------ FROM

    fn table_source(&mut self, table: &Rc<Table>, alias: Option<&str>) -> (Item, ScopeSource) {
        let ncols = table.columns.len();
        let cursor = self.new_cursor(ncols + 1);
        let name = alias.unwrap_or(&table.name).to_string();
        let src = ScopeSource {
            cursor,
            name: lower(&name),
            cols: table
                .columns
                .iter()
                .map(|c| ScopeCol {
                    name: c.name.clone(),
                    aff: c.affinity,
                    coll: c.collation,
                    hidden: false,
                })
                .collect(),
            rowid: if table.without_rowid {
                None
            } else {
                Some(ncols)
            },
        };
        (
            Item {
                cursor,
                name,
                kind: ItemKind::Table(table.clone()),
                left_join: false,
                ncols,
            },
            src,
        )
    }

    fn query_source(
        &mut self,
        query: Query,
        correlated: bool,
        name: &str,
        rename: &[String],
    ) -> Result<(Item, ScopeSource)> {
        if !rename.is_empty() && rename.len() != query.cols.len() {
            return Err(Error::Sql(format!(
                "expected {} columns for '{}' but got {}",
                rename.len(),
                name,
                query.cols.len()
            )));
        }
        let ncols = query.cols.len();
        let cursor = self.new_cursor(ncols);
        let cols = query
            .cols
            .iter()
            .enumerate()
            .map(|(i, c)| ScopeCol {
                name: rename.get(i).cloned().unwrap_or_else(|| c.name.clone()),
                aff: c.aff,
                coll: c.coll,
                hidden: false,
            })
            .collect();
        let src = ScopeSource {
            cursor,
            name: lower(name),
            cols,
            rowid: None,
        };
        let item = Item {
            cursor,
            name: if name.is_empty() {
                "(subquery)".to_string()
            } else {
                name.to_string()
            },
            kind: ItemKind::Sub {
                query: Box::new(query),
                correlated,
            },
            left_join: false,
            ncols,
        };
        Ok((item, src))
    }

    /// Bind a SELECT used as a FROM source. `keep_scopes` / `keep_ctes` say
    /// how much of the surrounding context it may see.
    fn bind_isolated(
        &mut self,
        sel: &ast::Select,
        keep_scopes: usize,
        keep_ctes: usize,
    ) -> Result<(Query, bool)> {
        self.isolated(keep_scopes, keep_ctes, |b| b.bind_query(sel))
    }

    fn isolated(
        &mut self,
        keep_scopes: usize,
        keep_ctes: usize,
        f: impl FnOnce(&mut Self) -> Result<Query>,
    ) -> Result<(Query, bool)> {
        self.view_depth += 1;
        if self.view_depth > 50 {
            self.view_depth -= 1;
            return Err(Error::Sql(
                "view or subquery nesting is too deep (circular view?)".into(),
            ));
        }
        let saved_scopes = self.scopes.split_off(keep_scopes.min(self.scopes.len()));
        let saved_ctes = self.ctes.split_off(keep_ctes.min(self.ctes.len()));
        let saved_aggs = std::mem::take(&mut self.aggs);
        let mark = self.refs.len();
        let first_cursor = self.used.len();
        let result = f(self);
        self.scopes.extend(saved_scopes);
        self.ctes.extend(saved_ctes);
        self.aggs = saved_aggs;
        self.view_depth -= 1;
        let correlated = self.refs[mark..].iter().any(|c| *c < first_cursor);
        Ok((result?, correlated))
    }

    fn resolve_source(&mut self, fi: &FromItem) -> Result<(Item, ScopeSource)> {
        match &fi.source {
            TableSource::Subquery(sel) => {
                let (q, correlated) =
                    self.bind_isolated(sel, self.scopes.len(), self.ctes.len())?;
                self.query_source(q, correlated, fi.alias.as_deref().unwrap_or(""), &[])
            }
            TableSource::Table { schema, name } => {
                let alias = fi.alias.as_deref();
                if schema.is_none() {
                    if let Some(w) = self
                        .work_tables
                        .iter()
                        .rev()
                        .find(|w| w.name == lower(name))
                    {
                        if w.depth != self.query_depth {
                            return Err(Error::Sql(format!(
                                "recursive reference in a subquery: {name}"
                            )));
                        }
                        let q = Query {
                            body: Body::WorkTable(w.id),
                            order_by: Vec::new(),
                            limit: None,
                            offset: None,
                            cols: w.cols.clone(),
                        };
                        // Correlated, so it is read afresh on every step.
                        return self.query_source(q, true, alias.unwrap_or(name), &[]);
                    }
                    if let Some(pos) = self.ctes.iter().rposition(|c| c.name == lower(name)) {
                        let (sel, cols, ctes_len, scopes_len, recursive) = {
                            let c = &self.ctes[pos];
                            (
                                c.select.clone(),
                                c.columns.clone(),
                                c.ctes_len,
                                c.scopes_len,
                                c.recursive,
                            )
                        };
                        let (q, correlated) = if recursive {
                            let cte_name = name.clone();
                            let rename = cols.clone();
                            self.isolated(scopes_len, ctes_len, |b| {
                                b.bind_recursive(&sel, &cte_name, &rename)
                            })?
                        } else {
                            self.bind_isolated(&sel, scopes_len, ctes_len)?
                        };
                        return self.query_source(q, correlated, alias.unwrap_or(name), &cols);
                    }
                }
                if let Some(t) = self.schema.table(name) {
                    let t = t.clone();
                    return Ok(self.table_source(&t, alias));
                }
                if let Some(v) = self.schema.view(name) {
                    let v = v.clone();
                    let (q, _) = self.bind_isolated(&v.select, 0, 0)?;
                    return self.query_source(q, false, alias.unwrap_or(&v.name), &v.columns);
                }
                if let Some(why) = self.schema.broken.get(&lower(name)) {
                    return Err(Error::Unsupported(format!("table {name}: {why}")));
                }
                Err(Error::Sql(format!("no such table: {name}")))
            }
            TableSource::Nested(_) => unreachable!("nested joins are flattened by the caller"),
        }
    }

    fn make_col(&mut self, src: &ScopeSource, col: usize) -> Expr {
        self.refs.push(src.cursor);
        self.used[src.cursor][col] = true;
        match src.cols.get(col) {
            Some(c) => Expr::Col {
                cur: src.cursor,
                col,
                aff: c.aff,
                coll: c.coll,
            },
            None => Expr::Col {
                cur: src.cursor,
                col,
                aff: Affinity::Integer,
                coll: Collation::Binary,
            },
        }
    }

    fn bind_from(
        &mut self,
        list: &[FromItem],
        items: &mut Vec<Item>,
        scope: &mut Scope,
        on_asts: &mut Vec<(ast::Expr, Option<usize>)>,
        bound: &mut Vec<(Expr, Option<usize>)>,
    ) -> Result<()> {
        for fi in list {
            if let TableSource::Nested(inner) = &fi.source {
                if fi.join != JoinKind::Inner || fi.natural || !fi.using.is_empty() {
                    return Err(Error::Unsupported(
                        "a parenthesised join on the right of an outer join".into(),
                    ));
                }
                self.bind_from(inner, items, scope, on_asts, bound)?;
                if let Some(on) = &fi.on {
                    on_asts.push((on.clone(), None));
                }
                continue;
            }
            let (mut item, src) = self.resolve_source(fi)?;
            let idx = items.len();
            let mut left_on = None;
            match fi.join {
                JoinKind::Inner => {}
                JoinKind::Left => {
                    item.left_join = true;
                    left_on = Some(idx);
                }
                JoinKind::Right => {
                    // `a RIGHT JOIN b` is `b LEFT JOIN a`; only supported
                    // when a is the single table before it.
                    if items.len() != 1 || items[0].left_join {
                        return Err(Error::Unsupported(
                            "RIGHT JOIN after more than one table".into(),
                        ));
                    }
                    items[0].left_join = true;
                    items.insert(0, item);
                    scope.sources.push(src);
                    let last = scope.sources.len() - 1;
                    self.join_columns(fi, scope, last, Some(1), bound)?;
                    if let Some(on) = &fi.on {
                        on_asts.push((on.clone(), Some(1)));
                    }
                    // Keep `*` in declaration order: scope order is
                    // unchanged, only the loop order was swapped.
                    continue;
                }
                JoinKind::Full => return Err(Error::Unsupported("FULL OUTER JOIN".into())),
            }
            items.push(item);
            scope.sources.push(src);
            let last = scope.sources.len() - 1;
            self.join_columns(fi, scope, last, left_on, bound)?;
            if let Some(on) = &fi.on {
                on_asts.push((on.clone(), left_on));
            }
        }
        Ok(())
    }

    /// Turn USING / NATURAL into equality terms and hide the right-hand
    /// copies of the merged columns.
    fn join_columns(
        &mut self,
        fi: &FromItem,
        scope: &mut Scope,
        right: usize,
        left_on: Option<usize>,
        bound: &mut Vec<(Expr, Option<usize>)>,
    ) -> Result<()> {
        let names: Vec<String> = if fi.natural {
            scope.sources[right]
                .cols
                .iter()
                .filter(|c| {
                    scope.sources[..right].iter().any(|s| {
                        s.cols
                            .iter()
                            .any(|l| !l.hidden && l.name.eq_ignore_ascii_case(&c.name))
                    })
                })
                .map(|c| c.name.clone())
                .collect()
        } else {
            fi.using.clone()
        };
        for name in names {
            let rcol = scope.sources[right]
                .cols
                .iter()
                .position(|c| c.name.eq_ignore_ascii_case(&name))
                .ok_or_else(|| {
                    Error::Sql(format!(
                        "cannot join using column {name} - column not present in both tables"
                    ))
                })?;
            let mut found = None;
            for (si, s) in scope.sources[..right].iter().enumerate() {
                if let Some(ci) = s
                    .cols
                    .iter()
                    .position(|c| !c.hidden && c.name.eq_ignore_ascii_case(&name))
                {
                    found = Some((si, ci));
                    break;
                }
            }
            let (lsrc, lcol) = found.ok_or_else(|| {
                Error::Sql(format!(
                    "cannot join using column {name} - column not present in both tables"
                ))
            })?;
            let l = self.make_col(&scope.sources[lsrc], lcol);
            let r = self.make_col(&scope.sources[right], rcol);
            let aff = cmp_affinity(affinity(&l), affinity(&r));
            let coll = cmp_collation(&l, &r);
            bound.push((
                Expr::Cmp {
                    op: BinaryOp::Eq,
                    l: Box::new(l),
                    r: Box::new(r),
                    aff,
                    coll,
                },
                left_on,
            ));
            scope.sources[right].cols[rcol].hidden = true;
        }
        Ok(())
    }

    // ------------------------------------------------------------ SELECT core

    fn bind_core(
        &mut self,
        core: &ast::SelectCore,
        order_by: &[ast::OrderTerm],
        limit: &Option<ast::Expr>,
        offset: &Option<ast::Expr>,
    ) -> Result<Query> {
        let mut items = Vec::new();
        let mut scope = Scope::default();
        let mut on_asts = Vec::new();
        let mut bound = Vec::new();
        self.bind_from(&core.from, &mut items, &mut scope, &mut on_asts, &mut bound)?;
        for rc in &core.columns {
            if let ResultColumn::Expr {
                expr,
                alias: Some(a),
                ..
            } = rc
            {
                scope.aliases.push((lower(a), expr.clone()));
            }
        }
        self.scopes.push(scope);
        let slot = self.next_slot;
        self.next_slot += 1;
        self.aggs.push(AggFrame {
            slot,
            calls: Vec::new(),
            allowed: true,
        });
        let result = self.bind_core_clauses(core, order_by, limit, offset, items, on_asts, bound);
        self.aggs.pop();
        self.scopes.pop();
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn bind_core_clauses(
        &mut self,
        core: &ast::SelectCore,
        order_by: &[ast::OrderTerm],
        limit: &Option<ast::Expr>,
        offset: &Option<ast::Expr>,
        items: Vec<Item>,
        on_asts: Vec<(ast::Expr, Option<usize>)>,
        mut bound: Vec<(Expr, Option<usize>)>,
    ) -> Result<Query> {
        // Result columns (aggregates allowed).
        let mut results = Vec::new();
        let mut cols = Vec::new();
        let mut col_asts: Vec<Option<ast::Expr>> = Vec::new();
        for rc in &core.columns {
            match rc {
                ResultColumn::Star | ResultColumn::TableStar(_) => {
                    let want = match rc {
                        ResultColumn::TableStar(t) => Some(lower(t)),
                        _ => None,
                    };
                    let scope = self.scopes.pop().unwrap();
                    let mut any = false;
                    for src in &scope.sources {
                        if want.as_ref().is_some_and(|w| *w != src.name) {
                            continue;
                        }
                        any = true;
                        for (ci, c) in src.cols.iter().enumerate() {
                            if c.hidden && want.is_none() {
                                continue;
                            }
                            results.push(self.make_col(src, ci));
                            cols.push(ColInfo {
                                name: c.name.clone(),
                                aff: c.aff,
                                coll: c.coll,
                            });
                            col_asts.push(None);
                        }
                    }
                    let empty = scope.sources.is_empty();
                    self.scopes.push(scope);
                    if !any {
                        return Err(match want {
                            Some(w) => Error::Sql(format!("no such table: {w}")),
                            None if empty => Error::Sql("no tables specified".into()),
                            None => Error::Sql("no columns to select".into()),
                        });
                    }
                }
                ResultColumn::Expr { expr, alias, text } => {
                    let e = self.bind_expr(expr)?;
                    let name = match (alias, expr) {
                        (Some(a), _) => a.clone(),
                        (None, ast::Expr::Name { name, .. }) => match &e {
                            Expr::Col { cur, col, .. } => {
                                self.column_name(*cur, *col).unwrap_or_else(|| name.clone())
                            }
                            _ => name.clone(),
                        },
                        _ => text.clone(),
                    };
                    cols.push(ColInfo {
                        name,
                        aff: affinity(&e),
                        coll: collation(&e).map_or(Collation::Binary, |c| c.0),
                    });
                    results.push(e);
                    col_asts.push(Some(expr.clone()));
                }
            }
        }

        // WHERE and ON terms (no aggregates).
        self.aggs.last_mut().unwrap().allowed = false;
        for (on, left_on) in &on_asts {
            let mut parts = Vec::new();
            split_and(on, &mut parts);
            for p in parts {
                bound.push((self.bind_expr(p)?, *left_on));
            }
        }
        if let Some(w) = &core.where_ {
            let mut parts = Vec::new();
            split_and(w, &mut parts);
            for p in parts {
                bound.push((self.bind_expr(p)?, None));
            }
        }

        // GROUP BY.
        let mut group_by = Vec::new();
        for g in &core.group_by {
            let e = match g {
                ast::Expr::Literal(Value::Int(k)) => {
                    if *k < 1 || *k as usize > results.len() {
                        return Err(Error::Sql(format!(
                            "GROUP BY term out of range - should be between 1 and {}",
                            results.len()
                        )));
                    }
                    results[*k as usize - 1].clone()
                }
                other => self.bind_expr(other)?,
            };
            if contains_agg(&e) {
                return Err(Error::Sql(
                    "aggregate functions are not allowed in the GROUP BY clause".into(),
                ));
            }
            let coll = collation(&e).map_or(Collation::Binary, |c| c.0);
            group_by.push((e, coll));
        }

        // HAVING and ORDER BY (aggregates allowed again).
        self.aggs.last_mut().unwrap().allowed = true;
        let having = match &core.having {
            Some(h) => Some(self.bind_expr(h)?),
            None => None,
        };
        let mut keys = Vec::new();
        for t in order_by {
            let (inner, explicit) = match &t.expr {
                ast::Expr::Collate { expr, collation } => {
                    (expr.as_ref(), Some(parse_collation(collation)?))
                }
                e => (e, None),
            };
            let n = results.len();
            let src = match inner {
                ast::Expr::Literal(Value::Int(k)) => {
                    if *k < 1 || *k as usize > n {
                        return Err(Error::Sql(format!(
                            "ORDER BY term out of range - should be between 1 and {n}"
                        )));
                    }
                    KeySrc::Result(*k as usize - 1)
                }
                other => {
                    let by_alias = match other {
                        ast::Expr::Name {
                            table: None, name, ..
                        } => core
                            .columns
                            .iter()
                            .enumerate()
                            .find_map(|(i, rc)| match rc {
                                ResultColumn::Expr { alias: Some(a), .. }
                                    if a.eq_ignore_ascii_case(name) =>
                                {
                                    // Position after star expansion.
                                    col_asts
                                        .iter()
                                        .enumerate()
                                        .filter(|(_, a)| a.is_some())
                                        .nth(
                                            core.columns[..i]
                                                .iter()
                                                .filter(|c| matches!(c, ResultColumn::Expr { .. }))
                                                .count(),
                                        )
                                        .map(|(pos, _)| pos)
                                }
                                _ => None,
                            }),
                        _ => None,
                    };
                    match by_alias
                        .or_else(|| col_asts.iter().position(|a| a.as_ref() == Some(other)))
                    {
                        Some(i) => KeySrc::Result(i),
                        None => KeySrc::Expr(self.bind_expr(other)?),
                    }
                }
            };
            let coll = explicit.unwrap_or_else(|| {
                let e = match &src {
                    KeySrc::Result(i) => &results[*i],
                    KeySrc::Expr(e) => e,
                };
                collation(e).map_or(Collation::Binary, |c| c.0)
            });
            keys.push(OrderKey {
                src,
                desc: t.desc,
                nulls_first: t.nulls_first.unwrap_or(!t.desc),
                coll,
            });
        }
        self.aggs.last_mut().unwrap().allowed = false;
        let limit = self.bind_opt_const(limit)?;
        let offset = self.bind_opt_const(offset)?;

        let frame = self.aggs.last_mut().unwrap();
        let calls = std::mem::take(&mut frame.calls);
        let slot = frame.slot;
        let agg = if !group_by.is_empty() || !calls.is_empty() || having.is_some() {
            Some(Agg {
                slot,
                group_by,
                calls,
                having,
            })
        } else {
            None
        };

        let (sources, const_filters) = self.plan_joins(items, bound)?;
        Ok(Query {
            body: Body::Core(Box::new(Core {
                sources,
                const_filters,
                results,
                distinct: core.distinct,
                agg,
            })),
            order_by: keys,
            limit,
            offset,
            cols,
        })
    }

    fn column_name(&self, cur: usize, col: usize) -> Option<String> {
        for scope in self.scopes.iter().rev() {
            for s in &scope.sources {
                if s.cursor == cur {
                    return s.cols.get(col).map(|c| c.name.clone());
                }
            }
        }
        None
    }

    // ------------------------------------------------------------ expressions

    fn resolve_name(&mut self, table: Option<&str>, name: &str, dq: bool) -> Result<Expr> {
        let want = table.map(lower);
        for depth in (0..self.scopes.len()).rev() {
            let mut found: Option<(usize, usize)> = None;
            let mut ambiguous = false;
            let scope = &self.scopes[depth];
            for (si, src) in scope.sources.iter().enumerate() {
                if want.as_ref().is_some_and(|w| *w != src.name) {
                    continue;
                }
                let hit = src
                    .cols
                    .iter()
                    .position(|c| {
                        c.name.eq_ignore_ascii_case(name) && (want.is_some() || !c.hidden)
                    })
                    .or_else(|| if is_rowid_name(name) { src.rowid } else { None });
                if let Some(ci) = hit {
                    if found.is_some() {
                        ambiguous = true;
                    }
                    found.get_or_insert((si, ci));
                }
            }
            if ambiguous {
                return Err(Error::Sql(format!("ambiguous column name: {name}")));
            }
            if let Some((si, ci)) = found {
                let scope = std::mem::take(&mut self.scopes[depth]);
                let e = self.make_col(&scope.sources[si], ci);
                self.scopes[depth] = scope;
                return Ok(e);
            }
            if depth + 1 == self.scopes.len() && want.is_none() {
                let alias = self.scopes[depth]
                    .aliases
                    .iter()
                    .position(|(a, _)| a.eq_ignore_ascii_case(name));
                if let Some(i) = alias {
                    // Bind the aliased expression with aliases switched
                    // off, so `SELECT a AS a` cannot recurse.
                    let aliases = std::mem::take(&mut self.scopes[depth].aliases);
                    let r = self.bind_expr(&aliases[i].1);
                    self.scopes[depth].aliases = aliases;
                    return r;
                }
            }
        }
        if dq && table.is_none() {
            return Ok(Expr::Lit(Value::text(name)));
        }
        Err(Error::Sql(match table {
            Some(t) => format!("no such column: {t}.{name}"),
            None => format!("no such column: {name}"),
        }))
    }

    fn bind_subquery(&mut self, sel: &ast::Select) -> Result<Rc<Subquery>> {
        let mark = self.refs.len();
        let first_cursor = self.used.len();
        let query = self.bind_query(sel)?;
        let mut outer_refs: Vec<usize> = self.refs[mark..]
            .iter()
            .copied()
            .filter(|c| *c < first_cursor)
            .collect();
        outer_refs.sort_unstable();
        outer_refs.dedup();
        let id = self.next_sub;
        self.next_sub += 1;
        Ok(Rc::new(Subquery {
            id,
            query,
            outer_refs,
        }))
    }

    fn cmp(&mut self, op: BinaryOp, l: Expr, r: Expr) -> Expr {
        let aff = cmp_affinity(affinity(&l), affinity(&r));
        let coll = cmp_collation(&l, &r);
        Expr::Cmp {
            op,
            l: Box::new(l),
            r: Box::new(r),
            aff,
            coll,
        }
    }

    pub fn bind_expr(&mut self, e: &ast::Expr) -> Result<Expr> {
        Ok(match e {
            ast::Expr::Literal(v) => Expr::Lit(v.clone()),
            ast::Expr::Param => return Err(Error::Unsupported("bound parameters".into())),
            ast::Expr::Name { table, name, dq } => {
                self.resolve_name(table.as_deref(), name, *dq)?
            }
            ast::Expr::Unary(op, a) => Expr::Unary(*op, Box::new(self.bind_expr(a)?)),
            ast::Expr::Binary(op, a, b) => {
                let l = self.bind_expr(a)?;
                let r = self.bind_expr(b)?;
                match op {
                    BinaryOp::And => Expr::And(Box::new(l), Box::new(r)),
                    BinaryOp::Or => Expr::Or(Box::new(l), Box::new(r)),
                    op if op.is_comparison() => self.cmp(*op, l, r),
                    op => Expr::Arith(*op, Box::new(l), Box::new(r)),
                }
            }
            ast::Expr::IsNull { expr, negated } => {
                Expr::IsNull(Box::new(self.bind_expr(expr)?), *negated)
            }
            ast::Expr::Between {
                expr,
                lo,
                hi,
                negated,
            } => {
                let e = self.bind_expr(expr)?;
                let lo = self.bind_expr(lo)?;
                let hi = self.bind_expr(hi)?;
                let lo_cmp = (
                    cmp_affinity(affinity(&e), affinity(&lo)),
                    cmp_collation(&e, &lo),
                );
                let hi_cmp = (
                    cmp_affinity(affinity(&e), affinity(&hi)),
                    cmp_collation(&e, &hi),
                );
                Expr::Between {
                    e: Box::new(e),
                    lo: Box::new(lo),
                    hi: Box::new(hi),
                    negated: *negated,
                    lo_cmp,
                    hi_cmp,
                }
            }
            ast::Expr::InList {
                expr,
                list,
                negated,
            } => {
                let e = self.bind_expr(expr)?;
                let list = list
                    .iter()
                    .map(|x| self.bind_expr(x))
                    .collect::<Result<Vec<_>>>()?;
                let aff = cmp_affinity(affinity(&e), Affinity::Blob);
                let coll = collation(&e).map_or(Collation::Binary, |c| c.0);
                Expr::InList {
                    e: Box::new(e),
                    list,
                    negated: *negated,
                    aff,
                    coll,
                }
            }
            ast::Expr::InSelect {
                expr,
                select,
                negated,
            } => self.bind_in_select(expr, select, *negated)?,
            ast::Expr::InTable {
                expr,
                table,
                negated,
            } => {
                let sel = ast::Select {
                    with: Vec::new(),
                    body: SelectBody::Core(Box::new(ast::SelectCore {
                        distinct: false,
                        columns: vec![ResultColumn::Star],
                        from: vec![FromItem {
                            join: JoinKind::Inner,
                            natural: false,
                            source: TableSource::Table {
                                schema: None,
                                name: table.clone(),
                            },
                            alias: None,
                            on: None,
                            using: Vec::new(),
                        }],
                        where_: None,
                        group_by: Vec::new(),
                        having: None,
                    })),
                    order_by: Vec::new(),
                    limit: None,
                    offset: None,
                };
                self.bind_in_select(expr, &sel, *negated)?
            }
            ast::Expr::Like {
                op,
                expr,
                pattern,
                escape,
                negated,
            } => {
                if matches!(op, LikeOp::Regexp | LikeOp::Match) {
                    return Err(Error::Unsupported("REGEXP and MATCH operators".into()));
                }
                let esc = match escape {
                    Some(x) => Some(Box::new(self.bind_expr(x)?)),
                    None => None,
                };
                Expr::Like {
                    op: *op,
                    e: Box::new(self.bind_expr(expr)?),
                    pat: Box::new(self.bind_expr(pattern)?),
                    esc,
                    negated: *negated,
                }
            }
            ast::Expr::Case {
                operand,
                whens,
                else_,
            } => {
                let operand = match operand {
                    Some(o) => Some(Box::new(self.bind_expr(o)?)),
                    None => None,
                };
                let mut out = Vec::new();
                for (w, t) in whens {
                    let when = self.bind_expr(w)?;
                    let then = self.bind_expr(t)?;
                    let (aff, coll) = match &operand {
                        Some(o) => (
                            cmp_affinity(affinity(o), affinity(&when)),
                            cmp_collation(o, &when),
                        ),
                        None => (Affinity::Blob, Collation::Binary),
                    };
                    out.push(CaseWhen {
                        when,
                        then,
                        aff,
                        coll,
                    });
                }
                let else_ = match else_ {
                    Some(x) => Some(Box::new(self.bind_expr(x)?)),
                    None => None,
                };
                Expr::Case {
                    operand,
                    whens: out,
                    else_,
                }
            }
            ast::Expr::Cast { expr, type_name } => Expr::Cast(
                Box::new(self.bind_expr(expr)?),
                Affinity::from_decl_type(type_name),
            ),
            ast::Expr::Collate { expr, collation } => {
                Expr::Collate(Box::new(self.bind_expr(expr)?), parse_collation(collation)?)
            }
            ast::Expr::Subquery(sel) => {
                let sub = self.bind_subquery(sel)?;
                if sub.query.cols.len() != 1 {
                    return Err(Error::Sql(format!(
                        "sub-select returns {} columns - expected 1",
                        sub.query.cols.len()
                    )));
                }
                Expr::Subquery(sub)
            }
            ast::Expr::Exists(sel) => Expr::Exists(self.bind_subquery(sel)?),
            ast::Expr::Function {
                name,
                args,
                distinct,
                star,
                filter,
                ..
            } => self.bind_function(name, args, *distinct, *star, filter.as_deref())?,
        })
    }

    fn bind_in_select(
        &mut self,
        expr: &ast::Expr,
        select: &ast::Select,
        negated: bool,
    ) -> Result<Expr> {
        let e = self.bind_expr(expr)?;
        let sub = self.bind_subquery(select)?;
        if sub.query.cols.len() != 1 {
            return Err(Error::Sql(format!(
                "sub-select returns {} columns - expected 1",
                sub.query.cols.len()
            )));
        }
        let aff = cmp_affinity(affinity(&e), sub.query.cols[0].aff);
        let coll = collation(&e).map_or(sub.query.cols[0].coll, |c| c.0);
        Ok(Expr::InSub {
            e: Box::new(e),
            sub,
            negated,
            aff,
            coll,
        })
    }

    fn bind_function(
        &mut self,
        name: &str,
        args: &[ast::Expr],
        distinct: bool,
        star: bool,
        filter: Option<&ast::Expr>,
    ) -> Result<Expr> {
        if let Some(func) = lookup_agg(name, args.len(), star) {
            if star && name != "count" {
                return Err(Error::Sql(format!(
                    "wrong number of arguments to function {name}()"
                )));
            }
            match self.aggs.last() {
                Some(f) if f.allowed => {}
                _ => return Err(Error::Sql(format!("misuse of aggregate function {name}()"))),
            }
            self.aggs.last_mut().unwrap().allowed = false;
            let bound: Result<Vec<Expr>> = args.iter().map(|a| self.bind_expr(a)).collect();
            let filter = filter.map(|f| self.bind_expr(f));
            self.aggs.last_mut().unwrap().allowed = true;
            let bound = bound?;
            let filter = filter.transpose()?;
            if distinct && bound.len() != 1 {
                return Err(Error::Sql(
                    "DISTINCT aggregates must have exactly one argument".into(),
                ));
            }
            let coll = bound
                .first()
                .and_then(collation)
                .map_or(Collation::Binary, |c| c.0);
            let frame = self.aggs.last_mut().unwrap();
            frame.calls.push(AggCall {
                func,
                args: bound,
                distinct,
                filter,
                coll,
            });
            return Ok(Expr::Agg {
                slot: frame.slot,
                idx: frame.calls.len() - 1,
            });
        }
        if star {
            return Err(Error::Sql(format!(
                "wrong number of arguments to function {name}()"
            )));
        }
        match lookup_scalar(name, args.len())? {
            Some(f) => {
                if distinct {
                    return Err(Error::Sql(format!(
                        "DISTINCT is not supported for function {name}()"
                    )));
                }
                if filter.is_some() {
                    return Err(Error::Sql(
                        "FILTER may not be used with non-aggregate functions".into(),
                    ));
                }
                let bound = args
                    .iter()
                    .map(|a| self.bind_expr(a))
                    .collect::<Result<Vec<_>>>()?;
                Ok(Expr::Func {
                    name: intern(name),
                    f,
                    args: bound,
                })
            }
            None => Err(Error::Sql(format!("no such function: {name}"))),
        }
    }

    // ------------------------------------------------------------ join planning

    fn constraints(&self, conj: &Expr, cursor: usize, out: &mut Vec<Constraint>) {
        let reads = |e: &Expr| {
            let mut r = Vec::new();
            refs(e, &mut r);
            r.contains(&cursor)
        };
        match conj {
            Expr::Cmp {
                op,
                l,
                r,
                aff,
                coll,
            } if matches!(
                op,
                BinaryOp::Eq | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
            ) =>
            {
                if let Expr::Col { cur, col, .. } = l.as_ref() {
                    if *cur == cursor && !reads(r) {
                        out.push(Constraint {
                            col: *col,
                            op: *op,
                            rhs: (**r).clone(),
                            aff: *aff,
                            coll: *coll,
                        });
                    }
                }
                if let Expr::Col { cur, col, .. } = r.as_ref() {
                    if *cur == cursor && !reads(l) {
                        let flipped = match op {
                            BinaryOp::Lt => BinaryOp::Gt,
                            BinaryOp::Le => BinaryOp::Ge,
                            BinaryOp::Gt => BinaryOp::Lt,
                            BinaryOp::Ge => BinaryOp::Le,
                            o => *o,
                        };
                        out.push(Constraint {
                            col: *col,
                            op: flipped,
                            rhs: (**l).clone(),
                            aff: *aff,
                            coll: *coll,
                        });
                    }
                }
            }
            Expr::Between {
                e,
                lo,
                hi,
                negated: false,
                lo_cmp,
                hi_cmp,
            } => {
                if let Expr::Col { cur, col, .. } = e.as_ref() {
                    if *cur == cursor {
                        if !reads(lo) {
                            out.push(Constraint {
                                col: *col,
                                op: BinaryOp::Ge,
                                rhs: (**lo).clone(),
                                aff: lo_cmp.0,
                                coll: lo_cmp.1,
                            });
                        }
                        if !reads(hi) {
                            out.push(Constraint {
                                col: *col,
                                op: BinaryOp::Le,
                                rhs: (**hi).clone(),
                                aff: hi_cmp.0,
                                coll: hi_cmp.1,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Choose the cheapest way to reach `item` given which other items are
    /// already bound by outer loops. Returns the estimated number of rows
    /// visited per outer row and the access.
    fn best_access(
        &self,
        item: &Item,
        idx: usize,
        placed: &[bool],
        conjs: &[Conj],
    ) -> (f64, Option<Access>, Option<HashProbe>) {
        let mut cons = Vec::new();
        let mut filtered = false;
        for c in conjs {
            let mine = if item.left_join {
                c.left_on == Some(idx)
            } else {
                c.left_on.is_none()
            };
            if !mine || !c.items.contains(&idx) || !c.items.iter().all(|i| *i == idx || placed[*i])
            {
                continue;
            }
            if c.items.len() == 1 {
                filtered = true;
            }
            self.constraints(&c.expr, item.cursor, &mut cons);
        }
        let varies = |e: &Expr| {
            let mut r = Vec::new();
            refs(e, &mut r);
            !r.is_empty()
        };
        let hash_probe = |cons: &[Constraint], ok: &dyn Fn(usize) -> bool| -> Option<HashProbe> {
            let mut h = HashProbe {
                cols: Vec::new(),
                probes: Vec::new(),
                cmps: Vec::new(),
            };
            for c in cons {
                if c.op == BinaryOp::Eq && ok(c.col) && varies(&c.rhs) && !h.cols.contains(&c.col) {
                    h.cols.push(c.col);
                    h.probes.push(c.rhs.clone());
                    h.cmps.push((c.aff, c.coll));
                }
            }
            if h.cols.is_empty() {
                None
            } else {
                Some(h)
            }
        };
        let table = match &item.kind {
            ItemKind::Sub { .. } => {
                let n = 100.0;
                return match hash_probe(&cons, &|_| true) {
                    Some(h) => (n / 10.0, None, Some(h)),
                    None => (if filtered { n / 4.0 } else { n }, None, None),
                };
            }
            ItemKind::Table(t) => t,
        };
        let n = (table.row_estimate(self.pager) as f64).max(1.0);
        let mut best: (f64, Access) = (if filtered { (n / 4.0).max(1.0) } else { n }, Access::Scan);
        if table.without_rowid {
            if let Some(h) = hash_probe(&cons, &|_| true) {
                let est = (n / 10.0).max(1.0) * 1.01;
                if est < best.0 {
                    best = (est, Access::Hash(h));
                }
            }
            return (best.0, Some(best.1), None);
        }
        let rowid_slot = table.columns.len();
        let is_rowid = |col: usize| col == rowid_slot || Some(col) == table.ipk;
        let rowid_ok = |c: &Constraint| is_rowid(c.col) && c.aff != Affinity::Text;
        // rowid = ?
        if let Some(c) = cons.iter().find(|c| c.op == BinaryOp::Eq && rowid_ok(c)) {
            return (1.0, Some(Access::Rowid(c.rhs.clone())), None);
        }
        // Indexes.
        for index in table
            .indexes
            .iter()
            .filter(|i| i.usable && !i.cols.is_empty())
        {
            let fits = |c: &Constraint, k: usize| {
                let ic = &index.cols[k];
                let col_aff = table.columns[ic.col].affinity;
                let aff_ok = match c.aff {
                    Affinity::Blob => true,
                    Affinity::Text => col_aff == Affinity::Text,
                    _ => col_aff.is_numeric(),
                };
                c.col == ic.col && c.coll == ic.collation && aff_ok
            };
            let mut eq = Vec::new();
            for k in 0..index.cols.len() {
                match cons.iter().find(|c| c.op == BinaryOp::Eq && fits(c, k)) {
                    Some(c) => eq.push((c.rhs.clone(), c.aff)),
                    None => break,
                }
            }
            let (mut lo, mut hi) = (None, None);
            if eq.len() < index.cols.len() {
                let k = eq.len();
                for c in cons.iter().filter(|c| fits(c, k)) {
                    match c.op {
                        BinaryOp::Gt | BinaryOp::Ge if lo.is_none() => {
                            lo = Some((c.rhs.clone(), c.aff, c.op == BinaryOp::Ge))
                        }
                        BinaryOp::Lt | BinaryOp::Le if hi.is_none() => {
                            hi = Some((c.rhs.clone(), c.aff, c.op == BinaryOp::Le))
                        }
                        _ => {}
                    }
                }
            }
            if eq.is_empty() && lo.is_none() && hi.is_none() {
                continue;
            }
            let est = if index.unique && eq.len() == index.cols.len() {
                1.0
            } else {
                let mut e = n / 10f64.powi(eq.len() as i32);
                if lo.is_some() || hi.is_some() {
                    e /= 3.0;
                }
                if lo.is_some() && hi.is_some() {
                    e /= 2.0;
                }
                e.max(1.0)
            };
            if est < best.0 || (est == best.0 && matches!(best.1, Access::Scan) && n > 1.0) {
                let covering = self.used[item.cursor]
                    .iter()
                    .enumerate()
                    .all(|(col, used)| {
                        !used || is_rowid(col) || index.cols.iter().any(|ic| ic.col == col)
                    });
                best = (
                    est,
                    Access::Index(IndexAccess {
                        index: index.clone(),
                        eq,
                        lo,
                        hi,
                        covering,
                    }),
                );
            }
        }
        // rowid range
        let lo = cons
            .iter()
            .find(|c| matches!(c.op, BinaryOp::Gt | BinaryOp::Ge) && rowid_ok(c))
            .map(|c| (c.rhs.clone(), c.op == BinaryOp::Ge));
        let hi = cons
            .iter()
            .find(|c| matches!(c.op, BinaryOp::Lt | BinaryOp::Le) && rowid_ok(c))
            .map(|c| (c.rhs.clone(), c.op == BinaryOp::Le));
        if lo.is_some() || hi.is_some() {
            let est = (n / if lo.is_some() && hi.is_some() {
                6.0
            } else {
                3.0
            })
            .max(1.0);
            if est < best.0 {
                best = (est, Access::RowidRange { lo, hi });
            }
        }
        // Transient hash index.
        if let Some(h) = hash_probe(&cons, &|col| !is_rowid(col)) {
            let est = (n / 10.0).max(1.0) * 1.01;
            if est < best.0 {
                best = (est, Access::Hash(h));
            }
        }
        (best.0, Some(best.1), None)
    }

    fn plan_joins(
        &mut self,
        items: Vec<Item>,
        bound: Vec<(Expr, Option<usize>)>,
    ) -> Result<(Vec<Source>, Vec<Expr>)> {
        let local: HashMap<usize, usize> = items
            .iter()
            .enumerate()
            .map(|(i, it)| (it.cursor, i))
            .collect();
        let mut conjs: Vec<Conj> = bound
            .into_iter()
            .map(|(expr, left_on)| {
                let mut r = Vec::new();
                refs(&expr, &mut r);
                let mut its: Vec<usize> = r.iter().filter_map(|c| local.get(c).copied()).collect();
                its.sort_unstable();
                its.dedup();
                Conj {
                    expr,
                    left_on,
                    items: its,
                }
            })
            .collect();

        let n = items.len();
        let mut placed = vec![false; n];
        let mut order: Vec<(usize, Option<Access>, Option<HashProbe>)> = Vec::new();
        let reorder = n > 1 && !items.iter().any(|i| i.left_join);
        for step in 0..n {
            let candidates: Vec<usize> = if reorder {
                (0..n).filter(|i| !placed[*i]).collect()
            } else {
                vec![step]
            };
            let mut best: Option<(f64, usize, Option<Access>, Option<HashProbe>)> = None;
            for i in candidates {
                let (est, access, hash) = self.best_access(&items[i], i, &placed, &conjs);
                if best.as_ref().is_none_or(|b| est < b.0) {
                    best = Some((est, i, access, hash));
                }
            }
            let (_, i, access, hash) = best.unwrap();
            placed[i] = true;
            order.push((i, access, hash));
        }

        let mut position = vec![0usize; n];
        for (pos, (i, _, _)) in order.iter().enumerate() {
            position[*i] = pos;
        }
        let mut filters: Vec<Vec<Expr>> = (0..n).map(|_| Vec::new()).collect();
        let mut on_filters: Vec<Vec<Expr>> = (0..n).map(|_| Vec::new()).collect();
        let mut const_filters = Vec::new();
        for c in conjs.drain(..) {
            let level = c.items.iter().map(|i| position[*i]).max();
            match (c.left_on, level) {
                (Some(owner), _) => {
                    if level.is_some_and(|l| l > position[owner]) {
                        return Err(Error::Sql(
                            "ON clause references tables to its right".into(),
                        ));
                    }
                    on_filters[position[owner]].push(c.expr);
                }
                (None, Some(l)) => filters[l].push(c.expr),
                (None, None) => const_filters.push(c.expr),
            }
        }
        let mut items: Vec<Option<Item>> = items.into_iter().map(Some).collect();
        let mut sources = Vec::new();
        for (pos, (i, access, hash)) in order.into_iter().enumerate() {
            let item = items[i].take().unwrap();
            let kind = match item.kind {
                ItemKind::Table(table) => SourceKind::Table {
                    table,
                    access: access.unwrap_or(Access::Scan),
                },
                ItemKind::Sub { query, correlated } => SourceKind::Subquery {
                    query,
                    correlated,
                    hash,
                },
            };
            sources.push(Source {
                cursor: item.cursor,
                name: item.name,
                kind,
                left_join: item.left_join,
                on_filters: std::mem::take(&mut on_filters[pos]),
                filters: std::mem::take(&mut filters[pos]),
                ncols: item.ncols,
            });
        }
        Ok((sources, const_filters))
    }
}

fn parse_collation(name: &str) -> Result<Collation> {
    Collation::from_name(name)
        .ok_or_else(|| Error::Sql(format!("no such collation sequence: {name}")))
}

/// Function names live for the whole program; map a name to the static
/// string in the function table so plans do not own strings.
fn intern(name: &str) -> &'static str {
    const NAMES: &[&str] = &[
        "abs",
        "coalesce",
        "ifnull",
        "nullif",
        "iif",
        "if",
        "length",
        "octet_length",
        "lower",
        "upper",
        "substr",
        "substring",
        "trim",
        "ltrim",
        "rtrim",
        "replace",
        "instr",
        "round",
        "typeof",
        "hex",
        "quote",
        "min",
        "max",
        "random",
        "zeroblob",
        "char",
        "unicode",
        "like",
        "glob",
        "printf",
        "format",
        "sign",
        "likely",
        "unlikely",
        "likelihood",
        "concat",
        "concat_ws",
        "sqlite_version",
    ];
    NAMES
        .iter()
        .find(|n| **n == name)
        .copied()
        .unwrap_or("function")
}

fn contains_agg(e: &Expr) -> bool {
    match e {
        Expr::Agg { .. } => true,
        Expr::Lit(_) | Expr::Col { .. } | Expr::Subquery(_) | Expr::Exists(_) => false,
        Expr::Unary(_, a) | Expr::IsNull(a, _) | Expr::Cast(a, _) | Expr::Collate(a, _) => {
            contains_agg(a)
        }
        Expr::Arith(_, a, b) | Expr::And(a, b) | Expr::Or(a, b) => {
            contains_agg(a) || contains_agg(b)
        }
        Expr::Cmp { l, r, .. } => contains_agg(l) || contains_agg(r),
        Expr::Between { e, lo, hi, .. } => contains_agg(e) || contains_agg(lo) || contains_agg(hi),
        Expr::InList { e, list, .. } => contains_agg(e) || list.iter().any(contains_agg),
        Expr::InSub { e, .. } => contains_agg(e),
        Expr::Like { e, pat, .. } => contains_agg(e) || contains_agg(pat),
        Expr::Case {
            operand,
            whens,
            else_,
        } => {
            operand.as_deref().is_some_and(contains_agg)
                || whens
                    .iter()
                    .any(|w| contains_agg(&w.when) || contains_agg(&w.then))
                || else_.as_deref().is_some_and(contains_agg)
        }
        Expr::Func { args, .. } => args.iter().any(contains_agg),
    }
}

/// Result-column expressions of the left-most SELECT of a compound, used to
/// match ORDER BY terms written as expressions.
fn leftmost_columns(body: &SelectBody) -> Vec<Option<ast::Expr>> {
    match body {
        SelectBody::Core(c) => c
            .columns
            .iter()
            .map(|rc| match rc {
                ResultColumn::Expr { expr, .. } => Some(expr.clone()),
                _ => None,
            })
            .collect(),
        SelectBody::Values(_) => Vec::new(),
        SelectBody::Compound { left, .. } => leftmost_columns(left),
    }
}

/// Does the FROM clause of `sel` (at any depth of compound) name `table`?
fn select_mentions(sel: &ast::Select, table: &str) -> bool {
    fn body(b: &SelectBody, t: &str) -> bool {
        match b {
            SelectBody::Core(c) => from(&c.from, t),
            SelectBody::Values(_) => false,
            SelectBody::Compound { left, right, .. } => body(left, t) || body(right, t),
        }
    }
    fn from(items: &[FromItem], t: &str) -> bool {
        items.iter().any(|i| match &i.source {
            TableSource::Table { name, .. } => name.eq_ignore_ascii_case(t),
            TableSource::Subquery(s) => body(&s.body, t),
            TableSource::Nested(n) => from(n, t),
        })
    }
    body(&sel.body, table)
}
