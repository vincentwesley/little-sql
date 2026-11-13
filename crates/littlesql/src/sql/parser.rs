//! A hand-written recursive-descent parser. Expressions use precedence
//! climbing with SQLite's operator precedence table.

use super::ast::*;
use super::lexer::{tokenize, Tok, Token};
use crate::error::{Error, Result};
use crate::value::Value;

/// Words that can never be used as a bare identifier or alias.
const RESERVED: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "AND",
    "AS",
    "AUTOINCREMENT",
    "BETWEEN",
    "CASE",
    "CHECK",
    "COLLATE",
    "COMMIT",
    "CONSTRAINT",
    "CREATE",
    "CROSS",
    "DEFAULT",
    "DEFERRABLE",
    "DELETE",
    "DISTINCT",
    "DROP",
    "ELSE",
    "ESCAPE",
    "EXCEPT",
    "EXISTS",
    "FOREIGN",
    "FROM",
    "FULL",
    "GROUP",
    "HAVING",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "ISNULL",
    "JOIN",
    "LEFT",
    "LIMIT",
    "NATURAL",
    "NOT",
    "NOTNULL",
    "NULL",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "PRIMARY",
    "REFERENCES",
    "RIGHT",
    "SELECT",
    "SET",
    "TABLE",
    "THEN",
    "TO",
    "TRANSACTION",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VALUES",
    "WHEN",
    "WHERE",
    "RETURNING",
    "WINDOW",
];

fn is_reserved(word: &str) -> bool {
    RESERVED.iter().any(|k| word.eq_ignore_ascii_case(k))
}

pub struct Parser<'a> {
    sql: &'a str,
    toks: Vec<Token>,
    pos: usize,
    depth: usize,
}

/// Parse every statement in `sql`. Each statement is returned with its
/// source text (needed to store `CREATE` statements in the schema table).
pub fn parse_statements(sql: &str) -> Result<Vec<(Stmt, String)>> {
    let mut p = Parser {
        sql,
        toks: tokenize(sql)?,
        pos: 0,
        depth: 0,
    };
    let mut out = Vec::new();
    loop {
        while p.eat_op(";") {}
        if p.at_eof() {
            return Ok(out);
        }
        let start = p.toks[p.pos].start;
        let stmt = p.statement()?;
        let end = p.toks[p.pos - 1].end;
        if !p.at_eof() && !p.eat_op(";") {
            return Err(p.unexpected());
        }
        out.push((stmt, sql[start..end].to_string()));
    }
}

/// Parse exactly one statement.
pub fn parse_one(sql: &str) -> Result<Stmt> {
    let mut v = parse_statements(sql)?;
    match v.len() {
        1 => Ok(v.pop().unwrap().0),
        0 => Err(Error::Parse("empty statement".into())),
        _ => Err(Error::Parse("expected a single statement".into())),
    }
}

