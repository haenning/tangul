//! `run_readonly`: a read through the engine's gate.

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tangul_core::QueryId;
use tangul_engine::{Caller, QueryOptions};

use crate::ctx::Ctx;
use crate::tool::{Tool, ToolError, ToolOutput, input, schema_of};

pub(crate) struct RunReadonly;

const DEFAULT_ROWS: usize = 50;
const MAX_ROWS: usize = 500;
/// Longest cell in the text for the model; longer ones are cut.
const CELL_CHARS: usize = 200;

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

/// Query ids of the assistant's reads: far from the app's, which count up from 1.
fn next_query() -> QueryId {
    static NEXT: AtomicU64 = AtomicU64::new(1 << 62);
    QueryId(NEXT.fetch_add(1, Ordering::Relaxed))
}

fn text(r: &Rows) -> String {
    let cell = |v: &Option<String>| match v {
        None => "NULL".to_string(),
        Some(v) if v.chars().count() > CELL_CHARS => {
            format!("{}…", v.chars().take(CELL_CHARS).collect::<String>())
        }
        Some(v) => v.replace(['\t', '\n'], " "),
    };
    let mut out = r.columns.join("\t");
    for row in &r.rows {
        out.push('\n');
        out.push_str(&row.iter().map(cell).collect::<Vec<_>>().join("\t"));
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
         run: propose them to the user instead. Keep results small (LIMIT, aggregates)."
    }

    fn input_schema(&self) -> Value {
        schema_of::<Input>()
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
        let engine = ctx.engine()?;
        let options = QueryOptions {
            row_limit: limit.unwrap_or(DEFAULT_ROWS).clamp(1, MAX_ROWS),
            ..QueryOptions::new(Caller::Assistant)
        };
        let out = engine
            .query(next_query(), conn.conn, sql, options)
            .await
            .map_err(|e| {
                if e.held.is_some() {
                    // The pass that would run it stays with the user; the model never
                    // sees it.
                    ToolError::NotAllowed(
                        "this statement writes and was not run; propose it to the user instead"
                            .into(),
                    )
                } else {
                    ToolError::Failed(e.message)
                }
            })?;
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
        let text = text(&rows);
        Ok(ToolOutput::new(&rows, text))
    }
}
