use std::{sync::Arc, time::Duration};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures::{TryStreamExt, pin_mut};
use postgres_native_tls::MakeTlsConnector;
use tangul_core::{
    CatalogExtras, CheckInfo, ColumnInfo, ColumnMeta, ConnectConfig, EnumInfo, ForeignKey,
    IndexInfo, RoutineInfo, RowBatch, TableInfo, TableKind, TlsMode, ViewDef,
};
use tokio_postgres::{CancelToken, Client, SimpleQueryMessage, SimpleQueryRow, config::SslMode};

use crate::{BATCH_ROWS, CancelHandle, Driver, ExecEvent, Session, Sink, SqlError, catalog_part};

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
            .application_name("tangul")
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

    /// Read-only whatever the session says, and besides: a read gives up waiting for
    /// a lock (behind a migration's `ALTER TABLE`, say) after [`READ_LOCK_WAIT`], and
    /// the server ends the transaction if it is left idle. Both end with it.
    async fn begin_read_only(&mut self) -> Result<()> {
        let sql = format!(
            "START TRANSACTION READ ONLY; \
             SET LOCAL lock_timeout = '{READ_LOCK_WAIT}'; \
             SET LOCAL idle_in_transaction_session_timeout = '{READ_IDLE}'"
        );
        self.execute(&sql, 0, &mut |_| {}).await
    }

    async fn set_read_only(&mut self, on: bool) -> Result<()> {
        let mode = if on { "READ ONLY" } else { "READ WRITE" };
        self.client
            .simple_query(&format!(
                "SET SESSION CHARACTERISTICS AS TRANSACTION {mode}"
            ))
            .await
            .map_err(pg_error)?;
        Ok(())
    }

    async fn set_statement_timeout(&mut self, limit: Option<Duration>) -> Result<()> {
        let ms = limit.map_or(0, |l| l.as_millis());
        self.client
            .simple_query(&format!("SET statement_timeout = {ms}"))
            .await
            .map_err(pg_error)?;
        Ok(())
    }

    fn cancel_handle(&self) -> Arc<dyn CancelHandle> {
        self.cancel.clone()
    }

    async fn validate(&mut self, sql: &str) -> Result<()> {
        // Parse + Describe only; the unnamed statement is closed when dropped.
        self.client
            .prepare(sql)
            .await
            .map(drop)
            .map_err(|e| pg_error_in(e, Some(sql)))
    }

    async fn list_tables(&mut self) -> Result<Vec<TableInfo>> {
        // `reltuples` is -1 for a table never analyzed: no estimate.
        let sql = format!(
            "SELECT n.nspname, c.relname, c.relkind, obj_description(c.oid, 'pg_class'), \
               CASE WHEN c.reltuples >= 0 THEN c.reltuples::bigint END, \
               CASE WHEN c.relkind IN ('r','m','p') THEN pg_total_relation_size(c.oid) END \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind IN ('r','p','v','m','f') AND {USER_SCHEMAS} \
             ORDER BY 1, 2"
        );
        Ok(self
            .rows(&sql)
            .await?
            .iter()
            .map(|r| TableInfo {
                schema: text(r, 0),
                name: text(r, 1),
                kind: match r.get(2) {
                    Some("v") => TableKind::View,
                    Some("m") => TableKind::MaterializedView,
                    Some("p") => TableKind::Partitioned,
                    Some("f") => TableKind::Foreign,
                    _ => TableKind::Table,
                },
                comment: opt(r, 3),
                row_estimate: number(r, 4),
                size_bytes: number(r, 5),
            })
            .collect())
    }

    async fn list_columns(&mut self) -> Result<Vec<ColumnMeta>> {
        let sql = format!(
            "SELECT n.nspname, c.relname, a.attname, \
               format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, \
               EXISTS (SELECT 1 FROM pg_index i \
                       WHERE i.indrelid = c.oid AND i.indisprimary AND a.attnum = ANY(i.indkey)), \
               col_description(c.oid, a.attnum), \
               CASE WHEN a.attgenerated = '' THEN pg_get_expr(d.adbin, d.adrelid) END, \
               CASE WHEN a.attgenerated <> '' THEN pg_get_expr(d.adbin, d.adrelid) END, \
               a.attidentity <> '' OR coalesce(pg_get_expr(d.adbin, d.adrelid), '') LIKE 'nextval(%', \
               CASE WHEN t.typtype = 'e' THEN tn.nspname || '.' || t.typname END \
             FROM pg_attribute a \
             JOIN pg_class c ON c.oid = a.attrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_type t ON t.oid = a.atttypid \
             JOIN pg_namespace tn ON tn.oid = t.typnamespace \
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE a.attnum > 0 AND NOT a.attisdropped \
               AND c.relkind IN ('r','p','v','m','f') AND {USER_SCHEMAS} \
             ORDER BY 1, 2, a.attnum"
        );
        Ok(self
            .rows(&sql)
            .await?
            .iter()
            .map(|r| ColumnMeta {
                schema: text(r, 0),
                table: text(r, 1),
                name: text(r, 2),
                data_type: text(r, 3),
                nullable: r.get(4) == Some("t"),
                primary_key: r.get(5) == Some("t"),
                comment: opt(r, 6),
                default: opt(r, 7),
                generated: opt(r, 8),
                identity: r.get(9) == Some("t"),
                enum_type: opt(r, 10),
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
        Ok(self
            .rows(SQL)
            .await?
            .iter()
            .map(|r| ForeignKey {
                name: text(r, 0),
                schema: text(r, 1),
                table: text(r, 2),
                ref_schema: text(r, 3),
                ref_table: text(r, 4),
                columns: list(r, 5),
                ref_columns: list(r, 6),
            })
            .collect())
    }

    async fn catalog_extras(&mut self) -> Result<CatalogExtras> {
        Ok(CatalogExtras {
            indexes: catalog_part("indexes", self.indexes().await),
            checks: catalog_part("check constraints", self.checks().await),
            enums: catalog_part("enums", self.enums().await),
            views: catalog_part("view definitions", self.views().await),
            routines: catalog_part("routines", self.routines().await),
        })
    }

    fn is_alive(&self) -> bool {
        !self.client.is_closed()
    }

    async fn close(self: Box<Self>) {
        // Dropping the client terminates the connection task.
    }
}

/// Schemas of the user's objects: not the system's, TOAST or temporary ones.
/// How long a read in a read-only transaction waits for a lock.
const READ_LOCK_WAIT: &str = "5s";
/// How long a read-only transaction may sit idle before the server ends it.
const READ_IDLE: &str = "60s";

const USER_SCHEMAS: &str = "n.nspname NOT IN ('pg_catalog','information_schema') \
     AND n.nspname NOT LIKE 'pg_toast%' AND n.nspname NOT LIKE 'pg_temp%'";

fn text(r: &SimpleQueryRow, i: usize) -> String {
    r.get(i).unwrap_or_default().to_string()
}

fn opt(r: &SimpleQueryRow, i: usize) -> Option<String> {
    r.get(i).filter(|s| !s.is_empty()).map(str::to_string)
}

fn number(r: &SimpleQueryRow, i: usize) -> Option<u64> {
    r.get(i)?.parse().ok()
}

/// A list joined with the unit separator (no sane name contains it).
fn list(r: &SimpleQueryRow, i: usize) -> Vec<String> {
    r.get(i)
        .filter(|s| !s.is_empty())
        .map(|s| s.split('\u{1f}').map(String::from).collect())
        .unwrap_or_default()
}

/// `CHECK ((total >= (0)::numeric))` → `total >= (0)::numeric`.
fn check_expression(def: &str) -> String {
    let inner = def.strip_prefix("CHECK ").unwrap_or(def);
    unwrap_parens(unwrap_parens(inner)).to_string()
}

/// `(a AND (b))` → `a AND (b)`: one pair of parentheses around the whole, if the
/// first one closes at the end.
fn unwrap_parens(s: &str) -> &str {
    let s = s.trim();
    let Some(inner) = s.strip_prefix('(').and_then(|s| s.strip_suffix(')')) else {
        return s;
    };
    let mut depth = 0i32;
    for c in inner.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            // `(a) AND (b)`: the first one closes early.
            return s;
        }
    }
    inner.trim()
}

