//! `run_readonly`: a read through the engine's gate, of SQL or a bookmark; with
//! `show`, its result also opens in tangul for the user, as the grid or a chart.

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tangul_core::chart::ChartStyle;
use tangul_core::{ConnId, QueryId, ResultSet};
use tangul_engine::{Caller, QueryOptions, QueryOutput, ROW_LIMIT};

use super::one_line;
use crate::ctx::{ChartRequest, Ctx, ShownResult};
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
    /// One read-only statement: SELECT, WITH, EXPLAIN, SHOW, … Or give `bookmark`
    /// instead.
    sql: Option<String>,
    /// A bookmark's name (list_bookmarks): runs its SQL.
    bookmark: Option<String>,
    /// A name from list_connections; can be left out when there is only one.
    connection: Option<String>,
    /// Rows to return to you at most (default 50, at most 500). What `show` opens
    /// for the user isn't cut to this.
    limit: Option<usize>,
    /// Also open the result in tangul for the user, in a new tab: the grid, or a
    /// chart.
    show: Option<Show>,
}

/// How the result is shown to the user.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Show {
    /// The tab's title (a bookmark's name when left out).
    title: Option<String>,
    /// One line on what it shows, above the SQL.
    note: Option<String>,
    /// Show it as this chart instead of the grid: the first column is the x axis
    /// (dates or labels), the others are numbers, one series each.
    chart: Option<Chart>,
    /// With `chart`: one number column by name; all of them when left out.
    column: Option<String>,
}

/// The chart styles tangul draws (see `tangul_features`).
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Chart {
    Line,
    Area,
    StackedArea,
    Bars,
    StackedBars,
}

impl Chart {
    fn style(self) -> ChartStyle {
        match self {
            Chart::Line => ChartStyle::Line,
            Chart::Area => ChartStyle::Area,
            Chart::StackedArea => ChartStyle::StackedArea,
            Chart::Bars => ChartStyle::Bars,
            Chart::StackedBars => ChartStyle::StackedBars,
        }
    }
}

