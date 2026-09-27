//! The tools, one module each, and what they share.

mod catalog;
mod connections;
mod describe;
mod joins;
mod profile;
mod propose;
mod query;
mod search;
mod slice;

use tangul_assist::lexer::unquote;
use tangul_assist::schema::SchemaIndex;

use crate::tool::{Tool, ToolError};

pub(crate) fn all() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(connections::ListConnections),
        Box::new(search::SearchSchema),
        Box::new(describe::DescribeTable),
        Box::new(joins::FindJoinPath),
        Box::new(slice::SchemaSlice),
        Box::new(profile::ColumnProfileTool),
        Box::new(catalog::ListEnums),
        Box::new(catalog::ViewDefinition),
        Box::new(query::RunReadonly),
        Box::new(propose::ProposeSql),
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
