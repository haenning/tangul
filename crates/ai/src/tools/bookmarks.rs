//! `list_bookmarks`: the user's saved queries for a database, by name, with their
//! SQL; run_readonly runs (and shows) one by name.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ctx::Ctx;
use crate::tool::{Tool, ToolError, ToolOutput, input, schema_of};

pub(crate) struct ListBookmarks;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Input {
    /// A name from list_connections; can be left out when there is only one.
    connection: Option<String>,
}

#[derive(Serialize)]
struct Listed {
    connection: String,
    bookmarks: Vec<Entry>,
}

#[derive(Serialize)]
struct Entry {
    name: String,
    sql: String,
}

#[async_trait]
impl Tool for ListBookmarks {
    fn name(&self) -> &'static str {
        "list_bookmarks"
    }

    fn description(&self) -> &'static str {
        "The user's bookmarks (saved queries) for a database: name and SQL. Run one \
         with run_readonly and `bookmark` (add `show` to open it for the user too)."
    }

    fn input_schema(&self) -> Value {
        schema_of::<Input>()
    }

    async fn call(&self, ctx: &Ctx, value: Value) -> Result<ToolOutput, ToolError> {
        let Input { connection } = input(value)?;
        let exposed = ctx.exposed_one(connection.as_deref())?;
        let listed = Listed {
            connection: exposed.name.clone(),
            bookmarks: exposed
                .bookmarks
                .iter()
                .map(|b| Entry {
                    name: b.name.clone(),
                    sql: b.sql.clone(),
                })
                .collect(),
        };
        let text = if listed.bookmarks.is_empty() {
            format!("No bookmarks on {}.", listed.connection)
        } else {
            let mut text = format!("Bookmarks on {}:", listed.connection);
            for b in &listed.bookmarks {
                text.push_str(&format!("\n\n{}:\n{}", b.name, b.sql.trim()));
            }
            text
        };
        Ok(ToolOutput::new(&listed, text))
    }
}