impl PgSession {
    async fn rows(&self, sql: &str) -> Result<Vec<SimpleQueryRow>> {
        let msgs = self.client.simple_query(sql).await.map_err(pg_error)?;
        Ok(msgs
            .into_iter()
            .filter_map(|m| match m {
                SimpleQueryMessage::Row(r) => Some(r),
                _ => None,
            })
            .collect())
    }

    async fn indexes(&self) -> Result<Vec<IndexInfo>> {
        // Key columns (or expressions) one by one, in index order.
        let sql = format!(
            "SELECT n.nspname, c.relname, ic.relname, i.indisunique, i.indisprimary, \
               pg_get_expr(i.indpred, i.indrelid), am.amname, \
               (SELECT string_agg(pg_get_indexdef(i.indexrelid, k, true), chr(31) ORDER BY k) \
                  FROM generate_series(1, i.indnkeyatts) k) \
             FROM pg_index i \
             JOIN pg_class ic ON ic.oid = i.indexrelid \
             JOIN pg_class c ON c.oid = i.indrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_am am ON am.oid = ic.relam \
             WHERE {USER_SCHEMAS} \
             ORDER BY 1, 2, 3"
        );
        Ok(self
            .rows(&sql)
            .await?
            .iter()
            .map(|r| IndexInfo {
                schema: text(r, 0),
                table: text(r, 1),
                name: text(r, 2),
                unique: r.get(3) == Some("t"),
                primary: r.get(4) == Some("t"),
                predicate: opt(r, 5).map(|p| unwrap_parens(&p).to_string()),
                method: opt(r, 6),
                columns: list(r, 7),
            })
            .collect())
    }

