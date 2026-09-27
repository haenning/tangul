//! `search_schema`: tables and columns by name, comment or enum value.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tangul_assist::schema::SchemaIndex;
use tangul_assist::slice::{KnownValues, matches};

use crate::ctx::Ctx;
use crate::tool::{Tool, ToolError, ToolOutput, input, schema_of};

pub(crate) struct SearchSchema;

const LIMIT: usize = 20;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Input {
    /// Words to look for, e.g. `customer email` or `cancelled`.
    query: String,
    /// A name from list_connections; can be left out when there is only one.
    connection: Option<String>,
    /// How many hits at most (default 20).
    limit: Option<usize>,
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct Hit {
    /// The table as written in SQL.
    pub table: String,
    pub column: Option<String>,
    /// What matched, e.g. `name`, `comment: "Registered customers"`, `value 'paid'`.
    pub why: String,
    pub score: u32,
}

pub(crate) fn search(
    schema: &SchemaIndex,
    known: &KnownValues,
    query: &str,
    limit: usize,
) -> Vec<Hit> {
    matches(schema, query, known)
        .into_iter()
        .take(limit)
        .map(|m| Hit {
            table: schema.sql_name(m.table),
            column: m.column.map(|c| schema.columns(m.table)[c].name.clone()),
            why: m.why,
            score: m.score,
        })
        .collect()
}

#[async_trait]
impl Tool for SearchSchema {
    fn name(&self) -> &'static str {
        "search_schema"
    }

    fn description(&self) -> &'static str {
        "Find tables and columns by words in their names, comments or enum values, best \
         first. Use it to find where something is stored before describe_table."
    }

    fn input_schema(&self) -> Value {
        schema_of::<Input>()
    }

    async fn call(&self, ctx: &Ctx, value: Value) -> Result<ToolOutput, ToolError> {
        let Input {
            query,
            connection,
            limit,
        } = input(value)?;
        if query.trim().is_empty() {
            return Err(ToolError::BadInput("query is empty".into()));
        }
        let conn = ctx.conn(connection.as_deref())?;
        let hits = search(
            &conn.schema,
            &conn.known_values,
            &query,
            limit.unwrap_or(LIMIT).clamp(1, 100),
        );
        let text = if hits.is_empty() {
            format!("Nothing matches \"{query}\".")
        } else {
            hits.iter()
                .map(|h| match &h.column {
                    Some(c) => format!("{}.{c} — {}", h.table, h.why),
                    None => format!("{} — {}", h.table, h.why),
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(ToolOutput::new(&hits, text))
    }
}
