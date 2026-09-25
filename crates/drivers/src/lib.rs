//! Database drivers. Everything uses the text protocol: every value arrives as a string,
//! so any server type renders without type-mapping code.

mod mysql;
mod postgres;
mod sqlite;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use rusdb_core::{
    ColumnInfo, ColumnMeta, ConnectConfig, DriverKind, ForeignKey, RowBatch, TableInfo,
};

/// Rows are forwarded to the UI in chunks of this size.
pub const BATCH_ROWS: usize = 1000;

/// Events produced while executing SQL. A single execution can contain several
/// statements and therefore several result sets.
#[derive(Debug)]
pub enum ExecEvent {
    /// A new result set starts; following `Rows` belong to it.
    ResultStart(Vec<ColumnInfo>),
    Rows(RowBatch),
    /// A statement finished. `rows` = affected or returned rows as reported by the server.
    StatementDone {
        rows: u64,
    },
    /// The row limit was hit; remaining rows were discarded.
    Truncated,
}

pub type Sink<'a> = &'a mut (dyn FnMut(ExecEvent) + Send);

/// An error the server reported for a statement, with where it happened (if known).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlError {
    pub message: String,
    /// Byte offset into the SQL passed to `Session::execute`.
    pub position: Option<usize>,
}

impl std::fmt::Display for SqlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SqlError {}

/// Byte offset of a 1-based character position (Postgres reports characters).
pub(crate) fn char_to_byte(sql: &str, position: usize) -> Option<usize> {
    let index = position.checked_sub(1)?;
    sql.char_indices()
        .nth(index)
        .map(|(i, _)| i)
        .or_else(|| (index == sql.chars().count()).then_some(sql.len()))
}

/// For errors without a position that name an object in quotes (MySQL's
/// `Table 'app.x' doesn't exist`, `Unknown column 'u.nme' in 'field list'`): where
/// the last part of the first quoted name appears in `sql`.
pub(crate) fn quoted_name_position(message: &str, sql: &str) -> Option<usize> {
    let start = message.find('\'')? + 1;
    let end = start + message[start..].find('\'')?;
    let name = message[start..end].rsplit('.').next()?;
    if name.is_empty() {
        return None;
    }
    let lower = sql.to_lowercase();
    let needle = name.to_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(&needle) {
        let at = from + i;
        let before = lower[..at].chars().next_back();
        let after = lower[at + needle.len()..].chars().next();
        let boundary = |c: Option<char>| c.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        if boundary(before) && boundary(after) {
            return Some(at);
        }
        from = at + needle.len();
    }
    None
}

/// Locates MySQL's `… near '<rest of statement>' at line N` in `sql`. The quoted part
/// is the (possibly cut) remainder of the statement from the error on; empty means
/// the statement ended unexpectedly.
pub(crate) fn mysql_near(message: &str, sql: &str) -> Option<usize> {
    let start = message.find("near '")? + "near '".len();
    let end = start + message[start..].rfind("' at line ")?;
    let near = &message[start..end];
    let line: usize = message[end + "' at line ".len()..]
        .trim()
        .trim_end_matches(|c: char| !c.is_ascii_digit())
        .parse()
        .ok()?;
    if near.is_empty() {
        return Some(sql.trim_end().trim_end_matches(';').trim_end().len());
    }
    // The server cuts long snippets; a short prefix is enough to find the spot.
    let probe: String = near.chars().take(24).collect();
    let line_start = sql
        .split_inclusive('\n')
        .take(line.saturating_sub(1))
        .map(str::len)
        .sum::<usize>();
    sql.get(line_start..)
        .and_then(|rest| rest.find(&probe))
        .map(|i| line_start + i)
        .or_else(|| sql.find(&probe))
}

#[async_trait]
pub trait Driver: Send + Sync {
    async fn connect(&self, cfg: &ConnectConfig) -> Result<Box<dyn Session>>;
}

