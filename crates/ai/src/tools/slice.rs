//! `schema_slice`: the part of the schema a question is about.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tangul_assist::digest::digest;
use tangul_assist::slice::slice;

use crate::ctx::Ctx;
use crate::tool::{Tool, ToolError, ToolOutput, input, schema_of};

pub(crate) struct SchemaSlice;

const MAX_TABLES: usize = 8;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Input {
    /// The question in the user's words, e.g. `revenue per customer last month`.
    question: String,
    /// A name from list_connections; can be left out when there is only one.
    connection: Option<String>,
    /// Matching tables at most (default 8); their neighbours come on top.
    max_tables: Option<usize>,
}

#[derive(Debug, Serialize, PartialEq)]
struct Table {
    name: String,
    why: Vec<String>,
    digest: String,
}

#[async_trait]
impl Tool for SchemaSlice {
    fn name(&self) -> &'static str {
        "schema_slice"
    }

    fn description(&self) -> &'static str {
        "Start here for a question about the data: the tables it is about (matched by \
         names, comments and enum values) and the tables they join with, each with its \
         columns, keys, references, enum values and comments. Then describe_table for \
         details and find_join_path for joins."
    }

    fn input_schema(&self) -> Value {
        schema_of::<Input>()
    }

    async fn call(&self, ctx: &Ctx, value: Value) -> Result<ToolOutput, ToolError> {
        let Input {
            question,
            connection,
            max_tables,
        } = input(value)?;
        let conn = ctx.conn(connection.as_deref())?;
        let max = max_tables.unwrap_or(MAX_TABLES).clamp(1, 30);
        let tables: Vec<Table> = slice(&conn.schema, &conn.usage, &question, max)
            .into_iter()
            .map(|t| Table {
                name: conn.schema.sql_name(t.table),
                digest: digest(&conn.schema, t.table),
                why: t.why,
            })
            .collect();
        let text = if tables.is_empty() {
            format!(
                "No table matches \"{question}\"; try other words with search_schema, or \
                 list the tables with a query."
            )
        } else {
            tables
                .iter()
                .map(|t| format!("{}\n  why: {}", t.digest, t.why.join("; ")))
                .collect::<Vec<_>>()
                .join("\n\n")
        };
        Ok(ToolOutput::new(&tables, text))
    }
}
