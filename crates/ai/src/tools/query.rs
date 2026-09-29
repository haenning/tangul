//! `run_readonly`: a read through the engine's gate.

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tangul_core::{ConnId, QueryId};
use tangul_engine::{Caller, QueryOptions, QueryOutput};

use crate::ctx::Ctx;
use crate::tool::{Tool, ToolError, ToolOutput, input, schema_of};

pub(crate) struct RunReadonly;

const DEFAULT_ROWS: usize = 50;
const MAX_ROWS: usize = 500;
/// Longest cell in the text for the model; longer ones are cut, and the result is
/// kept so `get_cell` can give the whole value.
pub(crate) const CELL_CHARS: usize = 200;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Input {
    /// One read-only statement: SELECT, WITH, EXPLAIN, SHOW, …
    sql: String,
    /// A name from list_connections; can be left out when there is only one.
    connection: Option<String>,
    /// Rows to return at most (default 50, at most 500).
    limit: Option<usize>,
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    /// There were more rows than `limit`.
    pub truncated: bool,
}

/// Runs `sql` on `conn` through the engine's gate as the assistant, keeping at most
/// `limit` rows. A statement that isn't a read comes back refused: the engine never
/// runs it for the assistant; `propose_sql` opens it for the user instead.
pub(crate) async fn read(
    ctx: &Ctx,
    conn: ConnId,
    sql: String,
    limit: usize,
) -> Result<QueryOutput, ToolError> {
    let options = QueryOptions {
        row_limit: limit,
        ..QueryOptions::new(Caller::Assistant)
    };
    ctx.engine()?
        .query(next_query(), conn, sql, options)
        .await
        .map_err(|e| {
            if e.proposal.is_some() {
                ToolError::NotAllowed(
                    "this statement changes something and was not run; use propose_sql to \
                     open it for the user to review and run"
                        .into(),
                )
            } else {
                ToolError::Failed(e.message)
            }
        })
}

/// Query ids of the assistant's reads: far from the app's, which count up from 1.
fn next_query() -> QueryId {
    static NEXT: AtomicU64 = AtomicU64::new(1 << 62);
    QueryId(NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Whether any cell is longer than the text shows.
fn has_long_cells(r: &Rows) -> bool {
    r.rows
        .iter()
        .flatten()
        .flatten()
        .any(|v| v.chars().count() > CELL_CHARS)
}

/// The rows as tab-separated text; a cut cell says where to get the rest (`kept`:
/// the id the result is kept under).
fn text(r: &Rows, kept: Option<&str>) -> String {
    let cell = |row: usize, col: usize, v: &Option<String>| match v {
        None => "NULL".to_string(),
        Some(v) => {
            let chars = v.chars().count();
            let shown: String = v.chars().take(CELL_CHARS).collect();
            let shown = shown.replace(['\t', '\n'], " ");
            match kept {
                Some(id) if chars > CELL_CHARS => format!(
                    "{shown}…[{chars} chars; get_cell {id} row {} col {}]",
                    row + 1,
                    r.columns[col]
                ),
                _ => shown,
            }
        }
    };
    let mut out = r.columns.join("\t");
    for (i, row) in r.rows.iter().enumerate() {
        out.push('\n');
        let cells: Vec<String> = row.iter().enumerate().map(|(c, v)| cell(i, c, v)).collect();
        out.push_str(&cells.join("\t"));
    }
    let n = r.rows.len();
    out.push_str(&format!(
        "\n({n} row{}{})",
        if n == 1 { "" } else { "s" },
        if r.truncated { ", more not shown" } else { "" }
    ));
    out
}

#[async_trait]
impl Tool for RunReadonly {
    fn name(&self) -> &'static str {
        "run_readonly"
    }

    fn description(&self) -> &'static str {
        "Run one read-only statement and get its rows (tab-separated). It runs in a \
         read-only transaction that is rolled back, with a time limit. Writes are never \
         run: use propose_sql for them. Keep results small (LIMIT, aggregates). Cells \
         longer than 200 characters are cut and say how to get the whole value with \
         get_cell (no need to query again)."
    }

    fn input_schema(&self) -> Value {
        schema_of::<Input>()
    }

    fn runs_sql(&self) -> bool {
        true
    }

    async fn call(&self, ctx: &Ctx, value: Value) -> Result<ToolOutput, ToolError> {
        let Input {
            sql,
            connection,
            limit,
        } = input(value)?;
        if sql.trim().is_empty() {
            return Err(ToolError::BadInput("sql is empty".into()));
        }
        let conn = ctx.conn(connection.as_deref())?;
        let limit = limit.unwrap_or(DEFAULT_ROWS).clamp(1, MAX_ROWS);
        let statement = sql.trim().to_string();
        let out = read(ctx, conn.conn, sql, limit).await?;
        let rows = match out.results.last() {
            Some(set) => Rows {
                columns: set.columns().iter().map(|c| c.name.clone()).collect(),
                rows: (0..set.row_count())
                    .map(|r| {
                        (0..set.columns().len())
                            .map(|c| set.cell(r, c).map(str::to_string))
                            .collect()
                    })
                    .collect(),
                truncated: out.truncated,
            },
            None => Rows {
                columns: Vec::new(),
                rows: Vec::new(),
                truncated: false,
            },
        };
        let kept = has_long_cells(&rows).then(|| {
            ctx.results().keep(
                &conn.name,
                &statement,
                rows.columns.clone(),
                rows.rows.clone(),
            )
        });
        let text = text(&rows, kept.as_deref());
        let n = rows.rows.len() as u64;
        Ok(ToolOutput::new(&rows, text).with_rows(n))
    }
}