/// One physical server connection.
#[async_trait]
pub trait Session: Send {
    /// Runs `sql` (may contain multiple statements) and streams results into `sink`.
    /// Rows beyond `row_limit` per result set are discarded.
    async fn execute(&mut self, sql: &str, row_limit: usize, sink: Sink<'_>) -> Result<()>;

    /// True while an explicit transaction is open. Valid after `execute` returned.
    /// The engine pins the session to its tab while this is true.
    fn in_transaction(&self) -> bool;

    /// Makes the server refuse writes on this session (`READ ONLY` transactions), or
    /// allow them again. Applies to transactions started afterwards.
    async fn set_read_only(&mut self, on: bool) -> Result<()>;

    /// Handle that can cancel the running statement from another task.
    fn cancel_handle(&self) -> Arc<dyn CancelHandle>;

    /// Checks one statement on the server without running it (prepare only): syntax
    /// and whether referenced objects exist. Errors carry a [`SqlError`] position.
    async fn validate(&mut self, sql: &str) -> Result<()>;

    async fn list_tables(&mut self) -> Result<Vec<TableInfo>>;

    /// Columns of all listed tables, ordered by schema, table, position.
    async fn list_columns(&mut self) -> Result<Vec<ColumnMeta>>;

    async fn list_foreign_keys(&mut self) -> Result<Vec<ForeignKey>>;

    /// False once the server connection is gone; broken sessions are not reused.
    fn is_alive(&self) -> bool;

    async fn close(self: Box<Self>);
}

#[async_trait]
pub trait CancelHandle: Send + Sync {
    async fn cancel(&self) -> Result<()>;
}

pub fn driver_for(kind: DriverKind) -> Arc<dyn Driver> {
    match kind {
        DriverKind::Postgres => Arc::new(postgres::PgDriver),
        DriverKind::Mysql => Arc::new(mysql::MysqlDriver),
        DriverKind::Sqlite => Arc::new(sqlite::SqliteDriver),
    }
}

/// Cheap check whether `sql` may change transaction state. Used to avoid a
/// transaction-status round-trip after ordinary statements.
pub(crate) fn may_touch_transaction(sql: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "begin",
        "start",
        "commit",
        "rollback",
        "end",
        "abort",
        "savepoint",
        "release",
    ];
    sql.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|w| KEYWORDS.iter().any(|k| w.eq_ignore_ascii_case(k)))
}

#[cfg(test)]
mod tests {
    use super::{char_to_byte, may_touch_transaction, mysql_near, quoted_name_position};

    #[test]
    fn error_positions() {
        assert_eq!(char_to_byte("select föo", 8), Some(7));
        // `ö` is two bytes: the final `o` (character 10) starts at byte 10.
        assert_eq!(char_to_byte("select föo", 10), Some(10));
        assert_eq!(char_to_byte("select föo", 11), Some(11));
        assert_eq!(char_to_byte("abc", 4), Some(3));
        assert_eq!(char_to_byte("abc", 0), None);

        let msg = "ERROR 42000 (1064): You have an error in your SQL syntax; check the manual \
                   that corresponds to your MySQL server version for the right syntax to use \
                   near 'form users' at line 2";
        let sql = "select 1;\nselect * form users";
        assert_eq!(mysql_near(msg, sql), Some(sql.find("form").unwrap()));
        let msg = "… to use near '' at line 1";
        assert_eq!(mysql_near(msg, "select * from;"), Some(13));
        assert_eq!(mysql_near("no position here", "x"), None);

        let sql = "select nme from users u where u.idx = 1";
        assert_eq!(
            quoted_name_position("Unknown column 'u.idx' in 'where clause'", sql),
            Some(sql.find("idx").unwrap())
        );
        assert_eq!(
            quoted_name_position("Table 'app.users' doesn't exist", sql),
            Some(sql.find("users").unwrap())
        );
        // Whole words only.
        assert_eq!(
            quoted_name_position("Unknown column 'u' in 'x'", "select users"),
            None
        );
    }

    #[test]
    fn transaction_keywords() {
        assert!(may_touch_transaction("BEGIN"));
        assert!(may_touch_transaction("select 1; commit;"));
        assert!(!may_touch_transaction("select begins_at from t"));
        assert!(!may_touch_transaction("select * from users"));
    }
}
