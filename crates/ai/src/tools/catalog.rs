//! `list_enums` and `view_definition`.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::resolve_table;
use crate::ctx::Ctx;
use crate::tool::{Tool, ToolError, ToolOutput, input, schema_of};

pub(crate) struct ListEnums;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ConnInput {
    /// A name from list_connections; can be left out when there is only one.
    connection: Option<String>,
}

#[async_trait]
impl Tool for ListEnums {
    fn name(&self) -> &'static str {
        "list_enums"
    }

    fn description(&self) -> &'static str {
        "Enum types and their allowed values, in order (MySQL: per column, named \
         table.column). Use the exact values in conditions."
    }

    fn input_schema(&self) -> Value {
        schema_of::<ConnInput>()
    }

    async fn call(&self, ctx: &Ctx, value: Value) -> Result<ToolOutput, ToolError> {
        let ConnInput { connection } = input(value)?;
        let conn = ctx.conn(connection.as_deref())?;
        let enums = &conn.schema.info().enums;
        let text = if enums.is_empty() {
            "No enum types.".to_string()
        } else {
            enums
                .iter()
                .map(|e| {
                    let comment = e
                        .comment
                        .as_ref()
                        .map(|c| format!(" — {c}"))
                        .unwrap_or_default();
                    format!("{}.{}: {}{comment}", e.schema, e.name, e.values.join(", "))
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(ToolOutput::new(enums, text))
    }
}

pub(crate) struct ViewDefinition;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ViewInput {
    /// The view: `name` or `schema.name`.
    view: String,
    /// A name from list_connections; can be left out when there is only one.
    connection: Option<String>,
}

#[async_trait]
impl Tool for ViewDefinition {
    fn name(&self) -> &'static str {
        "view_definition"
    }

    fn description(&self) -> &'static str {
        "The query behind a view or materialized view, to see where its columns come from."
    }

    fn input_schema(&self) -> Value {
        schema_of::<ViewInput>()
    }

    async fn call(&self, ctx: &Ctx, value: Value) -> Result<ToolOutput, ToolError> {
        let ViewInput { view, connection } = input(value)?;
        let conn = ctx.conn(connection.as_deref())?;
        let ix = resolve_table(&conn.schema, &view)?;
        let t = conn.schema.table(ix);
        let def = conn
            .schema
            .info()
            .views
            .iter()
            .find(|v| v.schema == t.schema && v.name == t.name)
            .ok_or_else(|| {
                ToolError::NotFound(format!("{view} is a {}, not a view", t.kind.label()))
            })?;
        Ok(ToolOutput::new(def, def.definition.clone()))
    }
}
