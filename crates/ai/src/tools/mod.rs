//! The tools, one module each, and what they share.

mod catalog;
mod connections;
mod describe;
mod query;
mod search;

use tangul_assist::lexer::unquote;
use tangul_assist::schema::SchemaIndex;
use tangul_core::{ColumnMeta, EnumInfo, SchemaInfo};

use crate::tool::{Tool, ToolError};

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(connections::ListConnections),
        Box::new(search::SearchSchema),
        Box::new(describe::DescribeTable),
        Box::new(catalog::ListEnums),
        Box::new(catalog::ViewDefinition),
        Box::new(query::RunReadonly),
    ]
}

/// `orders`, `app.orders` or `"My Table"` → the table's index in `schema`.
pub(crate) fn resolve_table(schema: &SchemaIndex, name: &str) -> Result<usize, ToolError> {
    let name = name.trim();
    let split = name
        .split_once('.')
        .and_then(|(s, t)| schema.resolve(Some(&unquote(s)), &unquote(t)));
    split
        .or_else(|| schema.resolve(None, &unquote(name)))
        .ok_or_else(|| {
            ToolError::NotFound(format!(
                "table {name}; search_schema finds tables by name or comment"
            ))
        })
}

/// The values of `column`'s enum type, if it has one.
pub(crate) fn enum_of<'a>(info: &'a SchemaInfo, column: &ColumnMeta) -> Option<&'a EnumInfo> {
    let ty = column.enum_type.as_deref()?;
    info.enums.iter().find(|e| {
        // Postgres names the type `schema.type`; MySQL `table.column` in the schema.
        ty == format!("{}.{}", e.schema, e.name) || (e.schema == column.schema && e.name == ty)
    })
}
