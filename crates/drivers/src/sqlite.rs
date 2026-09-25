//! The app's own SQLite data (bookmark metrics), always read-only: the file is opened
//! read-only and `query_only` is on, so no statement can change it. `database` in the
//! config is the file's path. SQLite is synchronous; statements run on a blocking
//! thread and their events come back over a channel, so a time limit can interrupt.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use rusqlite::fallible_iterator::FallibleIterator as _;
use rusqlite::{Connection, InterruptHandle, OpenFlags, types::ValueRef};
use tangul_core::{
    ColumnInfo, ColumnMeta, ConnectConfig, ForeignKey, RowBatch, TableInfo, TableKind,
};

use crate::{BATCH_ROWS, CancelHandle, Driver, ExecEvent, Session, Sink, SqlError};

/// The only schema of a single SQLite file.
const SCHEMA: &str = "main";
/// Wait this long for the app's own writes to finish.
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);

pub struct SqliteDriver;

#[async_trait]
impl Driver for SqliteDriver {
    async fn connect(&self, cfg: &ConnectConfig) -> Result<Box<dyn Session>> {
        let path = cfg.database.clone();
        let conn = tokio::task::spawn_blocking(move || open(&path)).await??;
        Ok(Box::new(SqliteSession {
            cancel: Arc::new(SqliteCancel(conn.get_interrupt_handle())),
            conn: Arc::new(Mutex::new(conn)),
        }))
    }
}

fn open(path: &str) -> Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags).map_err(|e| anyhow!("{path}: {e}"))?;
    conn.pragma_update(None, "query_only", true)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(conn)
}

struct SqliteSession {
    conn: Arc<Mutex<Connection>>,
    cancel: Arc<SqliteCancel>,
}

impl SqliteSession {
    /// Runs `f` on the connection on a blocking thread.
    async fn with<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || f(&conn.lock().unwrap())).await?
    }
}

fn sql_error(e: rusqlite::Error) -> anyhow::Error {
    SqlError {
        message: e.to_string(),
        position: None,
    }
    .into()
}

/// Cell text in the same form a server would send it.
fn text(value: ValueRef<'_>) -> Option<String> {
    match value {
        ValueRef::Null => None,
        ValueRef::Integer(i) => Some(i.to_string()),
        ValueRef::Real(f) => Some(f.to_string()),
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Some(b.iter().fold(String::from("0x"), |mut s, byte| {
            s.push_str(&format!("{byte:02x}"));
            s
        })),
    }
}

fn run(
    conn: &Connection,
    sql: &str,
    row_limit: usize,
    sink: &mut dyn FnMut(ExecEvent),
) -> Result<()> {
    let mut batch = rusqlite::Batch::new(conn, sql);
    while let Some(mut stmt) = batch.next().map_err(sql_error)? {
        let columns = stmt.column_count();
        if columns > 0 {
            let names = stmt
                .column_names()
                .into_iter()
                .map(|name| ColumnInfo { name: name.into() })
                .collect();
            sink(ExecEvent::ResultStart(names));
        }
        let mut rows = stmt.raw_query();
        let (mut count, mut chunk) = (0u64, RowBatch::with_capacity(BATCH_ROWS * columns));
        while let Some(row) = rows.next().map_err(sql_error)? {
            if count as usize >= row_limit {
                sink(ExecEvent::Truncated);
                break;
            }
            for i in 0..columns {
                let value = row.get_ref(i).map_err(sql_error)?;
                chunk.push_cell(text(value).as_deref());
            }
            chunk.finish_row();
            count += 1;
            if chunk.rows() >= BATCH_ROWS {
                let full =
                    std::mem::replace(&mut chunk, RowBatch::with_capacity(BATCH_ROWS * columns));
                sink(ExecEvent::Rows(full));
            }
        }
        if !chunk.is_empty() {
            sink(ExecEvent::Rows(chunk));
        }
        sink(ExecEvent::StatementDone { rows: count });
    }
    Ok(())
}

#[async_trait]
impl Session for SqliteSession {
    async fn execute(&mut self, sql: &str, row_limit: usize, sink: Sink<'_>) -> Result<()> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sql = sql.to_string();
        let conn = self.conn.clone();
        let job = tokio::task::spawn_blocking(move || {
            let mut send = |e| {
                let _ = tx.send(e);
            };
            run(&conn.lock().unwrap(), &sql, row_limit, &mut send)
        });
        while let Some(event) = rx.recv().await {
            sink(event);
        }
        job.await?
    }

    fn in_transaction(&self) -> bool {
        // Busy only while a statement runs, and then no one asks.
        self.conn.try_lock().is_ok_and(|c| !c.is_autocommit())
    }

