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
    CatalogExtras, CheckInfo, ColumnInfo, ColumnMeta, ConnectConfig, ForeignKey, IndexInfo,
    RowBatch, TableInfo, TableKind, ViewDef,
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

    /// The file is opened read-only; a plain transaction is enough.
    async fn begin_read_only(&mut self) -> Result<()> {
        self.execute("BEGIN", 0, &mut |_| {}).await
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
        self.with(list_foreign_keys).await
    }

    /// No comments, enums or routines in SQLite.
    async fn catalog_extras(&mut self) -> Result<CatalogExtras> {
        self.with(|conn| {
            Ok(CatalogExtras {
                indexes: crate::catalog_part("indexes", list_indexes(conn)),
                checks: crate::catalog_part("check constraints", list_checks(conn)),
                views: crate::catalog_part("view definitions", list_views(conn)),
                ..Default::default()
            })
        })
        .await
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
                ..Default::default()
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(tables)
}

fn list_columns(conn: &Connection) -> Result<Vec<ColumnMeta>> {
    let mut stmt = conn.prepare(
        "SELECT m.name, p.name, p.type, p.\"notnull\", p.pk, p.dflt_value,
           -- The rowid alias: a single INTEGER PRIMARY KEY column.
           p.pk = 1 AND upper(p.type) = 'INTEGER'
             AND (SELECT count(*) FROM pragma_table_info(m.name) q WHERE q.pk > 0) = 1
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
                default: r.get(5)?,
                identity: r.get(6)?,
                ..Default::default()
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(columns)
}

/// Names of the user's tables (not SQLite's own).
fn table_names(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_schema
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let names = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(names)
}

fn list_foreign_keys(conn: &Connection) -> Result<Vec<ForeignKey>> {
    let mut keys: Vec<ForeignKey> = Vec::new();
    for table in table_names(conn)? {
        let mut stmt = conn.prepare(
            "SELECT id, \"table\", \"from\", \"to\" FROM pragma_foreign_key_list(?1) \
             ORDER BY id, seq",
        )?;
        let rows: Vec<(i64, String, String, Option<String>)> = stmt
            .query_map([&table], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut last_id = None;
        for (id, ref_table, from, to) in rows {
            if last_id != Some(id) {
                last_id = Some(id);
                keys.push(ForeignKey {
                    name: format!("{table}_fk{id}"),
                    schema: SCHEMA.into(),
                    table: table.clone(),
                    ref_schema: SCHEMA.into(),
                    ref_table,
                    columns: Vec::new(),
                    ref_columns: Vec::new(),
                });
            }
            let fk = keys.last_mut().expect("pushed above");
            fk.columns.push(from);
            // Without a column the key refers to the other table's primary key.
            fk.ref_columns.push(to.unwrap_or_default());
        }
    }
    for fk in &mut keys {
        if fk.ref_columns.iter().any(String::is_empty) {
            let mut stmt =
                conn.prepare("SELECT name FROM pragma_table_info(?1) WHERE pk > 0 ORDER BY pk")?;
            fk.ref_columns = stmt
                .query_map([&fk.ref_table], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
        }
    }
    Ok(keys)
}

fn list_indexes(conn: &Connection) -> Result<Vec<IndexInfo>> {
    let mut out = Vec::new();
    for table in table_names(conn)? {
        let mut stmt = conn.prepare(
            "SELECT l.name, l.\"unique\", l.origin, l.partial, m.sql
             FROM pragma_index_list(?1) l
             LEFT JOIN sqlite_schema m ON m.type = 'index' AND m.name = l.name
             ORDER BY l.name",
        )?;
        let rows: Vec<(String, bool, String, bool, Option<String>)> = stmt
            .query_map([&table], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        for (name, unique, origin, partial, sql) in rows {
            let mut cols = conn.prepare(
                "SELECT coalesce(name, '(expression)') FROM pragma_index_info(?1) ORDER BY seqno",
            )?;
            let columns = cols
                .query_map([&name], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            out.push(IndexInfo {
                schema: SCHEMA.into(),
                table: table.clone(),
                name,
                columns,
                unique,
                primary: origin == "pk",
                predicate: sql
                    .filter(|_| partial)
                    .and_then(|s| after_keyword(&s, " WHERE ")),
                method: None,
            });
        }
    }
    Ok(out)
}

fn list_checks(conn: &Connection) -> Result<Vec<CheckInfo>> {
    let mut stmt = conn.prepare(
        "SELECT name, sql FROM sqlite_schema
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND sql IS NOT NULL ORDER BY name",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows
        .into_iter()
        .flat_map(|(table, sql)| {
            check_clauses(&sql)
                .into_iter()
                .enumerate()
                .map(move |(i, (name, expression))| CheckInfo {
                    schema: SCHEMA.into(),
                    name: name.unwrap_or_else(|| format!("{table}_check{}", i + 1)),
                    table: table.clone(),
                    expression,
                })
        })
        .collect())
}

fn list_views(conn: &Connection) -> Result<Vec<ViewDef>> {
    let mut stmt = conn.prepare(
        "SELECT name, sql FROM sqlite_schema WHERE type = 'view' AND sql IS NOT NULL ORDER BY name",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows
        .into_iter()
        .map(|(name, sql)| ViewDef {
            schema: SCHEMA.into(),
            name,
            definition: after_keyword(&sql, " AS ").unwrap_or(sql),
        })
        .collect())
}

/// The text after the first `keyword` (any case), trimmed.
fn after_keyword(sql: &str, keyword: &str) -> Option<String> {
    let at = sql.to_ascii_uppercase().find(keyword)?;
    Some(sql[at + keyword.len()..].trim().to_string())
}

/// The `CHECK (…)` clauses of a `CREATE TABLE`: (constraint name, condition).
fn check_clauses(sql: &str) -> Vec<(Option<String>, String)> {
    let words = words(sql);
    let mut out = Vec::new();
    for (i, &(start, word)) in words.iter().enumerate() {
        if !word.eq_ignore_ascii_case("CHECK") {
            continue;
        }
        let after = start + word.len();
        let Some(open) = sql[after..].find('(').map(|o| after + o) else {
            continue;
        };
        let mut depth = 0usize;
        let mut end = None;
        for (j, c) in sql[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(open + j);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { continue };
        // `CONSTRAINT name CHECK (…)`
        let name = (i >= 2 && words[i - 2].1.eq_ignore_ascii_case("CONSTRAINT"))
            .then(|| words[i - 1].1.trim_matches(['"', '`']).to_string());
        out.push((name, sql[open + 1..end].trim().to_string()));
    }
    out
}

/// Words of `sql` (letters, digits, `_`, quoted names) with their byte offsets.
fn words(sql: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in sql.char_indices() {
        let word = c.is_alphanumeric() || c == '_' || c == '"' || c == '`';
        match (word, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                out.push((s, &sql[s..i]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, &sql[s..]));
    }
    out
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
    async fn reads_the_whole_catalog() {
        let dir = std::env::temp_dir().join(format!("tangul-sqlite-cat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shop.db");
        let _ = std::fs::remove_file(&path);
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE,
                                     active INTEGER NOT NULL DEFAULT 1);
                 CREATE TABLE orders (
                     id INTEGER PRIMARY KEY,
                     user_id INTEGER NOT NULL REFERENCES users (id),
                     status TEXT NOT NULL DEFAULT 'open',
                     total REAL NOT NULL CHECK (total >= 0),
                     CONSTRAINT known_status CHECK (status IN ('open', 'paid'))
                 );
                 CREATE TABLE notes (order_id INTEGER REFERENCES orders, body TEXT);
                 CREATE INDEX orders_by_user ON orders (user_id, status);
                 CREATE INDEX open_orders ON orders (user_id) WHERE status = 'open';
                 CREATE VIEW paid AS SELECT * FROM orders WHERE status = 'paid';",
            )
            .unwrap();
        let mut session = SqliteDriver.connect(&config(&path)).await.unwrap();

        let columns = session.list_columns().await.unwrap();
        let col = |t: &str, c: &str| {
            columns
                .iter()
                .find(|m| m.table == t && m.name == c)
                .unwrap()
        };
        assert!(col("orders", "id").identity);
        assert!(!col("orders", "user_id").identity);
        assert_eq!(col("orders", "status").default.as_deref(), Some("'open'"));
        assert_eq!(col("users", "active").default.as_deref(), Some("1"));

        let keys = session.list_foreign_keys().await.unwrap();
        let fk = |t: &str| keys.iter().find(|k| k.table == t).unwrap();
        assert_eq!(
            (
                fk("orders").ref_table.as_str(),
                &fk("orders").columns,
                &fk("orders").ref_columns
            ),
            (
                "users",
                &vec!["user_id".to_string()],
                &vec!["id".to_string()]
            )
        );
        // No column given: the primary key of the table referred to.
        assert_eq!(fk("notes").ref_columns, ["id"]);

        let extras = session.catalog_extras().await.unwrap();
        let index = |n: &str| extras.indexes.iter().find(|i| i.name == n).unwrap();
        assert_eq!(index("orders_by_user").columns, ["user_id", "status"]);
        assert!(!index("orders_by_user").unique);
        assert_eq!(
            index("open_orders").predicate.as_deref(),
            Some("status = 'open'")
        );
        assert!(
            extras
                .indexes
                .iter()
                .any(|i| i.table == "users" && i.unique)
        );
        let checks: Vec<(&str, &str)> = extras
            .checks
            .iter()
            .map(|c| (c.name.as_str(), c.expression.as_str()))
            .collect();
        assert_eq!(
            checks,
            [
                ("orders_check1", "total >= 0"),
                ("known_status", "status IN ('open', 'paid')")
            ]
        );
        assert_eq!(extras.views.len(), 1);
        assert_eq!(
            extras.views[0].definition,
            "SELECT * FROM orders WHERE status = 'paid'"
        );
        assert!(extras.enums.is_empty() && extras.routines.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
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
