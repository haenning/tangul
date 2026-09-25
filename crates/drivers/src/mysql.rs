use std::sync::Arc;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use mysql_async::{
    Conn, Opts, OptsBuilder, Row, SslOpts, Value, consts::StatusFlags, prelude::Queryable,
};
use rusdb_core::{
    ColumnInfo, ColumnMeta, ConnectConfig, ForeignKey, RowBatch, TableInfo, TableKind, TlsMode,
};

use crate::{
    BATCH_ROWS, CancelHandle, Driver, ExecEvent, Session, Sink, SqlError, mysql_near,
    quoted_name_position,
};

pub struct MysqlDriver;

fn opts(cfg: &ConnectConfig, tls: bool) -> Opts {
    let mut b = OptsBuilder::default()
        .ip_or_hostname(cfg.host.clone())
        .tcp_port(cfg.port)
        .user(Some(cfg.user.clone()))
        .pass(cfg.password.clone())
        .prefer_socket(false);
    if !cfg.database.is_empty() {
        b = b.db_name(Some(cfg.database.clone()));
    }
    if tls {
        // Encrypt without verification, same semantics as the Postgres driver.
        // TODO(tls): add verified modes.
        b = b.ssl_opts(
            SslOpts::default()
                .with_danger_accept_invalid_certs(true)
                .with_danger_skip_domain_validation(true),
        );
    }
    b.into()
}

#[async_trait]
impl Driver for MysqlDriver {
    async fn connect(&self, cfg: &ConnectConfig) -> Result<Box<dyn Session>> {
        let conn = match cfg.tls {
            TlsMode::Disable => Conn::new(opts(cfg, false)).await?,
            TlsMode::Require => Conn::new(opts(cfg, true)).await?,
            // mysql_async has no native "prefer": try TLS first, fall back to plain.
            TlsMode::Prefer => match Conn::new(opts(cfg, true)).await {
                Ok(c) => c,
                Err(_) => Conn::new(opts(cfg, false)).await?,
            },
        };
        let cancel = Arc::new(MysqlCancel {
            opts: conn.opts().clone(),
            conn_id: conn.id(),
        });
        Ok(Box::new(MysqlSession {
            conn: Some(conn),
            cancel,
            in_transaction: false,
        }))
    }
}

struct MysqlSession {
    // Option so `close` can move the connection out for a clean disconnect.
    conn: Option<Conn>,
    cancel: Arc<MysqlCancel>,
    in_transaction: bool,
}

impl MysqlSession {
    fn conn(&mut self) -> Result<&mut Conn> {
        self.conn
            .as_mut()
            .ok_or_else(|| anyhow!("connection closed"))
    }
}

#[async_trait]
impl Session for MysqlSession {
    async fn execute(&mut self, sql: &str, row_limit: usize, sink: Sink<'_>) -> Result<()> {
        let conn = self.conn()?;
        let result = run_text(conn, sql, row_limit, sink)
            .await
            .map_err(|e| with_position(e, sql));
        // The server reports the transaction state in every OK packet.
        self.in_transaction = self
            .conn
            .as_ref()
            .and_then(|c| c.last_ok_packet())
            .map(|ok| {
                ok.status_flags()
                    .contains(StatusFlags::SERVER_STATUS_IN_TRANS)
            })
            .unwrap_or(self.in_transaction);
        result
    }

    fn in_transaction(&self) -> bool {
        self.in_transaction
    }

    async fn set_read_only(&mut self, on: bool) -> Result<()> {
        let mode = if on { "READ ONLY" } else { "READ WRITE" };
        self.conn()?
            .query_drop(format!("SET SESSION TRANSACTION {mode}"))
            .await?;
        Ok(())
    }

    /// MySQL limits `SELECT`s by `max_execution_time` (ms); MariaDB limits every
    /// statement by `max_statement_time` (seconds). 0 is no limit for both.
    async fn set_statement_timeout(&mut self, limit: Option<std::time::Duration>) -> Result<()> {
        let conn = self.conn()?;
        let ms = limit.map_or(0, |l| l.as_millis());
        if conn
            .query_drop(format!("SET SESSION max_execution_time = {ms}"))
            .await
            .is_ok()
        {
            return Ok(());
        }
        let secs = limit.map_or(0., |l| l.as_secs_f64());
        conn.query_drop(format!("SET SESSION max_statement_time = {secs}"))
            .await
            .map_err(|e| anyhow!("could not limit the statement time: {e}"))
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        self.cancel.clone()
    }

    async fn validate(&mut self, sql: &str) -> Result<()> {
        /// ER_UNSUPPORTED_PS: statements the prepared protocol can't take (USE, …).
        const UNSUPPORTED: u16 = 1295;
        let conn = self.conn()?;
        match conn.prep(sql).await {
            Ok(stmt) => Ok(conn.close(stmt).await?),
            Err(mysql_async::Error::Server(e)) if e.code == UNSUPPORTED => Ok(()),
            Err(e) => Err(with_position(e.into(), sql)),
        }
    }

    async fn list_tables(&mut self) -> Result<Vec<TableInfo>> {
        const SQL: &str = "SELECT table_schema, table_name, table_type \
            FROM information_schema.tables \
            WHERE table_schema NOT IN ('mysql','information_schema','performance_schema','sys') \
            ORDER BY 1, 2";
        let rows: Vec<(String, String, String)> = self.conn()?.query(SQL).await?;
        Ok(rows
            .into_iter()
            .map(|(schema, name, kind)| TableInfo {
                schema,
                name,
                kind: if kind.contains("VIEW") {
                    TableKind::View
                } else {
                    TableKind::Table
                },
            })
            .collect())
    }

