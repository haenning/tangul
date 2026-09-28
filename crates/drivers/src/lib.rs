//! Database drivers. Everything uses the text protocol: every value arrives as a string,
//! so any server type renders without type-mapping code.

#[cfg(feature = "mysql")]
mod mysql;
#[cfg(feature = "postgres")]
mod postgres;
#[cfg(feature = "sqlite")]
mod sqlite;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tangul_core::{
    CatalogExtras, ColumnInfo, ColumnMeta, ConnectConfig, DriverKind, ForeignKey, RowBatch,
    TableInfo,
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

    /// Starts a transaction the server keeps read-only whatever the session's
    /// setting; the caller ends it with `ROLLBACK`.
    async fn begin_read_only(&mut self) -> Result<()> {
        self.execute("START TRANSACTION READ ONLY", 0, &mut |_| {})
            .await
    }

    /// Makes the server stop statements that run longer than `limit` on this session
    /// (`None`: no limit).
    async fn set_statement_timeout(&mut self, limit: Option<std::time::Duration>) -> Result<()>;

    /// Handle that can cancel the running statement from another task.
    fn cancel_handle(&self) -> Arc<dyn CancelHandle>;

    /// Checks one statement on the server without running it (prepare only): syntax
    /// and whether referenced objects exist. Errors carry a [`SqlError`] position.
    async fn validate(&mut self, sql: &str) -> Result<()>;

    async fn list_tables(&mut self) -> Result<Vec<TableInfo>>;

    /// Columns of all listed tables, ordered by schema, table, position.
    async fn list_columns(&mut self) -> Result<Vec<ColumnMeta>>;

    async fn list_foreign_keys(&mut self) -> Result<Vec<ForeignKey>>;

    /// Indexes, check constraints, enums, view definitions and routines. A part the
    /// server can't answer (an older version, missing rights) stays empty.
    async fn catalog_extras(&mut self) -> Result<CatalogExtras> {
        Ok(CatalogExtras::default())
    }

    /// False once the server connection is gone; broken sessions are not reused.
    fn is_alive(&self) -> bool;

    async fn close(self: Box<Self>);
}

/// A part of [`Session::catalog_extras`]: empty (and logged) if the server couldn't
/// answer, so one missing part doesn't cost the others.
pub(crate) fn catalog_part<T>(what: &str, result: Result<Vec<T>>) -> Vec<T> {
    result.unwrap_or_else(|e| {
        eprintln!("could not read {what}: {e:#}");
        Vec::new()
    })
}

#[async_trait]
pub trait CancelHandle: Send + Sync {
    async fn cancel(&self) -> Result<()>;
}

/// The driver for `kind`; `None` when this build left it out (see the crate's
/// features).
pub fn driver_for(kind: DriverKind) -> Option<Arc<dyn Driver>> {
    match kind {
        #[cfg(feature = "postgres")]
        DriverKind::Postgres => Some(Arc::new(postgres::PgDriver)),
        #[cfg(feature = "mysql")]
        DriverKind::Mysql => Some(Arc::new(mysql::MysqlDriver)),
        #[cfg(feature = "sqlite")]
        DriverKind::Sqlite => Some(Arc::new(sqlite::SqliteDriver)),
        #[allow(unreachable_patterns)]
        _ => None,
    }
}
