//! `search_schema`: tables and columns by name, comment or enum value.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tangul_assist::schema::SchemaIndex;

use super::enum_of;
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

/// The words of `text`, lower case.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// How well `term` fits a name: exact, prefix, a word of it, part of it.
fn name_score(name: &str, term: &str) -> u32 {
    let name = name.to_lowercase();
    let singular = term.strip_suffix('s').filter(|s| s.len() > 2);
    let is = |t: &str| name == t;
    if is(term) || singular.is_some_and(is) {
        10
    } else if name.starts_with(term) || singular.is_some_and(|s| name.starts_with(s)) {
        6
    } else if words(&name)
        .iter()
        .any(|w| w == term || Some(w.as_str()) == singular)
    {
        5
    } else if name.contains(term) {
        3
    } else {
        0
    }
}

/// A word of the comment: 3 for the exact word, 2 for its singular.
fn comment_score(comment: Option<&str>, term: &str) -> u32 {
    let singular = term.strip_suffix('s').filter(|s| s.len() > 2);
    comment.map_or(0, |c| {
        let w = words(c);
        if w.iter().any(|w| w == term) {
            3
        } else if w.iter().any(|w| Some(w.as_str()) == singular) {
            2
        } else {
            0
        }
    })
}

pub(crate) fn search(schema: &SchemaIndex, query: &str, limit: usize) -> Vec<Hit> {
    let terms = words(query);
    let info = schema.info();
    let mut hits = Vec::new();
    for (ix, table) in schema.tables().iter().enumerate() {
        let name = schema.sql_name(ix);
        let (mut score, mut why) = (0, Vec::new());
        for t in &terms {
            let n = name_score(&table.name, t);
            let c = comment_score(table.comment.as_deref(), t);
            if n > 0 {
                why.push("name".to_string());
            } else if c > 0 {
                why.push(format!(
                    "comment: \"{}\"",
                    table.comment.as_deref().unwrap_or_default()
                ));
            }
            score += n.max(c);
        }
        if score > 0 {
            why.dedup();
            hits.push(Hit {
                table: name.clone(),
                column: None,
                why: why.join(", "),
                score: score + 1, // A table before its columns on a tie.
            });
        }
        for column in schema.columns(ix) {
            let values = enum_of(info, column).map(|e| &e.values);
            let (mut score, mut why) = (0, Vec::new());
            for t in &terms {
                let n = name_score(&column.name, t);
                let c = comment_score(column.comment.as_deref(), t);
                let v = values
                    .and_then(|vs| vs.iter().find(|v| v.to_lowercase() == *t))
                    .map(|v| (8, format!("value '{v}'")));
                let (best, reason) = [
                    (n, "name".to_string()),
                    v.unwrap_or((0, String::new())),
                    (
                        c,
                        format!(
                            "comment: \"{}\"",
                            column.comment.as_deref().unwrap_or_default()
                        ),
                    ),
                ]
                .into_iter()
                .max_by_key(|(s, _)| *s)
                .expect("three candidates");
                if best > 0 {
                    why.push(reason);
                }
                score += best;
            }
            if score > 0 {
                why.dedup();
                hits.push(Hit {
                    table: name.clone(),
                    column: Some(column.name.clone()),
                    why: why.join(", "),
                    score,
                });
            }
        }
    }
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.table.cmp(&b.table))
            .then_with(|| a.column.cmp(&b.column))
    });
    hits.truncate(limit);
    hits
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
        let hits = search(&conn.schema, &query, limit.unwrap_or(LIMIT).clamp(1, 100));
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
