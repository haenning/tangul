//! Shared types between UI, engine and drivers. No I/O in here.

pub mod chart;
mod profile;
mod result;
pub mod usage;

pub use profile::*;
pub use result::*;

use std::fmt;

use serde::{Deserialize, Serialize};

/// Identifies an open connection (one per database user at the moment).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnId(pub u64);

/// Identifies a query tab. Used to pin sessions to tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

/// Identifies a single query execution (used for cancel and event routing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QueryId(pub u64);

impl fmt::Display for ConnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "conn#{}", self.0)
    }
}

/// One entry of the schema explorer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableInfo {
    pub schema: String,
    pub name: String,
    pub kind: TableKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TableKind {
    Table,
    View,
}

/// A column of a table or view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnMeta {
    pub schema: String,
    pub table: String,
    pub name: String,
    /// Type as the server spells it (`integer`, `varchar(255)`, …).
    pub data_type: String,
    pub nullable: bool,
    pub primary_key: bool,
}

/// A foreign key; `columns[i]` references `ref_columns[i]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForeignKey {
    pub name: String,
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

/// Everything the schema explorer and SQL assistance know about a connection.
/// Tables are ordered by schema, then name; columns by table, then position.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SchemaInfo {
    pub tables: Vec<TableInfo>,
    pub columns: Vec<ColumnMeta>,
    pub foreign_keys: Vec<ForeignKey>,
}