    /// Always read-only.
    async fn set_read_only(&mut self, _: bool) -> Result<()> {
        Ok(())
    }

    /// No server setting; the engine interrupts a statement that runs over.
    async fn set_statement_timeout(&mut self, _: Option<Duration>) -> Result<()> {
        Ok(())
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        self.cancel.clone()
    }

    async fn validate(&mut self, sql: &str) -> Result<()> {
        let sql = sql.to_string();
        self.with(move |conn| conn.prepare(&sql).map(drop).map_err(sql_error))
            .await
    }

    async fn list_tables(&mut self) -> Result<Vec<TableInfo>> {
        self.with(list_tables).await
    }

    async fn list_columns(&mut self) -> Result<Vec<ColumnMeta>> {
        self.with(list_columns).await
    }

    async fn list_foreign_keys(&mut self) -> Result<Vec<ForeignKey>> {
        Ok(Vec::new())
    }

    fn is_alive(&self) -> bool {
        true
    }

    async fn close(self: Box<Self>) {}
}

fn list_tables(conn: &Connection) -> Result<Vec<TableInfo>> {
    let mut stmt = conn.prepare(
        "SELECT name, type FROM sqlite_schema
         WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )?;
    let tables = stmt
        .query_map([], |r| {
            let kind: String = r.get(1)?;
            Ok(TableInfo {
                schema: SCHEMA.into(),
                name: r.get(0)?,
                kind: if kind == "view" {
                    TableKind::View
                } else {
                    TableKind::Table
                },
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(tables)
}

fn list_columns(conn: &Connection) -> Result<Vec<ColumnMeta>> {
    let mut stmt = conn.prepare(
        "SELECT m.name, p.name, p.type, p.\"notnull\", p.pk
         FROM sqlite_schema m JOIN pragma_table_info(m.name) p
         WHERE m.type IN ('table', 'view') AND m.name NOT LIKE 'sqlite_%'
         ORDER BY m.name, p.cid",
    )?;
    let columns = stmt
        .query_map([], |r| {
            Ok(ColumnMeta {
                schema: SCHEMA.into(),
                table: r.get(0)?,
                name: r.get(1)?,
                data_type: r.get::<_, String>(2)?.to_lowercase(),
                nullable: !r.get::<_, bool>(3)?,
                primary_key: r.get::<_, i64>(4)? > 0,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(columns)
}

struct SqliteCancel(InterruptHandle);

#[async_trait]
impl CancelHandle for SqliteCancel {
    async fn cancel(&self) -> Result<()> {
        self.0.interrupt();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(path: &std::path::Path) -> ConnectConfig {
        ConnectConfig {
            host: String::new(),
            port: 0,
            user: String::new(),
            password: None,
            database: path.to_string_lossy().into_owned(),
            tls: tangul_core::TlsMode::Disable,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reads_but_never_writes() {
        let dir = std::env::temp_dir().join(format!("tangul-sqlite-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("metrics.db");
        let _ = std::fs::remove_file(&path);
        let db = Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE runs (id INTEGER PRIMARY KEY, ms REAL, note TEXT);
             INSERT INTO runs (ms, note) VALUES (1.5, 'a'), (2, NULL), (3, 'c');",
        )
        .unwrap();

        let mut session = SqliteDriver.connect(&config(&path)).await.unwrap();
        let mut events = Vec::new();
        let mut sink = |e: ExecEvent| events.push(e);
        session
            .execute(
                "SELECT ms, note FROM runs ORDER BY id; SELECT 1",
                2,
                &mut sink,
            )
            .await
            .unwrap();
        let rows: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                ExecEvent::StatementDone { rows } => Some(*rows),
                _ => None,
            })
            .collect();
        assert_eq!(rows, vec![2, 1]);
        assert!(events.iter().any(|e| matches!(e, ExecEvent::Truncated)));

        let mut sink = |_: ExecEvent| {};
        for write in [
            "DELETE FROM runs",
            "INSERT INTO runs (ms) VALUES (4)",
            "DROP TABLE runs",
            "PRAGMA query_only = OFF; DELETE FROM runs",
        ] {
            assert!(
                session.execute(write, 10, &mut sink).await.is_err(),
                "{write}"
            );
        }
        let count: i64 = db
            .query_row("SELECT count(*) FROM runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 3);

        let tables = session.list_tables().await.unwrap();
        assert_eq!(tables[0].name, "runs");
        let columns = session.list_columns().await.unwrap();
        assert_eq!(columns.len(), 3);
        assert!(columns[0].primary_key);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
