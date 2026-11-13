//! The engine's single error type.

use std::fmt;

/// Everything that can go wrong, grouped by who is at fault.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The database file violates the SQLite file format.
    Corrupt(String),
    /// The SQL text could not be parsed.
    Parse(String),
    /// The SQL is valid but refers to something that does not exist or is
    /// used incorrectly (unknown table, ambiguous column, wrong argument
    /// count, ...).
    Sql(String),
    /// A feature of SQLite that this engine does not implement.
    Unsupported(String),
    /// A failure while executing (integer overflow in `sum`, ...).
    Runtime(String),
    /// A constraint (NOT NULL, UNIQUE, PRIMARY KEY) rejected a write.
    Constraint(String),
    /// The operating system or storage layer failed.
    Io(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Corrupt(m) => write!(f, "database disk image is malformed: {m}"),
            Error::Parse(m) => write!(f, "parse error: {m}"),
            Error::Sql(m) => write!(f, "{m}"),
            Error::Unsupported(m) => write!(f, "not supported by Little SQL: {m}"),
            Error::Runtime(m) => write!(f, "{m}"),
            Error::Constraint(m) => write!(f, "constraint failed: {m}"),
            Error::Io(m) => write!(f, "I/O error: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

pub(crate) fn corrupt<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Corrupt(msg.into()))
}
