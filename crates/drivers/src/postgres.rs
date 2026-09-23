use std::{sync::Arc, time::Duration};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures::{TryStreamExt, pin_mut};
use postgres_native_tls::MakeTlsConnector;
use rusdb_core::{
    ColumnInfo, ColumnMeta, ConnectConfig, ForeignKey, RowBatch, TableInfo, TableKind, TlsMode,
};
use tokio_postgres::{CancelToken, Client, SimpleQueryMessage, config::SslMode};

use crate::{BATCH_ROWS, CancelHandle, Driver, ExecEvent, Session, Sink, may_touch_transaction};

pub struct PgDriver;

fn tls_connector() -> Result<MakeTlsConnector> {
    // libpq "prefer"/"require" semantics: encrypt, but don't verify the certificate.
    // TODO(tls): add verify-ca / verify-full modes.
    let connector = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .danger_accept_invalid_hostnames(true)
        .build()?;
    Ok(MakeTlsConnector::new(connector))
}

#[async_trait]
impl Driver for PgDriver {
    async fn connect(&self, cfg: &ConnectConfig) -> Result<Box<dyn Session>> {
        let mut pg = tokio_postgres::Config::new();
        pg.host(&cfg.host)
            .port(cfg.port)
            .user(&cfg.user)
            .application_name("rusdb")
            .connect_timeout(Duration::from_secs(10))
            .ssl_mode(match cfg.tls {
                TlsMode::Disable => SslMode::Disable,
                TlsMode::Prefer => SslMode::Prefer,
                TlsMode::Require => SslMode::Require,
            });
        if let Some(pw) = &cfg.password {
            pg.password(pw);
        }
        if !cfg.database.is_empty() {
            pg.dbname(&cfg.database);
        }

        let tls = tls_connector()?;
        let (client, connection) = pg.connect(tls.clone()).await.map_err(pg_error)?;
        // The connection future drives the socket; it ends when the client is dropped.
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                log_conn_error(&e);
            }
        });

        Ok(Box::new(PgSession {
            cancel: Arc::new(PgCancel {
                token: client.cancel_token(),
                tls,
            }),
            client,
            in_transaction: false,
        }))
    }
}

fn log_conn_error(e: &tokio_postgres::Error) {
    eprintln!("postgres connection closed: {e}");
}

struct PgSession {
    client: Client,
    cancel: Arc<PgCancel>,
    in_transaction: bool,
}

impl PgSession {
    /// Postgres exposes the transaction status only in the protocol's ReadyForQuery
    /// message, which tokio-postgres does not surface. Inside a transaction block
    /// `now()` (transaction start) differs from `statement_timestamp()`. A failed
    /// probe means the transaction is aborted, which still counts as open.
    async fn probe_transaction(&self) -> bool {
        match self
            .client
            .simple_query("SELECT now() <> statement_timestamp()")
            .await
        {
            Ok(msgs) => msgs.iter().any(|m| match m {
                SimpleQueryMessage::Row(r) => r.get(0) == Some("t"),
                _ => false,
            }),
            Err(_) => true,
        }
    }
}

#[async_trait]
impl Session for PgSession {
    async fn execute(&mut self, sql: &str, row_limit: usize, sink: Sink<'_>) -> Result<()> {
        let result = run_simple(&self.client, sql, row_limit, sink).await;
        if self.in_transaction || may_touch_transaction(sql) {
            self.in_transaction = self.probe_transaction().await;
        }
        result
    }

    fn in_transaction(&self) -> bool {
        self.in_transaction
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        self.cancel.clone()
    }

    async fn list_tables(&mut self) -> Result<Vec<TableInfo>> {
        const SQL: &str = "SELECT n.nspname, c.relname, c.relkind \
            FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
            WHERE c.relkind IN ('r','p','v','m','f') \
              AND n.nspname NOT IN ('pg_catalog','information_schema') \
              AND n.nspname NOT LIKE 'pg_toast%' AND n.nspname NOT LIKE 'pg_temp%' \
            ORDER BY 1, 2";
        let msgs = self.client.simple_query(SQL).await.map_err(pg_error)?;
        Ok(msgs
            .iter()
            .filter_map(|m| match m {
                SimpleQueryMessage::Row(r) => Some(TableInfo {
                    schema: r.get(0).unwrap_or_default().to_string(),
                    name: r.get(1).unwrap_or_default().to_string(),
                    kind: match r.get(2) {
                        Some("v") | Some("m") => TableKind::View,
                        _ => TableKind::Table,
                    },
                }),
                _ => None,
            })
            .collect())
    }

    async fn list_columns(&mut self) -> Result<Vec<ColumnMeta>> {
        const SQL: &str = "SELECT n.nspname, c.relname, a.attname, \
              format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, \
              EXISTS (SELECT 1 FROM pg_index i \
                      WHERE i.indrelid = c.oid AND i.indisprimary AND a.attnum = ANY(i.indkey)) \
            FROM pg_attribute a \
            JOIN pg_class c ON c.oid = a.attrelid \
            JOIN pg_namespace n ON n.oid = c.relnamespace \
            WHERE a.attnum > 0 AND NOT a.attisdropped \
              AND c.relkind IN ('r','p','v','m','f') \
              AND n.nspname NOT IN ('pg_catalog','information_schema') \
              AND n.nspname NOT LIKE 'pg_toast%' AND n.nspname NOT LIKE 'pg_temp%' \
            ORDER BY 1, 2, a.attnum";
        let msgs = self.client.simple_query(SQL).await.map_err(pg_error)?;
        let text = |r: &tokio_postgres::SimpleQueryRow, i| r.get(i).unwrap_or_default().to_string();
        Ok(msgs
            .iter()
            .filter_map(|m| match m {
                SimpleQueryMessage::Row(r) => Some(ColumnMeta {
                    schema: text(r, 0),
                    table: text(r, 1),
                    name: text(r, 2),
                    data_type: text(r, 3),
                    nullable: r.get(4) == Some("t"),
                    primary_key: r.get(5) == Some("t"),
                }),
                _ => None,
            })
            .collect())
    }