/// The statement to run: `sql`, or a bookmark's SQL (and its name).
fn statement(
    ctx: &Ctx,
    sql: Option<String>,
    bookmark: Option<&str>,
    connection: Option<&str>,
) -> Result<(String, Option<String>), ToolError> {
    match (sql, bookmark) {
        (Some(sql), None) => Ok((sql, None)),
        (None, Some(name)) => {
            let exposed = ctx.exposed_one(connection)?;
            let found = exposed
                .bookmarks
                .iter()
                .find(|b| b.name.eq_ignore_ascii_case(name.trim()))
                .ok_or_else(|| {
                    ToolError::NotFound(format!(
                        "bookmark {name} on {}; list_bookmarks lists them",
                        exposed.name
                    ))
                })?;
            Ok((found.sql.clone(), Some(found.name.clone())))
        }
        _ => Err(ToolError::BadInput("give one of sql or bookmark".into())),
    }
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

    fn reads_rows(&self) -> bool {
        true
    }

    fn description(&self) -> &'static str {
        "Run one read-only statement, or a bookmark (list_bookmarks) by name, and get \
         its rows (tab-separated). It runs once, in a read-only transaction that is \
         rolled back, with a time limit. Writes are never run: use propose_sql for them. \
         Keep results small (LIMIT, aggregates). Cells longer than 200 characters are \
         cut and say how to get the whole value with get_cell (no need to query again). \
         Set `show` to also open the result in tangul for the user, in a new tab: the \
         whole result in the grid, or as a chart (line, area, stacked_area, bars, \
         stacked_bars; GROUP BY reads best). Use it to visualize data, draw a graph, or \
         hand the user a finished query with its result; the user doesn't need to run \
         it again."
    }

    fn input_schema(&self) -> Value {
        schema_of::<Input>()
    }

    fn statement(&self, ctx: &Ctx, input: &Value) -> Option<String> {
        let text = |key| input.get(key).and_then(Value::as_str);
        statement(
            ctx,
            text("sql").map(str::to_string),
            text("bookmark"),
            text("connection"),
        )
        .ok()
        .map(|(sql, _)| sql)
    }

    async fn call(&self, ctx: &Ctx, value: Value) -> Result<ToolOutput, ToolError> {
        let Input {
            sql,
            bookmark,
            connection,
            limit,
            show,
        } = input(value)?;
        let (sql, bookmark) = statement(ctx, sql, bookmark.as_deref(), connection.as_deref())?;
        if sql.trim().is_empty() {
            return Err(ToolError::BadInput("sql is empty".into()));
        }
        if show.is_some() && ctx.shows().is_none() {
            return Err(ToolError::NotAllowed(
                "there is no app to show results in here".into(),
            ));
        }
        if let Some(Show {
            chart: None,
            column: Some(_),
            ..
        }) = &show
        {
            return Err(ToolError::BadInput("column needs a chart".into()));
        }
        let conn = ctx.conn(connection.as_deref())?;
        // What of its result the model may see, before anything runs.
        let plan = conn.plan(&sql)?;
        let limit = limit.unwrap_or(DEFAULT_ROWS).clamp(1, MAX_ROWS);
        // What the user is shown isn't cut to what the model reads.
        let kept_rows = if show.is_some() { ROW_LIMIT } else { limit };
        let statement = sql.trim().to_string();
        let out = read(ctx, conn.session()?, sql, kept_rows).await?;
        let set = out.results.into_iter().last();
        // The model reads it as AI tools see it; the user is shown all of it.
        let (seen, note) = match &set {
            Some(set) => {
                let (seen, note) = plan.apply(set);
                (Some(seen), note)
            }
            None => (None, None),
        };
        let rows = match &seen {
            Some(set) => Rows {
                columns: set.columns().iter().map(|c| c.name.clone()).collect(),
                rows: (0..set.row_count().min(limit))
                    .map(|r| {
                        (0..set.columns().len())
                            .map(|c| set.cell(r, c).map(str::to_string))
                            .collect()
                    })
                    .collect(),
                truncated: out.truncated || set.row_count() > limit,
            },
            None => Rows {
                columns: Vec::new(),
                rows: Vec::new(),
                truncated: false,
            },
        };
        let kept = has_long_cells(&rows).then(|| {
            ctx.results()
                .keep(&conn.name, rows.columns.clone(), rows.rows.clone())
        });
        let mut text = text(&rows, kept.as_deref());
        if let Some(note) = note {
            text.push_str(&format!("\n{note}"));
        }
        if let (Some(show), Some(sink)) = (show, ctx.shows()) {
            let chart = show.chart.map(|c| ChartRequest {
                style: c.style(),
                column: one_line(show.column),
            });
            let shown = match &chart {
                Some(ChartRequest {
                    style,
                    column: Some(column),
                }) => format!("as a {} chart of {column}", style.key()),
                Some(ChartRequest { style, .. }) => format!("as a {} chart", style.key()),
                None => "in the grid".to_string(),
            };
            text.push_str(&format!(
                "\nOpened in tangul in a new tab on {}, {shown} (the grid if the result \
                 doesn't fit the chart).",
                conn.name
            ));
            sink(ShownResult {
                connection: conn.name.clone(),
                user: conn.user.clone(),
                sql: statement,
                title: one_line(show.title).or(bookmark),
                note: one_line(show.note),
                chart,
                result: set.unwrap_or_else(|| ResultSet::new(Vec::new())),
                truncated: out.truncated,
                elapsed: out.elapsed,
            });
        }
        let n = rows.rows.len() as u64;
        Ok(ToolOutput::new(&rows, text).with_rows(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_chart_style_can_be_asked_for() {
        for style in ChartStyle::ALL {
            let chart: Chart = serde_json::from_value(Value::from(style.key())).unwrap();
            assert_eq!(chart.style(), style);
        }
    }
}
