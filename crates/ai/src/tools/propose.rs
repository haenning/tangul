//! `propose_sql`: SQL for the user to look at and run themselves, opened in a new
//! tab, unrun, on the user's own connection to the database. The assistant never
//! runs it and never gets its result: changes, and queries the user runs
//! themselves (on a schema-only database the only way to hand one over). A result
//! the assistant already got goes over with `run_readonly` and `show` instead.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tangul_assist::guard;

use super::one_line;
use super::query::Chart;
use crate::ctx::{ChartRequest, Ctx, SqlProposal};
use crate::tool::{Tool, ToolError, ToolOutput, input, schema_of};

pub(crate) struct ProposeSql;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Input {
    /// The statements, as the user should run them.
    sql: String,
    /// A name from list_connections; can be left out when there is only one.
    connection: Option<String>,
    /// The tab's title.
    title: Option<String>,
    /// One line on what it does and why, shown above it.
    note: Option<String>,
    /// A query's result as this chart once the user runs it, instead of the grid:
    /// the first column is the x axis (dates or labels), the others are numbers,
    /// one series each.
    chart: Option<Chart>,
    /// With `chart`: one number column by name; all of them when left out.
    column: Option<String>,
}

#[derive(Serialize)]
struct Opened {
    connection: String,
}

#[async_trait]
impl Tool for ProposeSql {
    fn name(&self) -> &'static str {
        "propose_sql"
    }

    fn description(&self) -> &'static str {
        "Hand the user SQL in tangul, in a new tab, not run: they review it and run it \
         as themselves, and its result stays with them. For any change (INSERT, UPDATE, \
         DELETE, DDL, grants), which you never run, but only one the user asked for. \
         For queries the user should run or edit themselves; on a schema-only database \
         (list_connections) this is how you hand over a query, with `chart` to draw its \
         result. A result you already got goes over with run_readonly and `show`."
    }

    fn input_schema(&self) -> Value {
        schema_of::<Input>()
    }

    async fn call(&self, ctx: &Ctx, value: Value) -> Result<ToolOutput, ToolError> {
        let Input {
            sql,
            connection,
            title,
            note,
            chart,
            column,
        } = input(value)?;
        let sink = ctx.proposals().ok_or_else(|| {
            ToolError::NotAllowed("there is no app to open proposals in here".into())
        })?;
        if sql.trim().is_empty() {
            return Err(ToolError::BadInput("sql is empty".into()));
        }
        if chart.is_none() && column.is_some() {
            return Err(ToolError::BadInput("column needs a chart".into()));
        }
        let conn = ctx.conn(connection.as_deref())?;
        let reads = guard::not_reads(&sql).is_empty();
        if chart.is_some() && !reads {
            return Err(ToolError::BadInput(
                "only a query's result can be a chart".into(),
            ));
        }
        let name = conn.name.clone();
        let sql = match one_line(note) {
            Some(note) => format!("-- Proposed by the assistant: {note}\n{}", sql.trim()),
            None => format!("-- Proposed by the assistant\n{}", sql.trim()),
        };
        sink(SqlProposal {
            connection: name.clone(),
            user: conn.user,
            sql,
            title: one_line(title),
            chart: chart.map(|c| ChartRequest {
                style: c.style(),
                column: one_line(column),
            }),
        });
        let mut text = format!(
            "Opened in tangul in a new tab on {name} for the user to review and run. It was \
             not run."
        );
        if reads {
            text.push_str(" Its result is the user's: it never comes back to you.");
        }
        Ok(ToolOutput::new(&Opened { connection: name }, text))
    }
}
