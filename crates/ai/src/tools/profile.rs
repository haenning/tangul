//! `column_profile`: what a column's values look like, from a sample. Only where
//! the user allows the assistant to read values; never for columns that look like
//! secrets.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tangul_assist::profile::{
    SAMPLE_ROWS, TOP_VALUES, clip, counts_sql, is_secret, top_values_sql,
};
use tangul_assist::refine::ColumnKind;
use tangul_core::ColumnProfile;

use super::query::read;
use super::resolve_table;
use crate::ctx::Ctx;
use crate::tool::{Tool, ToolError, ToolOutput, input, schema_of};

pub(crate) struct ColumnProfileTool;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Input {
    /// The table or view: `name` or `schema.name`.
    table: String,
    /// The column.
    column: String,
    /// A name from list_connections; can be left out when there is only one.
    connection: Option<String>,
}

fn number(v: Option<&str>) -> u64 {
    v.and_then(|v| v.parse().ok()).unwrap_or(0)
}

fn text(name: &str, p: &ColumnProfile) -> String {
    let sampled = if p.rows >= SAMPLE_ROWS as u64 {
        format!("first {} rows sampled", p.rows)
    } else {
        format!("{} rows", p.rows)
    };
    let percent = |n: u64| (n * 100 + p.rows / 2).checked_div(p.rows).unwrap_or(0);
    let mut parts = vec![
        sampled,
        format!("NULL {} ({}%)", p.nulls, percent(p.nulls)),
        format!("{} distinct", p.distinct),
    ];
    if let (Some(min), Some(max)) = (&p.min, &p.max) {
        parts.push(format!("range {min} – {max}"));
    }
    if !p.top.is_empty() {
        let top: Vec<String> = p.top.iter().map(|(v, n)| format!("{v} ({n})")).collect();
        let more = if p.distinct as usize > TOP_VALUES {
            ", …"
        } else {
            ""
        };
        parts.push(format!("values: {}{more}", top.join(", ")));
    }
    format!("{name}: {}", parts.join(" · "))
}

#[async_trait]
impl Tool for ColumnProfileTool {
    fn name(&self) -> &'static str {
        "column_profile"
    }

    fn description(&self) -> &'static str {
        "What a column's values look like, from a sample: how many are NULL, how many \
         distinct, the range, and the most frequent values with their counts. Use the \
         exact values in conditions. Only where the user allows reading values."
    }

    fn input_schema(&self) -> Value {
        schema_of::<Input>()
    }

    async fn call(&self, ctx: &Ctx, value: Value) -> Result<ToolOutput, ToolError> {
        let Input {
            table,
            column,
            connection,
        } = input(value)?;
        let conn = ctx.conn(connection.as_deref())?;
        if !conn.values.statistics() {
            return Err(ToolError::NotAllowed(
                "reading from the rows is off for this database; the user can allow \
                 statistics or values in the AI menu of the title bar"
                    .into(),
            ));
        }
        // Statistics only: counts, no values (no range, no top values).
        let read_values = conn.values.values();
        let ix = resolve_table(&conn.schema, &table)?;
        let meta = conn
            .schema
            .columns(ix)
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(&column))
            .ok_or_else(|| {
                ToolError::NotFound(format!("column {column}; describe_table lists them"))
            })?;
        let name = format!("{}.{}", conn.schema.sql_name(ix), meta.name);
        if is_secret(&meta.name) {
            return Err(ToolError::NotAllowed(format!(
                "{name} looks like a secret; its values are never read"
            )));
        }
        let key = (meta.schema.clone(), meta.table.clone(), meta.name.clone());
        // A cached profile of counts only doesn't do where values may be read.
        let cached = ctx
            .knowledge()
            .profile(&conn.user, &key)
            .filter(|p| p.values || !read_values);
        let profile = match cached {
            Some(p) if read_values => p,
            Some(p) => ColumnProfile {
                min: None,
                max: None,
                top: Vec::new(),
                values: false,
                ..p
            },
            None => {
                let d = conn.schema.dialect();
                let table_sql = conn.schema.sql_name(ix);
                let range = read_values
                    && matches!(
                        ColumnKind::from_type(&meta.data_type),
                        ColumnKind::Number | ColumnKind::Date | ColumnKind::DateTime
                    );
                let counts = read(
                    ctx,
                    conn.conn,
                    counts_sql(d, &table_sql, &meta.name, range),
                    1,
                )
                .await?;
                let set = counts
                    .results
                    .last()
                    .ok_or_else(|| ToolError::Failed("no result".into()))?;
                let (rows, non_null) = (number(set.cell(0, 0)), number(set.cell(0, 1)));
                // Types without equality or order (json, …) have no top values.
                let top = if !read_values {
                    Vec::new()
                } else {
                    match read(
                        ctx,
                        conn.conn,
                        top_values_sql(d, &table_sql, &meta.name),
                        TOP_VALUES,
                    )
                    .await
                    {
                        Ok(out) => out
                            .results
                            .last()
                            .map(|t| {
                                (0..t.row_count())
                                    .map(|r| {
                                        (
                                            clip(t.cell(r, 0).unwrap_or_default()),
                                            number(t.cell(r, 1)),
                                        )
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        Err(_) => Vec::new(),
                    }
                };
                let profile = ColumnProfile {
                    rows,
                    nulls: rows.saturating_sub(non_null),
                    distinct: number(set.cell(0, 2)),
                    min: range.then(|| set.cell(0, 3).map(clip)).flatten(),
                    max: range.then(|| set.cell(0, 4).map(clip)).flatten(),
                    top,
                    values: read_values,
                };
                ctx.knowledge()
                    .set_profile(&conn.user, key, profile.clone());
                profile
            }
        };
        Ok(ToolOutput::new(&profile, text(&name, &profile)))
    }
}