    async fn list_columns(&mut self) -> Result<Vec<ColumnMeta>> {
        const SQL: &str = "SELECT table_schema, table_name, column_name, column_type, \
              is_nullable = 'YES', column_key = 'PRI' \
            FROM information_schema.columns \
            WHERE table_schema NOT IN ('mysql','information_schema','performance_schema','sys') \
            ORDER BY table_schema, table_name, ordinal_position";
        let rows: Vec<(String, String, String, String, i64, i64)> = self.conn()?.query(SQL).await?;
        Ok(rows
            .into_iter()
            .map(
                |(schema, table, name, data_type, nullable, pk)| ColumnMeta {
                    schema,
                    table,
                    name,
                    data_type,
                    nullable: nullable != 0,
                    primary_key: pk != 0,
                },
            )
            .collect())
    }

    async fn list_foreign_keys(&mut self) -> Result<Vec<ForeignKey>> {
        // GROUP_CONCAT's SEPARATOR must be a literal; CHAR(31) is not allowed there.
        const SQL: &str = "SELECT constraint_name, table_schema, table_name, \
              referenced_table_schema, referenced_table_name, \
              GROUP_CONCAT(column_name ORDER BY ordinal_position SEPARATOR ','), \
              GROUP_CONCAT(referenced_column_name ORDER BY ordinal_position SEPARATOR ',') \
            FROM information_schema.key_column_usage \
            WHERE referenced_table_name IS NOT NULL \
              AND table_schema NOT IN ('mysql','information_schema','performance_schema','sys') \
            GROUP BY constraint_name, table_schema, table_name, \
              referenced_table_schema, referenced_table_name \
            ORDER BY 2, 3, 1";
        type Row = (String, String, String, String, String, String, String);
        let rows: Vec<Row> = self.conn()?.query(SQL).await?;
        let split = |s: String| s.split(',').map(String::from).collect();
        Ok(rows
            .into_iter()
            .map(
                |(name, schema, table, ref_schema, ref_table, cols, ref_cols)| ForeignKey {
                    name,
                    schema,
                    table,
                    columns: split(cols),
                    ref_schema,
                    ref_table,
                    ref_columns: split(ref_cols),
                },
            )
            .collect())
    }

    fn is_alive(&self) -> bool {
        self.conn.as_ref().is_some_and(|c| !c.is_disconnected())
    }

    async fn close(mut self: Box<Self>) {
        if let Some(conn) = self.conn.take() {
            let _ = conn.disconnect().await;
        }
    }
}

async fn run_text(conn: &mut Conn, sql: &str, row_limit: usize, sink: Sink<'_>) -> Result<()> {
    let mut result = conn.query_iter(sql).await?;
    loop {
        let columns = result.columns().unwrap_or_else(|| Arc::from([]));
        if columns.is_empty() {
            // OK packet only (INSERT/UPDATE/DDL ...).
            sink(ExecEvent::StatementDone {
                rows: result.affected_rows(),
            });
            // Advances to the next result set, if any.
            result.next().await?;
        } else {
            sink(ExecEvent::ResultStart(
                columns
                    .iter()
                    .map(|c| ColumnInfo {
                        name: c.name_str().into_owned(),
                    })
                    .collect(),
            ));
            let mut batch = RowBatch::default();
            let mut rows = 0usize;
            let mut truncated = false;
            // `next` returns None at the end of this set and moves to the next one.
            while let Some(row) = result.next().await? {
                rows += 1;
                if rows > row_limit {
                    // TODO(fetch-more): stop reading instead of discarding.
                    if !truncated {
                        truncated = true;
                        sink(ExecEvent::Truncated);
                    }
                    continue;
                }
                push_row(&mut batch, &row, columns.len());
                if batch.rows() >= BATCH_ROWS {
                    sink(ExecEvent::Rows(std::mem::take(&mut batch)));
                }
            }
            if !batch.is_empty() {
                sink(ExecEvent::Rows(batch));
            }
            sink(ExecEvent::StatementDone { rows: rows as u64 });
        }
        if result.is_empty() {
            break;
        }
    }
    Ok(())
}

fn push_row(batch: &mut RowBatch, row: &Row, cols: usize) {
    for i in 0..cols {
        match row.as_ref(i) {
            None | Some(Value::NULL) => batch.push_cell(None),
            Some(Value::Bytes(b)) => match std::str::from_utf8(b) {
                Ok(text) => batch.push_cell(Some(text)),
                // Binary data (BLOB, BINARY, ...) is shown as hex, like Postgres bytea.
                Err(_) => batch.push_cell(Some(&hex(b))),
            },
            // Text protocol only yields bytes; other variants are formatted defensively.
            Some(v) => batch.push_cell(Some(&v.as_sql(true))),
        }
    }
    batch.finish_row();
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("0x");
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

struct MysqlCancel {
    opts: Opts,
    conn_id: u32,
}

#[async_trait]
impl CancelHandle for MysqlCancel {
    /// MySQL has no out-of-band cancel: open a short-lived second connection.
    async fn cancel(&self) -> Result<()> {
        let mut conn = Conn::new(self.opts.clone()).await?;
        conn.query_drop(format!("KILL QUERY {}", self.conn_id))
            .await?;
        conn.disconnect().await?;
        Ok(())
    }
}

/// Server errors become a [`SqlError`] with the position MySQL describes as
/// `near '…' at line N`.
fn with_position(e: anyhow::Error, sql: &str) -> anyhow::Error {
    match e.downcast_ref::<mysql_async::Error>() {
        Some(mysql_async::Error::Server(server)) => {
            let message = e.to_string();
            let position = mysql_near(&server.message, sql)
                .or_else(|| quoted_name_position(&server.message, sql));
            anyhow::anyhow!(SqlError { message, position })
        }
        _ => e,
    }
}
