//! `propose_sql`: a statement for the user to review and run themselves. The
//! assistant never runs anything that changes data; the app opens it in a new tab,
//! unrun, on the user's own connection to the database.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ctx::{Ctx, SqlProposal};
use crate::tool::{Tool, ToolError, ToolOutput, input, schema_of};

pub(crate) struct ProposeSql;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Input {
    /// The statements, as the user should run them.
    sql: String,
    /// A name from list_connections; can be left out when there is only one.
    connection: Option<String>,
    /// One line on what it does and why, shown above it.
    note: Option<String>,
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
        "Propose statements that change something (INSERT, UPDATE, DELETE, DDL, grants). \
         They open in tangul in a new tab, not run: the user reviews and runs them as \
         themselves. Nothing here runs anything."
    }

    fn input_schema(&self) -> Value {
        schema_of::<Input>()
    }

    async fn call(&self, ctx: &Ctx, value: Value) -> Result<ToolOutput, ToolError> {
        let Input {
            sql,
            connection,
            note,
        } = input(value)?;
        if sql.trim().is_empty() {
            return Err(ToolError::BadInput("sql is empty".into()));
        }
        let sink = ctx.proposals().ok_or_else(|| {
            ToolError::NotAllowed("there is no app to open proposals in here".into())
        })?;
        let conn = ctx.conn(connection.as_deref())?;
        let name = ctx
            .exposed()
            .iter()
            .find(|e| e.user == conn.user)
            .map(|e| e.name.clone())
            .unwrap_or_default();
        let note = note
            .map(|n| n.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|n| !n.is_empty());
        let sql = match note {
            Some(note) => format!("-- Proposed by the assistant: {note}\n{}", sql.trim()),
            None => format!("-- Proposed by the assistant\n{}", sql.trim()),
        };
        sink(SqlProposal {
            connection: name.clone(),
            user: conn.user,
            sql,
        });
        let text = format!(
            "Opened in tangul in a new tab on {name} for the user to review and run. It was \
             not run."
        );
        Ok(ToolOutput::new(&Opened { connection: name }, text))
    }
}