    async fn checks(&self) -> Result<Vec<CheckInfo>> {
        let sql = format!(
            "SELECT n.nspname, c.relname, con.conname, pg_get_constraintdef(con.oid, true) \
             FROM pg_constraint con \
             JOIN pg_class c ON c.oid = con.conrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE con.contype = 'c' AND {USER_SCHEMAS} \
             ORDER BY 1, 2, 3"
        );
        Ok(self
            .rows(&sql)
            .await?
            .iter()
            .map(|r| CheckInfo {
                schema: text(r, 0),
                table: text(r, 1),
                name: text(r, 2),
                expression: check_expression(&text(r, 3)),
            })
            .collect())
    }

    async fn enums(&self) -> Result<Vec<EnumInfo>> {
        let sql = format!(
            "SELECT n.nspname, t.typname, \
               string_agg(e.enumlabel, chr(31) ORDER BY e.enumsortorder), \
               obj_description(t.oid, 'pg_type') \
             FROM pg_type t \
             JOIN pg_enum e ON e.enumtypid = t.oid \
             JOIN pg_namespace n ON n.oid = t.typnamespace \
             WHERE {USER_SCHEMAS} \
             GROUP BY n.nspname, t.typname, t.oid \
             ORDER BY 1, 2"
        );
        Ok(self
            .rows(&sql)
            .await?
            .iter()
            .map(|r| EnumInfo {
                schema: text(r, 0),
                name: text(r, 1),
                values: list(r, 2),
                comment: opt(r, 3),
            })
            .collect())
    }

    async fn views(&self) -> Result<Vec<ViewDef>> {
        let sql = format!(
            "SELECT n.nspname, c.relname, pg_get_viewdef(c.oid, true) \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind IN ('v','m') AND {USER_SCHEMAS} \
             ORDER BY 1, 2"
        );
        Ok(self
            .rows(&sql)
            .await?
            .iter()
            .map(|r| ViewDef {
                schema: text(r, 0),
                name: text(r, 1),
                definition: text(r, 2).trim().to_string(),
            })
            .collect())
    }

