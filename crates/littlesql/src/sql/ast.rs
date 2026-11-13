//! The abstract syntax tree produced by the parser.

use crate::value::Value;

#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Select(Select),
    Explain {
        query_plan: bool,
        stmt: Box<Stmt>,
    },
    CreateTable(CreateTable),
    CreateIndex(CreateIndex),
    CreateView(CreateView),
    Drop {
        kind: DropKind,
        if_exists: bool,
        name: String,
    },
    Insert(Insert),
    Update(Update),
    Delete(Delete),
    Begin,
    Commit,
    Rollback,
    Pragma {
        name: String,
        value: Option<String>,
    },
    /// Statements that are accepted and do nothing (ANALYZE, REINDEX,
    /// VACUUM).
    Noop(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DropKind {
    Table,
    Index,
    View,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Select {
    pub with: Vec<Cte>,
    pub body: SelectBody,
    pub order_by: Vec<OrderTerm>,
    pub limit: Option<Expr>,
    pub offset: Option<Expr>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cte {
    pub name: String,
    pub columns: Vec<String>,
    pub select: Select,
    pub recursive: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SelectBody {
    Core(Box<SelectCore>),
    Values(Vec<Vec<Expr>>),
    Compound {
        op: CompoundOp,
        left: Box<SelectBody>,
        right: Box<SelectBody>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompoundOp {
    Union,
    UnionAll,
    Intersect,
    Except,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SelectCore {
    pub distinct: bool,
    pub columns: Vec<ResultColumn>,
    pub from: Vec<FromItem>,
    pub where_: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub having: Option<Expr>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ResultColumn {
    Star,
    TableStar(String),
    Expr {
        expr: Expr,
        alias: Option<String>,
        /// The expression exactly as written, used as the column name when
        /// there is no alias.
        text: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinKind {
    /// The first item, a comma, `JOIN`, `INNER JOIN` or `CROSS JOIN`.
    Inner,
    Left,
    Right,
    Full,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FromItem {
    pub join: JoinKind,
    pub natural: bool,
    pub source: TableSource,
    pub alias: Option<String>,
    pub on: Option<Expr>,
    pub using: Vec<String>,
    pub indexed: Indexed,
}

/// `INDEXED BY name` / `NOT INDEXED` after a table in FROM.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Indexed {
    #[default]
    Any,
    /// Use no index (the rowid is still allowed).
    No,
    /// Use exactly this index.
    By(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum TableSource {
    Table {
        schema: Option<String>,
        name: String,
    },
    Subquery(Box<Select>),
    /// A parenthesised join: `FROM a JOIN (b JOIN c ON ...) ON ...`.
    Nested(Vec<FromItem>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct OrderTerm {
    pub expr: Expr,
    pub desc: bool,
    pub nulls_first: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Pos,
    Not,
    BitNot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Concat,
    BitAnd,
    BitOr,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    /// `IS` / `IS NOT DISTINCT FROM`: equality where NULL equals NULL.
    Is,
    IsNot,
}

impl BinaryOp {
    pub fn is_comparison(self) -> bool {
        use BinaryOp::*;
        matches!(self, Eq | Ne | Lt | Le | Gt | Ge | Is | IsNot)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LikeOp {
    Like,
    Glob,
    Regexp,
    Match,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Literal(Value),
    /// A column reference, optionally qualified. `dq` marks a bare
    /// double-quoted name, which may turn out to be a string literal.
    Name {
        table: Option<String>,
        name: String,
        dq: bool,
    },
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    IsNull {
        expr: Box<Expr>,
        negated: bool,
    },
    Between {
        expr: Box<Expr>,
        lo: Box<Expr>,
        hi: Box<Expr>,
        negated: bool,
    },
    InList {
        expr: Box<Expr>,
        list: Vec<Expr>,
        negated: bool,
    },
    InSelect {
        expr: Box<Expr>,
        select: Box<Select>,
        negated: bool,
    },
    /// `x IN tablename`
    InTable {
        expr: Box<Expr>,
        table: String,
        negated: bool,
    },
    Like {
        op: LikeOp,
        expr: Box<Expr>,
        pattern: Box<Expr>,
        escape: Option<Box<Expr>>,
        negated: bool,
    },
    Case {
        operand: Option<Box<Expr>>,
        whens: Vec<(Expr, Expr)>,
        else_: Option<Box<Expr>>,
    },
    Cast {
        expr: Box<Expr>,
        type_name: String,
    },
    Collate {
        expr: Box<Expr>,
        collation: String,
    },
    Function {
        name: String,
        args: Vec<Expr>,
        distinct: bool,
        star: bool,
        filter: Option<Box<Expr>>,
        over: bool,
    },
    Subquery(Box<Select>),
    Exists(Box<Select>),
    Param,
}

impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        self.type_name() == other.type_name()
            && self.compare(other, crate::value::Collation::Binary) == std::cmp::Ordering::Equal
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnDef {
    pub name: String,
    pub type_name: String,
    pub primary_key: bool,
    pub pk_desc: bool,
    pub autoincrement: bool,
    pub not_null: bool,
    pub unique: bool,
    pub default: Option<Expr>,
    pub collation: Option<String>,
    pub generated: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IndexedColumn {
    /// Column name, or `None` when the index key is an expression.
    pub name: Option<String>,
    pub collation: Option<String>,
    pub desc: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TableConstraint {
    PrimaryKey(Vec<IndexedColumn>),
    Unique(Vec<IndexedColumn>),
    /// CHECK and FOREIGN KEY constraints are parsed and not enforced.
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CreateTable {
    pub name: String,
    pub temp: bool,
    pub if_not_exists: bool,
    pub columns: Vec<ColumnDef>,
    pub constraints: Vec<TableConstraint>,
    pub without_rowid: bool,
    pub as_select: Option<Box<Select>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CreateIndex {
    pub name: String,
    pub table: String,
    pub unique: bool,
    pub if_not_exists: bool,
    pub columns: Vec<IndexedColumn>,
    pub partial: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CreateView {
    pub name: String,
    pub temp: bool,
    pub if_not_exists: bool,
    pub columns: Vec<String>,
    pub select: Select,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnConflict {
    Abort,
    Ignore,
    Replace,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InsertSource {
    Select(Box<Select>),
    DefaultValues,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Insert {
    pub table: String,
    pub columns: Vec<String>,
    pub source: InsertSource,
    pub on_conflict: OnConflict,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub table: String,
    pub sets: Vec<(String, Expr)>,
    pub where_: Option<Expr>,
    pub on_conflict: OnConflict,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Delete {
    pub table: String,
    pub where_: Option<Expr>,
}
