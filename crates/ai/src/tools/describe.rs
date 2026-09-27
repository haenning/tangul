//! `describe_table`: everything known about one table or view.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tangul_assist::join_graph::{JoinGraph, JoinSource};
use tangul_assist::schema::SchemaIndex;
use tangul_assist::usage::Usage;
use tangul_core::{CheckInfo, IndexInfo, TableKind};

use super::resolve_table;
use crate::ctx::Ctx;
use crate::tool::{Tool, ToolError, ToolOutput, input, schema_of};

pub(crate) struct DescribeTable;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Input {
    /// The table or view: `orders` or `schema.orders`.
    table: String,
    /// A name from list_connections; can be left out when there is only one.
    connection: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct Description {
    /// As it is written in SQL on this connection.
    pub name: String,
    pub kind: TableKind,
    pub comment: Option<String>,
    pub row_estimate: Option<u64>,
    pub size_bytes: Option<u64>,
    pub columns: Vec<Column>,
    pub indexes: Vec<IndexInfo>,
    pub checks: Vec<CheckInfo>,
    /// Other tables' columns that reference this one: `schema.table (cols) → (cols)`.
    pub referenced_by: Vec<String>,
    /// Joins the user's queries use that no foreign key declares: `a.x = b.y`.
    pub learned_joins: Vec<String>,
    /// The query of a view.
    pub definition: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct Column {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub primary_key: bool,
    pub identity: bool,
    pub default: Option<String>,
    pub generated: Option<String>,
    pub comment: Option<String>,
    pub enum_values: Option<Vec<String>>,
    /// The column it references (a single-column foreign key): `table.column`.
    pub references: Option<String>,
}

pub(crate) fn describe(schema: &SchemaIndex, usage: &Usage, ix: usize) -> Description {
    let info = schema.info();
    let table = schema.table(ix);
    let is = |s: &str, t: &str| s == table.schema && t == table.name;
    let own_keys: Vec<_> = info
        .foreign_keys
        .iter()
        .filter(|f| is(&f.schema, &f.table))
        .collect();
    let target = |schema_name: &str, name: &str| {
        schema
            .resolve(Some(schema_name), name)
            .map_or_else(|| format!("{schema_name}.{name}"), |i| schema.sql_name(i))
    };
    let columns = schema
        .columns(ix)
        .iter()
        .map(|c| Column {
            name: c.name.clone(),
            data_type: c.data_type.clone(),
            nullable: c.nullable,
            primary_key: c.primary_key,
            identity: c.identity,
            default: c.default.clone(),
            generated: c.generated.clone(),
            comment: c.comment.clone(),
            enum_values: schema.enum_of(c).map(|e| e.values.clone()),
            references: own_keys
                .iter()
                .find(|f| f.columns.len() == 1 && f.columns[0] == c.name)
                .map(|f| {
                    format!(
                        "{}.{}",
                        target(&f.ref_schema, &f.ref_table),
                        f.ref_columns[0]
                    )
                }),
        })
        .collect();
    let referenced_by = info
        .foreign_keys
        .iter()
        .filter(|f| is(&f.ref_schema, &f.ref_table))
        .map(|f| {
            format!(
                "{} ({}) → ({})",
                target(&f.schema, &f.table),
                f.columns.join(", "),
                f.ref_columns.join(", ")
            )
        })
        .collect();
    let learned_joins = JoinGraph::new(schema, usage)
        .neighbours(ix)
        .into_iter()
        .filter(|s| s.source == JoinSource::Learned)
        .map(|s| {
            let (f, t) = (schema.sql_name(s.from), schema.sql_name(s.to));
            s.from_cols
                .iter()
                .zip(&s.to_cols)
                .map(|(a, b)| format!("{f}.{a} = {t}.{b}"))
                .collect::<Vec<_>>()
                .join(" AND ")
        })
        .collect();
    Description {
        name: schema.sql_name(ix),
        learned_joins,
        kind: table.kind,
        comment: table.comment.clone(),
        row_estimate: table.row_estimate,
        size_bytes: table.size_bytes,
        columns,
        indexes: info
            .indexes
            .iter()
            .filter(|i| is(&i.schema, &i.table))
            .cloned()
            .collect(),
        checks: info
            .checks
            .iter()
            .filter(|c| is(&c.schema, &c.table))
            .cloned()
            .collect(),
        referenced_by,
        definition: info
            .views
            .iter()
            .find(|v| is(&v.schema, &v.name))
            .map(|v| v.definition.clone()),
    }
}

/// The compact text a model reads.
pub(crate) fn text(d: &Description) -> String {
    let mut head = vec![d.kind.label().to_string()];
    if let Some(n) = d.row_estimate {
        head.push(format!("~{n} rows"));
    }
    let mut out = format!("{} ({})", d.name, head.join(", "));
    if let Some(c) = &d.comment {
        out.push_str(&format!(" — {c}"));
    }
    out.push_str("\ncolumns:");
    for c in &d.columns {
        let mut line = format!("\n  {} {}", c.name, c.data_type);
        if c.primary_key {
            line.push_str(" pk");
        }
        if c.identity {
            line.push_str(" identity");
        }
        if !c.nullable && !c.primary_key {
            line.push_str(" not null");
        }
        if let Some(r) = &c.references {
            line.push_str(&format!(" → {r}"));
        }
        if let Some(g) = &c.generated {
            line.push_str(&format!(" generated as {g}"));
        } else if let Some(v) = c.default.as_ref().filter(|_| !c.identity) {
            line.push_str(&format!(" default {v}"));
        }
        if let Some(values) = &c.enum_values {
            line.push_str(&format!(" {{{}}}", values.join(", ")));
        }
        if let Some(comment) = &c.comment {
            line.push_str(&format!(" — {comment}"));
        }
        out.push_str(&line);
    }
    let indexes: Vec<_> = d.indexes.iter().filter(|i| !i.primary).collect();
    if !indexes.is_empty() {
        out.push_str("\nindexes:");
        for i in indexes {
            let unique = if i.unique { " unique" } else { "" };
            let partial = i
                .predicate
                .as_ref()
                .map(|p| format!(" where {p}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "\n  {} ({}){unique}{partial}",
                i.name,
                i.columns.join(", ")
            ));
        }
    }
    if !d.checks.is_empty() {
        out.push_str("\nchecks:");
        for c in &d.checks {
            out.push_str(&format!("\n  {}", c.expression));
        }
    }
    if !d.referenced_by.is_empty() {
        out.push_str("\nreferenced by:");
        for r in &d.referenced_by {
            out.push_str(&format!("\n  {r}"));
        }
    }
    if !d.learned_joins.is_empty() {
        out.push_str("\njoined in your queries:");
        for j in &d.learned_joins {
            out.push_str(&format!("\n  {j}"));
        }
    }
    if let Some(def) = &d.definition {
        out.push_str(&format!("\ndefinition:\n  {}", def.replace('\n', "\n  ")));
    }
    out
}

#[async_trait]
impl Tool for DescribeTable {
    fn name(&self) -> &'static str {
        "describe_table"
    }

    fn description(&self) -> &'static str {
        "Everything known about a table or view: columns with types, keys, defaults, \
         enum values and comments; foreign keys both ways; indexes; checks; the row \
         estimate; a view's query. Use it before writing SQL against a table."
    }

    fn input_schema(&self) -> Value {
        schema_of::<Input>()
    }

    async fn call(&self, ctx: &Ctx, value: Value) -> Result<ToolOutput, ToolError> {
        let Input { table, connection } = input(value)?;
        let conn = ctx.conn(connection.as_deref())?;
        let ix = resolve_table(&conn.schema, &table)?;
        let d = describe(&conn.schema, &conn.usage, ix);
        let text = text(&d);
        Ok(ToolOutput::new(&d, text))
    }
}
