//! `find_join_path`: how to join two tables.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tangul_assist::join_graph::{JoinGraph, JoinSource, JoinStep, join_sql};
use tangul_assist::schema::SchemaIndex;

use super::resolve_table;
use crate::ctx::Ctx;
use crate::tool::{Tool, ToolError, ToolOutput, input, schema_of};

pub(crate) struct FindJoinPath;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Input {
    /// The table to start from: `name` or `schema.name`.
    from: String,
    /// The table to reach.
    to: String,
    /// A name from list_connections; can be left out when there is only one.
    connection: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
struct Step {
    from: String,
    from_columns: Vec<String>,
    to: String,
    to_columns: Vec<String>,
    /// `foreign key <name>` or `learned from your queries`.
    source: String,
}

#[derive(Debug, Serialize, PartialEq)]
struct Path {
    connected: bool,
    steps: Vec<Step>,
    /// `FROM … JOIN … ON …`, ready to use.
    sql: Option<String>,
}

pub(crate) fn source(s: &JoinSource) -> String {
    match s {
        JoinSource::ForeignKey(name) => format!("foreign key {name}"),
        JoinSource::Learned => "learned from your queries".into(),
    }
}

fn step(schema: &SchemaIndex, s: &JoinStep) -> Step {
    Step {
        from: schema.sql_name(s.from),
        from_columns: s.from_cols.clone(),
        to: schema.sql_name(s.to),
        to_columns: s.to_cols.clone(),
        source: source(&s.source),
    }
}

#[async_trait]
impl Tool for FindJoinPath {
    fn name(&self) -> &'static str {
        "find_join_path"
    }

    fn description(&self) -> &'static str {
        "How to join two tables: the chain of join conditions from declared foreign keys \
         and joins the user's queries use, with the FROM … JOIN … ON clause ready to use."
    }

    fn input_schema(&self) -> Value {
        schema_of::<Input>()
    }

    async fn call(&self, ctx: &Ctx, value: Value) -> Result<ToolOutput, ToolError> {
        let Input {
            from,
            to,
            connection,
        } = input(value)?;
        let conn = ctx.conn(connection.as_deref())?;
        let (a, b) = (
            resolve_table(&conn.schema, &from)?,
            resolve_table(&conn.schema, &to)?,
        );
        let graph = JoinGraph::new(&conn.schema, &conn.usage);
        let path = match graph.path(a, b) {
            Some(steps) => Path {
                connected: true,
                sql: Some(join_sql(&conn.schema, a, &steps)),
                steps: steps.iter().map(|s| step(&conn.schema, s)).collect(),
            },
            None => Path {
                connected: false,
                steps: Vec::new(),
                sql: None,
            },
        };
        let (fa, fb) = (conn.schema.sql_name(a), conn.schema.sql_name(b));
        let text = match &path.sql {
            None => format!(
                "No join path between {fa} and {fb}: no foreign keys or joins from your \
                 queries connect them."
            ),
            Some(sql) => {
                let mut lines: Vec<String> = path
                    .steps
                    .iter()
                    .map(|s| {
                        let on: Vec<String> = s
                            .from_columns
                            .iter()
                            .zip(&s.to_columns)
                            .map(|(x, y)| format!("{}.{x} = {}.{y}", s.from, s.to))
                            .collect();
                        format!("{} ({})", on.join(" AND "), s.source)
                    })
                    .collect();
                lines.push(sql.clone());
                lines.join("\n")
            }
        };
        Ok(ToolOutput::new(&path, text))
    }
}