/// Parse a standalone expression (used for stored DEFAULT values).
pub fn parse_expr(sql: &str) -> Result<Expr> {
    let mut p = Parser {
        sql,
        toks: tokenize(sql)?,
        pos: 0,
        depth: 0,
    };
    let e = p.expr()?;
    if !p.at_eof() {
        return Err(p.unexpected());
    }
    Ok(e)
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }

    fn peek_at(&self, n: usize) -> &Tok {
        &self.toks[(self.pos + n).min(self.toks.len() - 1)].tok
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek(), Tok::Eof)
    }

    fn advance(&mut self) -> Tok {
        let t = self.toks[self.pos].tok.clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    fn unexpected(&self) -> Error {
        let t = &self.toks[self.pos];
        if matches!(t.tok, Tok::Eof) {
            Error::Parse("incomplete input".into())
        } else {
            Error::Parse(format!(
                "near \"{}\": syntax error",
                &self.sql[t.start..t.end]
            ))
        }
    }

    fn is_kw_at(&self, n: usize, kw: &str) -> bool {
        matches!(self.peek_at(n), Tok::Word(w) if w.eq_ignore_ascii_case(kw))
    }

    fn is_kw(&self, kw: &str) -> bool {
        self.is_kw_at(0, kw)
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.is_kw(kw) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_kw(&mut self, kw: &str) -> Result<()> {
        if self.eat_kw(kw) {
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(), Tok::Op(o) if *o == op)
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if self.is_op(op) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_op(&mut self, op: &str) -> Result<()> {
        if self.eat_op(op) {
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    /// An identifier: a non-reserved bare word or any quoted name.
    fn ident(&mut self) -> Result<String> {
        match self.peek() {
            Tok::Word(w) if !is_reserved(w) => {
                let w = w.clone();
                self.pos += 1;
                Ok(w)
            }
            Tok::Quoted { name, .. } => {
                let n = name.clone();
                self.pos += 1;
                Ok(n)
            }
            Tok::Str(s) => {
                // SQLite accepts 'name' where an identifier is required.
                let s = s.clone();
                self.pos += 1;
                Ok(s)
            }
            _ => Err(self.unexpected()),
        }
    }

    /// `[schema.]name`, discarding the schema.
    fn qualified_name(&mut self) -> Result<String> {
        let first = self.ident()?;
        if self.eat_op(".") {
            return self.ident();
        }
        Ok(first)
    }

    fn opt_alias(&mut self) -> Result<Option<String>> {
        if self.eat_kw("AS") {
            return Ok(Some(self.ident()?));
        }
        match self.peek() {
            // `t INDEXED BY i` and `t NOT INDEXED` are not aliases.
            Tok::Word(w) if w.eq_ignore_ascii_case("INDEXED") && self.is_kw_at(1, "BY") => Ok(None),
            Tok::Word(w) if !is_reserved(w) => Ok(Some(self.ident()?)),
            Tok::Quoted { .. } | Tok::Str(_) => Ok(Some(self.ident()?)),
            _ => Ok(None),
        }
    }

    fn unsupported<T>(&self, what: &str) -> Result<T> {
        Err(Error::Unsupported(what.to_string()))
    }

    // ---------------------------------------------------------------- statements

    fn statement(&mut self) -> Result<Stmt> {
        if self.eat_kw("EXPLAIN") {
            let query_plan = if self.eat_kw("QUERY") {
                self.expect_kw("PLAN")?;
                true
            } else {
                false
            };
            return Ok(Stmt::Explain {
                query_plan,
                stmt: Box::new(self.statement()?),
            });
        }
        if self.is_kw("SELECT") || self.is_kw("VALUES") || self.is_kw("WITH") {
            return Ok(Stmt::Select(self.select()?));
        }
        if self.eat_kw("CREATE") {
            return self.create();
        }
        if self.eat_kw("DROP") {
            let kind = if self.eat_kw("TABLE") {
                DropKind::Table
            } else if self.eat_kw("INDEX") {
                DropKind::Index
            } else if self.eat_kw("VIEW") {
                DropKind::View
            } else if self.is_kw("TRIGGER") {
                return self.unsupported("triggers");
            } else {
                return Err(self.unexpected());
            };
            let if_exists = if self.eat_kw("IF") {
                self.expect_kw("EXISTS")?;
                true
            } else {
                false
            };
            return Ok(Stmt::Drop {
                kind,
                if_exists,
                name: self.qualified_name()?,
            });
        }
        if self.is_kw("INSERT") || self.is_kw("REPLACE") {
            return self.insert();
        }
        if self.eat_kw("UPDATE") {
            return self.update();
        }
        if self.eat_kw("DELETE") {
            self.expect_kw("FROM")?;
            let table = self.qualified_name()?;
            let where_ = if self.eat_kw("WHERE") {
                Some(self.expr()?)
            } else {
                None
            };
            return Ok(Stmt::Delete(Delete { table, where_ }));
        }
        if self.eat_kw("BEGIN") {
            let _ = self.eat_kw("DEFERRED") || self.eat_kw("IMMEDIATE") || self.eat_kw("EXCLUSIVE");
            self.eat_kw("TRANSACTION");
            return Ok(Stmt::Begin);
        }
        if self.eat_kw("COMMIT") || self.eat_kw("END") {
            self.eat_kw("TRANSACTION");
            return Ok(Stmt::Commit);
        }
        if self.eat_kw("ROLLBACK") {
            self.eat_kw("TRANSACTION");
            if self.is_kw("TO") {
                return self.unsupported("savepoints");
            }
            return Ok(Stmt::Rollback);
        }
        if self.eat_kw("PRAGMA") {
            let name = self.qualified_name()?;
            let value = if self.eat_op("=") {
                Some(self.pragma_value()?)
            } else if self.eat_op("(") {
                let v = self.pragma_value()?;
                self.expect_op(")")?;
                Some(v)
            } else {
                None
            };
            return Ok(Stmt::Pragma { name, value });
        }
        for (kw, name) in [
            ("ANALYZE", "ANALYZE"),
            ("REINDEX", "REINDEX"),
            ("VACUUM", "VACUUM"),
        ] {
            if self.eat_kw(kw) {
                while !self.at_eof() && !self.is_op(";") {
                    self.pos += 1;
                }
                return Ok(Stmt::Noop(name));
            }
        }
        for (kw, what) in [
            ("ALTER", "ALTER TABLE"),
            ("SAVEPOINT", "savepoints"),
            ("RELEASE", "savepoints"),
            ("ATTACH", "ATTACH"),
            ("DETACH", "DETACH"),
        ] {
            if self.is_kw(kw) {
                return self.unsupported(what);
            }
        }
        Err(self.unexpected())
    }

    fn pragma_value(&mut self) -> Result<String> {
        let neg = self.eat_op("-");
        let v = match self.advance() {
            Tok::Word(w) => w,
            Tok::Num(n) => n,
            Tok::Str(s) => s,
            Tok::Quoted { name, .. } => name,
            _ => return Err(self.unexpected()),
        };
        Ok(if neg { format!("-{v}") } else { v })
    }

    fn if_not_exists(&mut self) -> Result<bool> {
        if self.eat_kw("IF") {
            self.expect_kw("NOT")?;
            self.expect_kw("EXISTS")?;
            return Ok(true);
        }
        Ok(false)
    }

    fn create(&mut self) -> Result<Stmt> {
        let temp = self.eat_kw("TEMP") || self.eat_kw("TEMPORARY");
        if self.eat_kw("TABLE") {
            let if_not_exists = self.if_not_exists()?;
            let name = self.qualified_name()?;
            return self.create_table_body(name, temp, if_not_exists);
        }
        if self.eat_kw("VIEW") {
            let if_not_exists = self.if_not_exists()?;
            let name = self.qualified_name()?;
            let mut columns = Vec::new();
            if self.eat_op("(") {
                loop {
                    columns.push(self.ident()?);
                    if !self.eat_op(",") {
                        break;
                    }
                }
                self.expect_op(")")?;
            }
            self.expect_kw("AS")?;
            let select = self.select()?;
            return Ok(Stmt::CreateView(CreateView {
                name,
                temp,
                if_not_exists,
                columns,
                select,
            }));
        }
        let unique = self.eat_kw("UNIQUE");
        if self.eat_kw("INDEX") {
            let if_not_exists = self.if_not_exists()?;
            let name = self.qualified_name()?;
            self.expect_kw("ON")?;
            let table = self.ident()?;
            self.expect_op("(")?;
            let columns = self.indexed_columns()?;
            self.expect_op(")")?;
            let partial = if self.eat_kw("WHERE") {
                self.expr()?;
                true
            } else {
                false
            };
            return Ok(Stmt::CreateIndex(CreateIndex {
                name,
                table,
                unique,
                if_not_exists,
                columns,
                partial,
            }));
        }
        if self.is_kw("TRIGGER") {
            return self.unsupported("triggers");
        }
        if self.is_kw("VIRTUAL") {
            return self.unsupported("virtual tables");
        }
        Err(self.unexpected())
    }

    fn indexed_columns(&mut self) -> Result<Vec<IndexedColumn>> {
        let mut cols = Vec::new();
        loop {
            let e = self.expr()?;
            let (name, collation) = match e {
                Expr::Name {
                    table: None, name, ..
                } => (Some(name), None),
                Expr::Literal(Value::Text(s)) => (Some(s.to_string()), None),
                Expr::Collate { expr, collation } => match *expr {
                    Expr::Name {
                        table: None, name, ..
                    } => (Some(name), Some(collation)),
                    _ => (None, Some(collation)),
                },
                _ => (None, None),
            };
            let desc = if self.eat_kw("DESC") {
                true
            } else {
                self.eat_kw("ASC");
                false
            };
            cols.push(IndexedColumn {
                name,
                collation,
                desc,
            });
            if !self.eat_op(",") {
                return Ok(cols);
            }
        }
    }

    fn conflict_clause(&mut self) -> Result<()> {
        if self.is_kw("ON") && self.is_kw_at(1, "CONFLICT") {
            self.pos += 3;
        }
        Ok(())
    }

    fn skip_references(&mut self) -> Result<()> {
        self.ident()?;
        if self.eat_op("(") {
            while !self.eat_op(")") {
                if self.at_eof() {
                    return Err(self.unexpected());
                }
                self.pos += 1;
            }
        }
        loop {
            if self.eat_kw("ON") {
                // ON DELETE|UPDATE  SET NULL|SET DEFAULT|CASCADE|RESTRICT|NO ACTION
                self.pos += 1;
                // SET NULL / NO ACTION take two words; the first was eaten
                // above when present, so skip exactly one more word.
                let _ = self.eat_kw("SET") || self.eat_kw("NO");
                self.pos += 1;
            } else if self.eat_kw("MATCH") {
                self.pos += 1;
            } else if self.is_kw("DEFERRABLE")
                || (self.is_kw("NOT") && self.is_kw_at(1, "DEFERRABLE"))
            {
                self.eat_kw("NOT");
                self.pos += 1;
                if self.eat_kw("INITIALLY") {
                    self.pos += 1;
                }
            } else {
                return Ok(());
            }
        }
    }

    fn type_name(&mut self) -> Result<String> {
        let mut parts: Vec<String> = Vec::new();
        const STOP: &[&str] = &["GENERATED", "AS"];
        while let Tok::Word(w) = self.peek() {
            if is_reserved(w) || STOP.iter().any(|s| w.eq_ignore_ascii_case(s)) {
                break;
            }
            parts.push(w.clone());
            self.pos += 1;
        }
        let mut name = parts.join(" ");
        if !parts.is_empty() && self.is_op("(") {
            let start = self.toks[self.pos].start;
            while !self.eat_op(")") {
                if self.at_eof() {
                    return Err(self.unexpected());
                }
                self.pos += 1;
            }
            name.push_str(&self.sql[start..self.toks[self.pos - 1].end]);
        }
        Ok(name)
    }

    fn create_table_body(&mut self, name: String, temp: bool, if_not_exists: bool) -> Result<Stmt> {
        let mut ct = CreateTable {
            name,
            temp,
            if_not_exists,
            columns: Vec::new(),
            constraints: Vec::new(),
            without_rowid: false,
            as_select: None,
        };
        if self.eat_kw("AS") {
            ct.as_select = Some(Box::new(self.select()?));
            return Ok(Stmt::CreateTable(ct));
        }
        self.expect_op("(")?;
        loop {
            let table_constraint = self.is_kw("CONSTRAINT")
                || self.is_kw("PRIMARY")
                || self.is_kw("UNIQUE")
                || self.is_kw("CHECK")
                || self.is_kw("FOREIGN");
            if table_constraint && !ct.columns.is_empty() {
                if self.eat_kw("CONSTRAINT") {
                    self.ident()?;
                }
                if self.eat_kw("PRIMARY") {
                    self.expect_kw("KEY")?;
                    self.expect_op("(")?;
                    let cols = self.indexed_columns()?;
                    self.eat_kw("AUTOINCREMENT");
                    self.expect_op(")")?;
                    self.conflict_clause()?;
                    ct.constraints.push(TableConstraint::PrimaryKey(cols));
                } else if self.eat_kw("UNIQUE") {
                    self.expect_op("(")?;
                    let cols = self.indexed_columns()?;
                    self.expect_op(")")?;
                    self.conflict_clause()?;
                    ct.constraints.push(TableConstraint::Unique(cols));
                } else if self.eat_kw("CHECK") {
                    self.expect_op("(")?;
                    self.expr()?;
                    self.expect_op(")")?;
                    self.conflict_clause()?;
                    ct.constraints.push(TableConstraint::Other);
                } else if self.eat_kw("FOREIGN") {
                    self.expect_kw("KEY")?;
                    self.expect_op("(")?;
                    while !self.eat_op(")") {
                        if self.at_eof() {
                            return Err(self.unexpected());
                        }
                        self.pos += 1;
                    }
                    self.expect_kw("REFERENCES")?;
                    self.skip_references()?;
                    ct.constraints.push(TableConstraint::Other);
                } else {
                    return Err(self.unexpected());
                }
            } else {
                ct.columns.push(self.column_def()?);
            }
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op(")")?;
        loop {
            if self.eat_kw("WITHOUT") {
                match self.advance() {
                    Tok::Word(w) if w.eq_ignore_ascii_case("ROWID") => ct.without_rowid = true,
                    _ => return Err(self.unexpected()),
                }
            } else if !self.eat_kw("STRICT") {
                break;
            }
            if !self.eat_op(",") {
                break;
            }
        }
        Ok(Stmt::CreateTable(ct))
    }

    fn column_def(&mut self) -> Result<ColumnDef> {
        let name = self.ident()?;
        let type_name = self.type_name()?;
        let mut col = ColumnDef {
            name,
            type_name,
            primary_key: false,
            pk_desc: false,
            autoincrement: false,
            not_null: false,
            unique: false,
            default: None,
            collation: None,
            generated: false,
        };
        loop {
            if self.eat_kw("CONSTRAINT") {
                self.ident()?;
            }
            if self.eat_kw("PRIMARY") {
                self.expect_kw("KEY")?;
                col.primary_key = true;
                if self.eat_kw("DESC") {
                    col.pk_desc = true;
                } else {
                    self.eat_kw("ASC");
                }
                self.conflict_clause()?;
                col.autoincrement = self.eat_kw("AUTOINCREMENT");
            } else if self.eat_kw("NOT") {
                self.expect_kw("NULL")?;
                col.not_null = true;
                self.conflict_clause()?;
            } else if self.eat_kw("NULL") {
                self.conflict_clause()?;
            } else if self.eat_kw("UNIQUE") {
                col.unique = true;
                self.conflict_clause()?;
            } else if self.eat_kw("CHECK") {
                self.expect_op("(")?;
                self.expr()?;
                self.expect_op(")")?;
            } else if self.eat_kw("DEFAULT") {
                col.default = Some(if self.eat_op("(") {
                    let e = self.expr()?;
                    self.expect_op(")")?;
                    e
                } else {
                    self.expr_bp(12)?
                });
            } else if self.eat_kw("COLLATE") {
                col.collation = Some(self.ident()?);
            } else if self.eat_kw("REFERENCES") {
                self.skip_references()?;
            } else if self.is_kw("GENERATED") || self.is_kw("AS") {
                if self.eat_kw("GENERATED") {
                    self.expect_kw("ALWAYS")?;
                }
                self.expect_kw("AS")?;
                self.expect_op("(")?;
                self.expr()?;
                self.expect_op(")")?;
                let _ = self.eat_kw("STORED") || self.eat_kw("VIRTUAL");
                col.generated = true;
            } else {
                return Ok(col);
            }
        }
    }

    fn on_conflict(&mut self) -> Result<OnConflict> {
        if !self.eat_kw("OR") {
            return Ok(OnConflict::Abort);
        }
        match self.advance() {
            Tok::Word(w) => match w.to_ascii_uppercase().as_str() {
                "REPLACE" => Ok(OnConflict::Replace),
                "IGNORE" => Ok(OnConflict::Ignore),
                "ABORT" | "FAIL" | "ROLLBACK" => Ok(OnConflict::Abort),
                _ => Err(self.unexpected()),
            },
            _ => Err(self.unexpected()),
        }
    }

    fn insert(&mut self) -> Result<Stmt> {
        let on_conflict = if self.eat_kw("REPLACE") {
            OnConflict::Replace
        } else {
            self.expect_kw("INSERT")?;
            self.on_conflict()?
        };
        self.expect_kw("INTO")?;
        let table = self.qualified_name()?;
        if self.eat_kw("AS") {
            self.ident()?;
        }
        let mut columns = Vec::new();
        if self.eat_op("(") {
            loop {
                columns.push(self.ident()?);
                if !self.eat_op(",") {
                    break;
                }
            }
            self.expect_op(")")?;
        }
        let source = if self.eat_kw("DEFAULT") {
            self.expect_kw("VALUES")?;
            InsertSource::DefaultValues
        } else {
            InsertSource::Select(Box::new(self.select()?))
        };
        if self.is_kw("ON") {
            return self.unsupported("upsert (ON CONFLICT)");
        }
        if self.is_kw("RETURNING") {
            return self.unsupported("RETURNING");
        }
        Ok(Stmt::Insert(Insert {
            table,
            columns,
            source,
            on_conflict,
        }))
    }

    fn update(&mut self) -> Result<Stmt> {
        let on_conflict = self.on_conflict()?;
        let table = self.qualified_name()?;
        self.expect_kw("SET")?;
        let mut sets = Vec::new();
        loop {
            let col = self.ident()?;
            self.expect_op("=")?;
            sets.push((col, self.expr()?));
            if !self.eat_op(",") {
                break;
            }
        }
        if self.is_kw("FROM") {
            return self.unsupported("UPDATE ... FROM");
        }
        let where_ = if self.eat_kw("WHERE") {
            Some(self.expr()?)
        } else {
            None
        };
        if self.is_kw("RETURNING") {
            return self.unsupported("RETURNING");
        }
        Ok(Stmt::Update(Update {
            table,
            sets,
            where_,
            on_conflict,
        }))
    }

    // ---------------------------------------------------------------- SELECT

    pub fn select(&mut self) -> Result<Select> {
        self.depth += 1;
        if self.depth > 200 {
            return Err(Error::Parse("statement is nested too deeply".into()));
        }
        let mut with = Vec::new();
        if self.eat_kw("WITH") {
            let recursive = self.eat_kw("RECURSIVE");
            loop {
                let name = self.ident()?;
                let mut columns = Vec::new();
                if self.eat_op("(") {
                    loop {
                        columns.push(self.ident()?);
                        if !self.eat_op(",") {
                            break;
                        }
                    }
                    self.expect_op(")")?;
                }
                self.expect_kw("AS")?;
                if self.eat_kw("NOT") {
                    self.expect_kw("MATERIALIZED")?;
                } else {
                    self.eat_kw("MATERIALIZED");
                }
                self.expect_op("(")?;
                let select = self.select()?;
                self.expect_op(")")?;
                with.push(Cte {
                    name,
                    columns,
                    select,
                    recursive,
                });
                if !self.eat_op(",") {
                    break;
                }
            }
        }
        let mut body = self.select_term()?;
        loop {
            let op = if self.eat_kw("UNION") {
                if self.eat_kw("ALL") {
                    CompoundOp::UnionAll
                } else {
                    CompoundOp::Union
                }
            } else if self.eat_kw("INTERSECT") {
                CompoundOp::Intersect
            } else if self.eat_kw("EXCEPT") {
                CompoundOp::Except
            } else {
                break;
            };
            let right = self.select_term()?;
            body = SelectBody::Compound {
                op,
                left: Box::new(body),
                right: Box::new(right),
            };
        }
        let mut order_by = Vec::new();
        if self.eat_kw("ORDER") {
            self.expect_kw("BY")?;
            loop {
                let expr = self.expr()?;
                let desc = if self.eat_kw("DESC") {
                    true
                } else {
                    self.eat_kw("ASC");
                    false
                };
                let nulls_first = if self.eat_kw("NULLS") {
                    if self.eat_kw("FIRST") {
                        Some(true)
                    } else {
                        self.expect_kw("LAST")?;
                        Some(false)
                    }
                } else {
                    None
                };
                order_by.push(OrderTerm {
                    expr,
                    desc,
                    nulls_first,
                });
                if !self.eat_op(",") {
                    break;
                }
            }
        }
        let (mut limit, mut offset) = (None, None);
        if self.eat_kw("LIMIT") {
            let first = self.expr()?;
            if self.eat_kw("OFFSET") {
                offset = Some(self.expr()?);
                limit = Some(first);
            } else if self.eat_op(",") {
                limit = Some(self.expr()?);
                offset = Some(first);
            } else {
                limit = Some(first);
            }
        }
        self.depth -= 1;
        Ok(Select {
            with,
            body,
            order_by,
            limit,
            offset,
        })
    }

    fn select_term(&mut self) -> Result<SelectBody> {
        if self.eat_kw("VALUES") {
            let mut rows = Vec::new();
            loop {
                self.expect_op("(")?;
                let mut row = Vec::new();
                loop {
                    row.push(self.expr()?);
                    if !self.eat_op(",") {
                        break;
                    }
                }
                self.expect_op(")")?;
                rows.push(row);
                if !self.eat_op(",") {
                    break;
                }
            }
            return Ok(SelectBody::Values(rows));
        }
        self.expect_kw("SELECT")?;
        let distinct = if self.eat_kw("DISTINCT") {
            true
        } else {
            self.eat_kw("ALL");
            false
        };
        let mut columns = Vec::new();
        loop {
            if self.eat_op("*") {
                columns.push(ResultColumn::Star);
            } else if matches!(self.peek(), Tok::Word(_) | Tok::Quoted { .. })
                && matches!(self.peek_at(1), Tok::Op("."))
                && matches!(self.peek_at(2), Tok::Op("*"))
            {
                let t = self.ident()?;
                self.pos += 2;
                columns.push(ResultColumn::TableStar(t));
            } else {
                let start = self.toks[self.pos].start;
                let expr = self.expr()?;
                let text = self.sql[start..self.toks[self.pos - 1].end].to_string();
                let alias = self.opt_alias()?;
                columns.push(ResultColumn::Expr { expr, alias, text });
            }
            if !self.eat_op(",") {
                break;
            }
        }
        let from = if self.eat_kw("FROM") {
            self.parse_from_list()?
        } else {
            Vec::new()
        };
        let where_ = if self.eat_kw("WHERE") {
            Some(self.expr()?)
        } else {
            None
        };
        let mut group_by = Vec::new();
        if self.eat_kw("GROUP") {
            self.expect_kw("BY")?;
            loop {
                group_by.push(self.expr()?);
                if !self.eat_op(",") {
                    break;
                }
            }
        }
        let having = if self.eat_kw("HAVING") {
            Some(self.expr()?)
        } else {
            None
        };
        if self.is_kw("WINDOW") {
            return self.unsupported("window functions");
        }
        Ok(SelectBody::Core(Box::new(SelectCore {
            distinct,
            columns,
            from,
            where_,
            group_by,
            having,
        })))
    }

    fn parse_from_list(&mut self) -> Result<Vec<FromItem>> {
        let mut items = vec![self.parse_from_item(JoinKind::Inner, false)?];
        loop {
            if self.eat_op(",") {
                items.push(self.parse_from_item(JoinKind::Inner, false)?);
                continue;
            }
            let save = self.pos;
            let natural = self.eat_kw("NATURAL");
            let kind = if self.eat_kw("LEFT") {
                self.eat_kw("OUTER");
                JoinKind::Left
            } else if self.eat_kw("RIGHT") {
                self.eat_kw("OUTER");
                JoinKind::Right
            } else if self.eat_kw("FULL") {
                self.eat_kw("OUTER");
                JoinKind::Full
            } else {
                let _ = self.eat_kw("INNER") || self.eat_kw("CROSS");
                JoinKind::Inner
            };
            if !self.eat_kw("JOIN") {
                self.pos = save;
                return Ok(items);
            }
            let mut item = self.parse_from_item(kind, natural)?;
            if self.eat_kw("ON") {
                item.on = Some(self.expr()?);
            } else if self.eat_kw("USING") {
                self.expect_op("(")?;
                loop {
                    item.using.push(self.ident()?);
                    if !self.eat_op(",") {
                        break;
                    }
                }
                self.expect_op(")")?;
            }
            items.push(item);
        }
    }

    fn parse_from_item(&mut self, join: JoinKind, natural: bool) -> Result<FromItem> {
        let source = if self.eat_op("(") {
            let s = if self.is_kw("SELECT") || self.is_kw("VALUES") || self.is_kw("WITH") {
                TableSource::Subquery(Box::new(self.select()?))
            } else {
                TableSource::Nested(self.parse_from_list()?)
            };
            self.expect_op(")")?;
            s
        } else {
            let first = self.ident()?;
            let (schema, name) = if self.eat_op(".") {
                (Some(first), self.ident()?)
            } else {
                (None, first)
            };
            if self.is_op("(") {
                return self.unsupported("table-valued functions");
            }
            TableSource::Table { schema, name }
        };
        let alias = self.opt_alias()?;
        let mut indexed = Indexed::Any;
        if self.eat_kw("INDEXED") {
            self.expect_kw("BY")?;
            indexed = Indexed::By(self.ident()?);
        } else if self.is_kw("NOT") && self.is_kw_at(1, "INDEXED") {
            self.pos += 2;
            indexed = Indexed::No;
        }
        Ok(FromItem {
            join,
            natural,
            source,
            alias,
            on: None,
            using: Vec::new(),
            indexed,
        })
    }

    // ---------------------------------------------------------------- expressions

    pub fn expr(&mut self) -> Result<Expr> {
        self.expr_bp(0)
    }

    /// Precedence of the infix operator at the cursor, if there is one.
    fn infix_prec(&self) -> Option<u8> {
        Some(match self.peek() {
            Tok::Op(op) => match *op {
                "=" | "==" | "!=" | "<>" => 4,
                "<" | "<=" | ">" | ">=" => 5,
                "&" | "|" | "<<" | ">>" => 7,
                "+" | "-" => 8,
                "*" | "/" | "%" => 9,
                "||" => 10,
                "->" | "->>" => 10,
                _ => return None,
            },
            Tok::Word(w) => match w.to_ascii_uppercase().as_str() {
                "OR" => 1,
                "AND" => 2,
                "IS" | "IN" | "LIKE" | "GLOB" | "MATCH" | "REGEXP" | "BETWEEN" | "ISNULL"
                | "NOTNULL" => 4,
                "NOT" => match self.peek_at(1) {
                    Tok::Word(n)
                        if ["IN", "LIKE", "GLOB", "MATCH", "REGEXP", "BETWEEN", "NULL"]
                            .iter()
                            .any(|k| n.eq_ignore_ascii_case(k)) =>
                    {
                        4
                    }
                    _ => return None,
                },
                "COLLATE" => 11,
                _ => return None,
            },
            _ => return None,
        })
    }

    fn expr_bp(&mut self, min: u8) -> Result<Expr> {
        self.depth += 1;
        if self.depth > 200 {
            return Err(Error::Parse("expression is nested too deeply".into()));
        }
        let mut lhs = self.prefix()?;
        while let Some(prec) = self.infix_prec() {
            if prec < min {
                break;
            }
            lhs = self.infix(lhs, prec)?;
        }
        self.depth -= 1;
        Ok(lhs)
    }

    fn infix(&mut self, lhs: Expr, prec: u8) -> Result<Expr> {
        let tok = self.advance();
        let lhs = Box::new(lhs);
        if let Tok::Op(op) = tok {
            let bop = match op {
                "=" | "==" => BinaryOp::Eq,
                "!=" | "<>" => BinaryOp::Ne,
                "<" => BinaryOp::Lt,
                "<=" => BinaryOp::Le,
                ">" => BinaryOp::Gt,
                ">=" => BinaryOp::Ge,
                "&" => BinaryOp::BitAnd,
                "|" => BinaryOp::BitOr,
                "<<" => BinaryOp::Shl,
                ">>" => BinaryOp::Shr,
                "+" => BinaryOp::Add,
                "-" => BinaryOp::Sub,
                "*" => BinaryOp::Mul,
                "/" => BinaryOp::Div,
                "%" => BinaryOp::Rem,
                "||" => BinaryOp::Concat,
                _ => return self.unsupported("JSON operators"),
            };
            let rhs = self.expr_bp(prec + 1)?;
            return Ok(Expr::Binary(bop, lhs, Box::new(rhs)));
        }
        let Tok::Word(word) = tok else { unreachable!() };
        let mut kw = word.to_ascii_uppercase();
        let mut negated = false;
        if kw == "NOT" {
            negated = true;
            kw = match self.advance() {
                Tok::Word(w) => w.to_ascii_uppercase(),
                _ => unreachable!(),
            };
        }
        match kw.as_str() {
            "OR" => Ok(Expr::Binary(
                BinaryOp::Or,
                lhs,
                Box::new(self.expr_bp(prec + 1)?),
            )),
            "AND" => Ok(Expr::Binary(
                BinaryOp::And,
                lhs,
                Box::new(self.expr_bp(prec + 1)?),
            )),
            "COLLATE" => Ok(Expr::Collate {
                expr: lhs,
                collation: self.ident()?,
            }),
            "ISNULL" => Ok(Expr::IsNull {
                expr: lhs,
                negated: false,
            }),
            "NOTNULL" | "NULL" => Ok(Expr::IsNull {
                expr: lhs,
                negated: true,
            }),
            "IS" => {
                let mut not = self.eat_kw("NOT");
                if self.is_kw("DISTINCT") && self.is_kw_at(1, "FROM") {
                    self.pos += 2;
                    not = !not;
                }
                let rhs = self.expr_bp(prec + 1)?;
                if rhs == Expr::Literal(Value::Null) {
                    return Ok(Expr::IsNull {
                        expr: lhs,
                        negated: not,
                    });
                }
                let op = if not { BinaryOp::IsNot } else { BinaryOp::Is };
                Ok(Expr::Binary(op, lhs, Box::new(rhs)))
            }
            "BETWEEN" => {
                let lo = self.expr_bp(prec + 1)?;
                self.expect_kw("AND")?;
                let hi = self.expr_bp(prec + 1)?;
                Ok(Expr::Between {
                    expr: lhs,
                    lo: Box::new(lo),
                    hi: Box::new(hi),
                    negated,
                })
            }
            "IN" => {
                if self.eat_op("(") {
                    if self.is_kw("SELECT") || self.is_kw("VALUES") || self.is_kw("WITH") {
                        let select = Box::new(self.select()?);
                        self.expect_op(")")?;
                        return Ok(Expr::InSelect {
                            expr: lhs,
                            select,
                            negated,
                        });
                    }
                    let mut list = Vec::new();
                    if !self.is_op(")") {
                        loop {
                            list.push(self.expr()?);
                            if !self.eat_op(",") {
                                break;
                            }
                        }
                    }
                    self.expect_op(")")?;
                    return Ok(Expr::InList {
                        expr: lhs,
                        list,
                        negated,
                    });
                }
                let table = self.qualified_name()?;
                Ok(Expr::InTable {
                    expr: lhs,
                    table,
                    negated,
                })
            }
            "LIKE" | "GLOB" | "MATCH" | "REGEXP" => {
                let op = match kw.as_str() {
                    "LIKE" => LikeOp::Like,
                    "GLOB" => LikeOp::Glob,
                    "MATCH" => LikeOp::Match,
                    _ => LikeOp::Regexp,
                };
                let pattern = Box::new(self.expr_bp(prec + 1)?);
                let escape = if self.eat_kw("ESCAPE") {
                    Some(Box::new(self.expr_bp(prec + 1)?))
                } else {
                    None
                };
                Ok(Expr::Like {
                    op,
                    expr: lhs,
                    pattern,
                    escape,
                    negated,
                })
            }
            _ => unreachable!(),
        }
    }

    fn number(text: &str) -> Value {
        if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            return Value::Int(u64::from_str_radix(hex, 16).unwrap_or(0) as i64);
        }
        if !text.contains(['.', 'e', 'E']) {
            if let Ok(i) = text.parse::<i64>() {
                return Value::Int(i);
            }
        }
        Value::Real(text.parse::<f64>().unwrap_or(0.0))
    }

    fn prefix(&mut self) -> Result<Expr> {
        let tok = self.advance();
        match tok {
            Tok::Num(n) => Ok(Expr::Literal(Self::number(&n))),
            Tok::Str(s) => Ok(Expr::Literal(Value::text(s))),
            Tok::Blob(b) => Ok(Expr::Literal(Value::blob(b))),
            Tok::Param => Ok(Expr::Param),
            Tok::Op("(") => {
                if self.is_kw("SELECT") || self.is_kw("VALUES") || self.is_kw("WITH") {
                    let s = self.select()?;
                    self.expect_op(")")?;
                    return Ok(Expr::Subquery(Box::new(s)));
                }
                let e = self.expr()?;
                if self.is_op(",") {
                    return self.unsupported("row values");
                }
                self.expect_op(")")?;
                Ok(e)
            }
            Tok::Op("-") => {
                // Fold the sign into numeric literals so that
                // -9223372036854775808 stays an integer.
                if let Tok::Num(n) = self.peek() {
                    if n == "9223372036854775808" {
                        self.pos += 1;
                        return Ok(Expr::Literal(Value::Int(i64::MIN)));
                    }
                }
                let e = self.expr_bp(12)?;
                Ok(match e {
                    Expr::Literal(Value::Int(i)) if i != i64::MIN => Expr::Literal(Value::Int(-i)),
                    Expr::Literal(Value::Real(r)) => Expr::Literal(Value::Real(-r)),
                    e => Expr::Unary(UnaryOp::Neg, Box::new(e)),
                })
            }
            Tok::Op("+") => Ok(Expr::Unary(UnaryOp::Pos, Box::new(self.expr_bp(12)?))),
            Tok::Op("~") => Ok(Expr::Unary(UnaryOp::BitNot, Box::new(self.expr_bp(12)?))),
            Tok::Quoted { name, dq } => self.name_tail(name, dq),
            Tok::Word(w) => {
                let up = w.to_ascii_uppercase();
                match up.as_str() {
                    "NOT" => {
                        if self.is_kw("EXISTS") {
                            self.pos += 1;
                            let e = self.exists()?;
                            return Ok(Expr::Unary(UnaryOp::Not, Box::new(e)));
                        }
                        return Ok(Expr::Unary(UnaryOp::Not, Box::new(self.expr_bp(4)?)));
                    }
                    "EXISTS" => return self.exists(),
                    "NULL" => return Ok(Expr::Literal(Value::Null)),
                    "CASE" => return self.case(),
                    "CAST" => {
                        self.expect_op("(")?;
                        let expr = Box::new(self.expr()?);
                        self.expect_kw("AS")?;
                        let type_name = self.type_name()?;
                        self.expect_op(")")?;
                        return Ok(Expr::Cast { expr, type_name });
                    }
                    "TRUE" if !self.is_op("(") && !self.is_op(".") => {
                        return Ok(Expr::Literal(Value::Int(1)))
                    }
                    "FALSE" if !self.is_op("(") && !self.is_op(".") => {
                        return Ok(Expr::Literal(Value::Int(0)))
                    }
                    "CURRENT_DATE" | "CURRENT_TIME" | "CURRENT_TIMESTAMP" if !self.is_op("(") => {
                        return Ok(Expr::Function {
                            name: up.to_ascii_lowercase(),
                            args: Vec::new(),
                            distinct: false,
                            star: false,
                            filter: None,
                            over: false,
                        });
                    }
                    _ => {}
                }
                if self.is_op("(") {
                    return self.function(w);
                }
                if is_reserved(&w) {
                    self.pos -= 1;
                    return Err(self.unexpected());
                }
                self.name_tail(w, false)
            }
            _ => {
                self.pos -= 1;
                Err(self.unexpected())
            }
        }
    }

    fn name_tail(&mut self, first: String, dq: bool) -> Result<Expr> {
        if !self.eat_op(".") {
            return Ok(Expr::Name {
                table: None,
                name: first,
                dq,
            });
        }
        let second = self.ident()?;
        if self.eat_op(".") {
            // schema.table.column
            let third = self.ident()?;
            return Ok(Expr::Name {
                table: Some(second),
                name: third,
                dq: false,
            });
        }
        Ok(Expr::Name {
            table: Some(first),
            name: second,
            dq: false,
        })
    }

    fn exists(&mut self) -> Result<Expr> {
        self.expect_op("(")?;
        let s = self.select()?;
        self.expect_op(")")?;
        Ok(Expr::Exists(Box::new(s)))
    }

    fn case(&mut self) -> Result<Expr> {
        let operand = if self.is_kw("WHEN") {
            None
        } else {
            Some(Box::new(self.expr()?))
        };
        let mut whens = Vec::new();
        while self.eat_kw("WHEN") {
            let w = self.expr()?;
            self.expect_kw("THEN")?;
            whens.push((w, self.expr()?));
        }
        if whens.is_empty() {
            return Err(self.unexpected());
        }
        let else_ = if self.eat_kw("ELSE") {
            Some(Box::new(self.expr()?))
        } else {
            None
        };
        self.expect_kw("END")?;
        Ok(Expr::Case {
            operand,
            whens,
            else_,
        })
    }

    fn function(&mut self, name: String) -> Result<Expr> {
        self.expect_op("(")?;
        let mut args = Vec::new();
        let mut distinct = false;
        let mut star = false;
        if self.eat_op("*") {
            star = true;
        } else if !self.is_op(")") {
            if self.eat_kw("DISTINCT") {
                distinct = true;
            } else {
                self.eat_kw("ALL");
            }
            loop {
                args.push(self.expr()?);
                if !self.eat_op(",") {
                    break;
                }
            }
            if self.is_kw("ORDER") {
                return self.unsupported("ORDER BY inside an aggregate");
            }
        }
        self.expect_op(")")?;
        let mut filter = None;
        if self.is_kw("FILTER") && matches!(self.peek_at(1), Tok::Op("(")) {
            self.pos += 2;
            self.expect_kw("WHERE")?;
            filter = Some(Box::new(self.expr()?));
            self.expect_op(")")?;
        }
        let over = self.is_kw("OVER");
        if over {
            return self.unsupported("window functions");
        }
        Ok(Expr::Function {
            name: name.to_ascii_lowercase(),
            args,
            distinct,
            star,
            filter,
            over,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(sql: &str) -> Expr {
        parse_expr(sql).unwrap()
    }

    #[test]
    fn precedence() {
        assert_eq!(e("1 + 2 * 3"), e("1 + (2 * 3)"));
        assert_eq!(e("NOT a = b"), e("NOT (a = b)"));
        assert_eq!(e("a OR b AND c"), e("a OR (b AND c)"));
        assert_eq!(e("- a * b"), e("(-a) * b"));
        assert_eq!(e("a || b COLLATE nocase"), e("a || (b COLLATE nocase)"));
        assert_eq!(e("a = b < c"), e("a = (b < c)"));
        assert_eq!(e("a BETWEEN 1 AND 2 AND c"), e("(a BETWEEN 1 AND 2) AND c"));
        assert_eq!(e("a NOT IN (1,2) OR b"), e("(a NOT IN (1,2)) OR b"));
        assert_eq!(
            e("-9223372036854775808"),
            Expr::Literal(Value::Int(i64::MIN))
        );
        assert_eq!(e("a IS NOT NULL"), e("a NOTNULL"));
    }

    #[test]
    fn statements() {
        let v = parse_statements(
            "SELECT DISTINCT a AS x, t.*, count(*) FROM t LEFT JOIN u USING (id), (SELECT 1) s \
             WHERE a IN (SELECT b FROM u) GROUP BY 1 HAVING count(*) > 1 ORDER BY x DESC LIMIT 5 OFFSET 2; \
             CREATE TABLE IF NOT EXISTS t(a INTEGER PRIMARY KEY, b VARCHAR(10) NOT NULL DEFAULT 'x', \
               c REAL UNIQUE, CONSTRAINT k UNIQUE (b, c), FOREIGN KEY (b) REFERENCES u(id) ON DELETE CASCADE) WITHOUT ROWID; \
             CREATE UNIQUE INDEX i ON t(b COLLATE nocase DESC, c); \
             INSERT INTO t(a,b) VALUES (1,'x'),(2,'y'); UPDATE t SET b = b || 'z' WHERE a = 1; DELETE FROM t; \
             WITH c(n) AS (SELECT 1 UNION ALL SELECT 2) SELECT * FROM c; BEGIN; COMMIT; PRAGMA page_size = 1024;",
        )
        .unwrap();
        assert_eq!(v.len(), 10);
        match &v[1].0 {
            Stmt::CreateTable(ct) => {
                assert_eq!(ct.columns.len(), 3);
                assert_eq!(ct.columns[1].type_name, "VARCHAR(10)");
                assert!(ct.columns[1].not_null && ct.columns[2].unique && ct.without_rowid);
                assert_eq!(ct.constraints.len(), 2);
            }
            other => panic!("{other:?}"),
        }
        assert!(v[2].1.starts_with("CREATE UNIQUE INDEX"));
    }

    #[test]
    fn errors() {
        assert!(parse_one("SELECT FROM").is_err());
        assert!(parse_one("SELECT 1 +").is_err());
        assert!(parse_one("SELEC 1").is_err());
        assert!(matches!(
            parse_one("SELECT row_number() OVER () FROM t"),
            Err(Error::Unsupported(_))
        ));
    }
}
