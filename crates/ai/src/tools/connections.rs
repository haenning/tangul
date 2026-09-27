//! `list_connections`: the databases the assistant may use.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ctx::Ctx;
use crate::tool::{Tool, ToolError, ToolOutput, input, schema_of};

pub(crate) struct ListConnections;

/// No input.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Input {}

#[derive(Debug, Serialize, PartialEq)]
struct Connection {
    name: String,
    database: &'static str,
    connected: bool,
    tables: usize,
    /// `none`, `statistics` or `values`: what may be read from the rows.
    values: tangul_core::ValueAccess,
}

#[async_trait]
impl Tool for ListConnections {
    fn name(&self) -> &'static str {
        "list_connections"
    }

    fn description(&self) -> &'static str {
        "The databases you may use, with their kind (PostgreSQL, MySQL, SQLite) and \
         table count. Pass a name as `connection` to other tools when there are several."
    }

    fn input_schema(&self) -> Value {
        schema_of::<Input>()
    }

    async fn call(&self, ctx: &Ctx, value: Value) -> Result<ToolOutput, ToolError> {
        let Input {} = input(value)?;
        let list: Vec<Connection> = ctx
            .exposed()
            .iter()
            .map(|e| {
                let known = ctx.knowledge().user(&e.user);
                Connection {
                    name: e.name.clone(),
                    database: e.driver.label(),
                    connected: known.is_some(),
                    tables: known.map_or(0, |k| k.schema.tables().len()),
                    values: e.values,
                }
            })
            .collect();
        let text = if list.is_empty() {
            "No connection is enabled for the assistant.".to_string()
        } else {
            list.iter()
                .map(|c| {
                    let state = if c.connected {
                        format!("{} tables", c.tables)
                    } else {
                        "not connected".into()
                    };
                    let values = match c.values {
                        tangul_core::ValueAccess::None => "",
                        tangul_core::ValueAccess::Statistics => ", column statistics readable",
                        tangul_core::ValueAccess::Values => ", column values readable",
                    };
                    format!("{} ({}, {state}{values})", c.name, c.database)
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(ToolOutput::new(&list, text))
    }
}
