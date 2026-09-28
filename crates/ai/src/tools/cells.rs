//! `get_cell`: the whole value of a cell `run_readonly` cut, from tangul's copy of
//! the result (the database doesn't run the query again).

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ctx::Ctx;
use crate::results::{CELL_BYTES, Missing};
use crate::tool::{Tool, ToolError, ToolOutput, input, schema_of};

pub(crate) struct GetCell;

/// Longest text given back; the rest is left out (a `path` picks a part).
const MAX_CHARS: usize = 100_000;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Input {
    /// The result's id, as the cut cell says (`r3`).
    result: String,
    /// Its row, from 1.
    row: usize,
    /// Its column: the name, or its number from 1.
    column: String,
    /// For JSON: the part to give, as keys and array positions from 0 separated by
    /// dots (`lots.0.name`).
    path: Option<String>,
    /// A name from list_connections; can be left out when there is only one.
    connection: Option<String>,
}

#[derive(Serialize)]
struct Whole {
    value: Option<String>,
    chars: usize,
}

#[async_trait]
impl Tool for GetCell {
    fn name(&self) -> &'static str {
        "get_cell"
    }

    fn description(&self) -> &'static str {
        "The whole value of a cell run_readonly cut short, from tangul's copy of that \
         result (kept about 10 minutes); the database doesn't run anything. JSON comes \
         formatted; `path` picks a part of it (`lots.0.name`)."
    }

    fn input_schema(&self) -> Value {
        schema_of::<Input>()
    }

    async fn call(&self, ctx: &Ctx, value: Value) -> Result<ToolOutput, ToolError> {
        let Input {
            result,
            row,
            column,
            path,
            connection,
        } = input(value)?;
        let conn = ctx.conn(connection.as_deref())?;
        let cell = ctx
            .results()
            .cell(&result, &conn.name, row, &column)
            .map_err(|missing| match missing {
                Missing::Result => ToolError::NotFound(format!(
                    "result {result} (kept about 10 minutes); run the query again"
                )),
                Missing::Cell { rows, columns } => ToolError::NotFound(format!(
                    "row {row}, column {column} in {result}; it has rows 1–{rows} and \
                     columns {}",
                    columns.join(", ")
                )),
            })?;
        let Some(value) = cell.value.clone() else {
            return Ok(ToolOutput::new(
                &Whole {
                    value: None,
                    chars: 0,
                },
                "NULL",
            ));
        };
        // Only documents (objects, arrays) count as JSON here, not `1` or `true`.
        let json = serde_json::from_str::<Value>(&value)
            .ok()
            .filter(|j| j.is_object() || j.is_array())
            .ok_or(());
        let (mut text, part) = match (json, &path) {
            (Ok(json), Some(path)) => {
                let part = pick(&json, path)
                    .ok_or_else(|| ToolError::NotFound(format!("{path} in the cell's JSON")))?;
                (pretty(part), true)
            }
            (Ok(json), None) => (pretty(&json), false),
            (Err(_), Some(_)) => {
                return Err(ToolError::BadInput(
                    "the cell isn't JSON; leave out path".into(),
                ));
            }
            _ => (value, false),
        };
        let chars = text.chars().count();
        if chars > MAX_CHARS {
            text = text.chars().take(MAX_CHARS).collect();
            text.push_str(&format!(
                "\n…[{chars} chars, the first {MAX_CHARS} shown; a path gives a part]"
            ));
        } else if !part && cell.value.as_ref().is_some_and(|v| v.len() >= CELL_BYTES) {
            text.push_str(&format!(
                "\n…[tangul kept the first {CELL_BYTES} bytes of {} chars]",
                cell.chars
            ));
        }
        Ok(ToolOutput::new(
            &Whole {
                value: cell.value,
                chars: cell.chars,
            },
            text,
        ))
    }
}

fn pretty(json: &Value) -> String {
    match json {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// The part of `json` at `path` (`a.0.b`).
fn pick<'a>(json: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .filter(|p| !p.is_empty())
        .try_fold(json, |at, key| match at {
            Value::Object(map) => map.get(key),
            Value::Array(items) => items.get(key.parse::<usize>().ok()?),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn picks_parts_of_json() {
        let j = json!({"lots": [{"name": "A"}, {"name": "B"}], "n": 2});
        assert_eq!(pick(&j, "lots.1.name"), Some(&json!("B")));
        assert_eq!(pick(&j, "n"), Some(&json!(2)));
        assert_eq!(pick(&j, ""), Some(&j));
        assert_eq!(pick(&j, "lots.x"), None);
        assert_eq!(pick(&j, "nope"), None);
        assert_eq!(pretty(&json!("text")), "text");
    }
}