    async fn list_foreign_keys(&mut self) -> Result<Vec<ForeignKey>> {
        // Column lists are joined with the unit separator, which no sane name contains.
        const SQL: &str = "SELECT con.conname, n.nspname, c.relname, rn.nspname, rc.relname, \
              (SELECT string_agg(a.attname, chr(31) ORDER BY k.ord) \
                 FROM unnest(con.conkey) WITH ORDINALITY k(attnum, ord) \
                 JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.attnum), \
              (SELECT string_agg(a.attname, chr(31) ORDER BY k.ord) \
                 FROM unnest(con.confkey) WITH ORDINALITY k(attnum, ord) \
                 JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.attnum) \
            FROM pg_constraint con \
            JOIN pg_class c ON c.oid = con.conrelid \
            JOIN pg_namespace n ON n.oid = c.relnamespace \
            JOIN pg_class rc ON rc.oid = con.confrelid \
            JOIN pg_namespace rn ON rn.oid = rc.relnamespace \
            WHERE con.contype = 'f' \
              AND n.nspname NOT IN ('pg_catalog','information_schema') \
            ORDER BY 2, 3, 1";
        let msgs = self.client.simple_query(SQL).await.map_err(pg_error)?;
        let text = |r: &tokio_postgres::SimpleQueryRow, i| r.get(i).unwrap_or_default().to_string();
        let list = |r: &tokio_postgres::SimpleQueryRow, i| {
            r.get(i)
                .unwrap_or_default()
                .split('\u{1f}')
                .map(String::from)
                .collect()
        };
        Ok(msgs
            .iter()
            .filter_map(|m| match m {
                SimpleQueryMessage::Row(r) => Some(ForeignKey {
                    name: text(r, 0),
                    schema: text(r, 1),
                    table: text(r, 2),
                    ref_schema: text(r, 3),
                    ref_table: text(r, 4),
                    columns: list(r, 5),
                    ref_columns: list(r, 6),
                }),
                _ => None,
            })
            .collect())
    }

    fn is_alive(&self) -> bool {
        !self.client.is_closed()
    }

    async fn close(self: Box<Self>) {
        // Dropping the client terminates the connection task.
    }
}

async fn run_simple(client: &Client, sql: &str, row_limit: usize, sink: Sink<'_>) -> Result<()> {
    let stream = client.simple_query_raw(sql).await.map_err(pg_error)?;
    pin_mut!(stream);

    let mut cols = 0usize;
    let mut rows_in_set = 0usize;
    let mut truncated = false;
    let mut batch = RowBatch::default();

    while let Some(msg) = stream.try_next().await.map_err(pg_error)? {
        match msg {
            SimpleQueryMessage::RowDescription(desc) => {
                cols = desc.len();
                rows_in_set = 0;
                truncated = false;
                sink(ExecEvent::ResultStart(
                    desc.iter()
                        .map(|c| ColumnInfo {
                            name: c.name().to_string(),
                        })
                        .collect(),
                ));
            }
            SimpleQueryMessage::Row(row) => {
                rows_in_set += 1;
                if rows_in_set > row_limit {
                    // TODO(fetch-more): use a server-side cursor instead of discarding.
                    if !truncated {
                        truncated = true;
                        sink(ExecEvent::Truncated);
                    }
                    continue;
                }
                for i in 0..cols {
                    batch.push_cell(row.get(i));
                }
                batch.finish_row();
                if batch.rows() >= BATCH_ROWS {
                    sink(ExecEvent::Rows(std::mem::take(&mut batch)));
                }
            }
            SimpleQueryMessage::CommandComplete(n) => {
                if !batch.is_empty() {
                    sink(ExecEvent::Rows(std::mem::take(&mut batch)));
                }
                sink(ExecEvent::StatementDone { rows: n });
            }
            _ => {}
        }
    }
    Ok(())
}

/// Formats server errors like psql does (message, detail, hint, position).
fn pg_error(e: tokio_postgres::Error) -> anyhow::Error {
    let Some(db) = e.as_db_error() else {
        // Client-side errors keep the useful part in the source chain
        // (e.g. "invalid configuration: password missing").
        let mut msg = e.to_string();
        let mut source = std::error::Error::source(&e);
        while let Some(s) = source {
            msg.push_str(&format!(": {s}"));
            source = s.source();
        }
        return anyhow!(msg);
    };
    let mut msg = format!("{}: {}", db.severity(), db.message());
    if let Some(d) = db.detail() {
        msg.push_str(&format!("\nDETAIL: {d}"));
    }
    if let Some(h) = db.hint() {
        msg.push_str(&format!("\nHINT: {h}"));
    }
    if let Some(tokio_postgres::error::ErrorPosition::Original(p)) = db.position() {
        msg.push_str(&format!("\nPOSITION: {p}"));
    }
    anyhow!(msg)
}

struct PgCancel {
    token: CancelToken,
    tls: MakeTlsConnector,
}

#[async_trait]
impl CancelHandle for PgCancel {
    async fn cancel(&self) -> Result<()> {
        self.token
            .cancel_query(self.tls.clone())
            .await
            .map_err(pg_error)
    }
}
