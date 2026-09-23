//! Database drivers. Everything uses the text protocol: every value arrives as a string,
//! so any server type renders without type-mapping code.

mod mysql;
mod postgres;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use rusdb_core::{ColumnInfo, ConnectConfig, DriverKind, RowBatch, TableInfo};

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

    /// Handle that can cancel the running statement from another task.
    fn cancel_handle(&self) -> Arc<dyn CancelHandle>;

    async fn list_tables(&mut self) -> Result<Vec<TableInfo>>;

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
    use super::may_touch_transaction;

    #[test]
    fn transaction_keywords() {
        assert!(may_touch_transaction("BEGIN"));
        assert!(may_touch_transaction("select 1; commit;"));
        assert!(!may_touch_transaction("select begins_at from t"));
        assert!(!may_touch_transaction("select * from users"));
    }
}