    async fn routines(&self) -> Result<Vec<RoutineInfo>> {
        // Not those of extensions (pgcrypto's dozens of functions, …).
        let sql = format!(
            "SELECT n.nspname, p.proname, \
               CASE p.prokind WHEN 'p' THEN 'procedure' WHEN 'a' THEN 'aggregate' \
                 WHEN 'w' THEN 'window' ELSE 'function' END, \
               pg_get_function_arguments(p.oid), \
               CASE WHEN p.prokind <> 'p' THEN pg_get_function_result(p.oid) END, \
               obj_description(p.oid, 'pg_proc') \
             FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE {USER_SCHEMAS} \
               AND NOT EXISTS (SELECT 1 FROM pg_depend d \
                               WHERE d.objid = p.oid AND d.deptype = 'e') \
             ORDER BY 1, 2, 4"
        );
        Ok(self
            .rows(&sql)
            .await?
            .iter()
            .map(|r| RoutineInfo {
                schema: text(r, 0),
                name: text(r, 1),
                kind: text(r, 2),
                arguments: text(r, 3),
                returns: opt(r, 4),
                comment: opt(r, 5),
            })
            .collect())
    }
}

async fn run_simple(client: &Client, sql: &str, row_limit: usize, sink: Sink<'_>) -> Result<()> {
    let stream = client
        .simple_query_raw(sql)
        .await
        .map_err(|e| pg_error_in(e, Some(sql)))?;
    pin_mut!(stream);

    let mut cols = 0usize;
    let mut rows_in_set = 0usize;
    let mut truncated = false;
    let mut batch = RowBatch::default();

    while let Some(msg) = stream
        .try_next()
        .await
        .map_err(|e| pg_error_in(e, Some(sql)))?
    {
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
    pg_error_in(e, None)
}

/// With the executed SQL, the error carries its position as a [`SqlError`].
fn pg_error_in(e: tokio_postgres::Error, sql: Option<&str>) -> anyhow::Error {
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
    let position = match (db.position(), sql) {
        (Some(tokio_postgres::error::ErrorPosition::Original(p)), Some(sql)) => {
            char_to_byte(sql, *p as usize)
        }
        _ => None,
    };
    anyhow!(SqlError {
        message: msg,
        position,
    })
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

/// Byte offset of a 1-based character position (Postgres reports characters).
fn char_to_byte(sql: &str, position: usize) -> Option<usize> {
    let index = position.checked_sub(1)?;
    sql.char_indices()
        .nth(index)
        .map(|(i, _)| i)
        .or_else(|| (index == sql.chars().count()).then_some(sql.len()))
}

/// Cheap check whether `sql` may change transaction state. Used to avoid a
/// transaction-status round-trip after ordinary statements.
fn may_touch_transaction(sql: &str) -> bool {
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
    #[test]
    fn expressions_without_extra_parentheses() {
        use super::{check_expression, unwrap_parens};
        assert_eq!(
            check_expression("CHECK ((total >= (0)::numeric))"),
            "total >= (0)::numeric"
        );
        assert_eq!(check_expression("CHECK (a > 0)"), "a > 0");
        assert_eq!(unwrap_parens("(mood = 'happy')"), "mood = 'happy'");
        assert_eq!(unwrap_parens("(a) AND (b)"), "(a) AND (b)");
        assert_eq!(unwrap_parens("a > 1"), "a > 1");
    }

    use super::{char_to_byte, may_touch_transaction};

    #[test]
    fn error_positions() {
        assert_eq!(char_to_byte("select föo", 8), Some(7));
        // `ö` is two bytes: the final `o` (character 10) starts at byte 10.
        assert_eq!(char_to_byte("select föo", 10), Some(10));
        assert_eq!(char_to_byte("select föo", 11), Some(11));
        assert_eq!(char_to_byte("abc", 4), Some(3));
        assert_eq!(char_to_byte("abc", 0), None);
    }

    #[test]
    fn transaction_keywords() {
        assert!(may_touch_transaction("BEGIN"));
        assert!(may_touch_transaction("select 1; commit;"));
        assert!(!may_touch_transaction("select begins_at from t"));
        assert!(!may_touch_transaction("select * from users"));
    }
}
