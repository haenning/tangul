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

/// A table, view or the like. Fields added later are `serde(default)`, so schemas
/// cached by an older version still load.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TableInfo {
    pub schema: String,
    pub name: String,
    pub kind: TableKind,
    /// The server's comment on it (`COMMENT ON TABLE`, MySQL `COMMENT=`).
    #[serde(default)]
    pub comment: Option<String>,
    /// Rows as the server estimates them (statistics, not a count).
    #[serde(default)]
    pub row_estimate: Option<u64>,
    /// Size on disk, indexes included, where the server tells.
    #[serde(default)]
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TableKind {
    #[default]
    Table,
    View,
    MaterializedView,
    /// The parent of a partitioned table (its partitions are tables of their own).
    Partitioned,
    /// A table of another server (Postgres foreign data wrapper).
    Foreign,
}

impl TableKind {
    /// Defined by a query; shown and completed like a view.
    pub fn is_view(self) -> bool {
        matches!(self, TableKind::View | TableKind::MaterializedView)
    }

    /// The group it is listed under: tables or views.
    pub fn group(self) -> TableKind {
        if self.is_view() {
            TableKind::View
        } else {
            TableKind::Table
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TableKind::Table => "table",
            TableKind::View => "view",
            TableKind::MaterializedView => "materialized view",
            TableKind::Partitioned => "partitioned table",
            TableKind::Foreign => "foreign table",
        }
    }
}

/// A column of a table or view.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ColumnMeta {
    pub schema: String,
    pub table: String,
    pub name: String,
    /// Type as the server spells it (`integer`, `varchar(255)`, …).
    pub data_type: String,
    pub nullable: bool,
    pub primary_key: bool,
    /// The server's comment on it.
    #[serde(default)]
    pub comment: Option<String>,
    /// Default expression as the server spells it (`now()`, `'open'::text`).
    #[serde(default)]
    pub default: Option<String>,
    /// Expression of a generated (computed) column.
    #[serde(default)]
    pub generated: Option<String>,
    /// Filled by the server: identity, serial, auto-increment.
    #[serde(default)]
    pub identity: bool,
    /// The enum type it has, if any (see [`EnumInfo`]; MySQL: the column's own).
    #[serde(default)]
    pub enum_type: Option<String>,
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

/// An index of a table (primary keys included).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IndexInfo {
    pub schema: String,
    pub table: String,
    pub name: String,
    /// Columns, or expressions for an expression index, in index order.
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary: bool,
    /// `WHERE` of a partial index.
    pub predicate: Option<String>,
    /// `btree`, `gin`, `hash`, … where the server tells.
    pub method: Option<String>,
}

/// A check constraint.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CheckInfo {
    pub schema: String,
    pub table: String,
    pub name: String,
    /// The condition, e.g. `total >= 0`.
    pub expression: String,
}

/// An enum type and its values, in their order. MySQL enums belong to a column:
/// their name is `table.column`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EnumInfo {
    pub schema: String,
    pub name: String,
    pub values: Vec<String>,
    pub comment: Option<String>,
}

/// The query behind a view or materialized view.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewDef {
    pub schema: String,
    pub name: String,
    pub definition: String,
}

/// A function or procedure: its signature, not its body.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RoutineInfo {
    pub schema: String,
    pub name: String,
    /// `function`, `procedure`, `aggregate`, …
    pub kind: String,
    /// As the server spells them, e.g. `user_id integer, since date`.
    pub arguments: String,
    /// Return type (functions).
    pub returns: Option<String>,
    pub comment: Option<String>,
}

/// What a catalog reads besides tables, columns and foreign keys.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CatalogExtras {
    pub indexes: Vec<IndexInfo>,
    pub checks: Vec<CheckInfo>,
    pub enums: Vec<EnumInfo>,
    pub views: Vec<ViewDef>,
    pub routines: Vec<RoutineInfo>,
}

/// Everything the schema explorer, SQL assistance and the assistant know about a
/// connection. Tables are ordered by schema, then name; columns by table, then
/// position. Fields added later are `serde(default)` (older caches still load).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SchemaInfo {
    pub tables: Vec<TableInfo>,
    pub columns: Vec<ColumnMeta>,
    pub foreign_keys: Vec<ForeignKey>,
    #[serde(default)]
    pub indexes: Vec<IndexInfo>,
    #[serde(default)]
    pub checks: Vec<CheckInfo>,
    #[serde(default)]
    pub enums: Vec<EnumInfo>,
    #[serde(default)]
    pub views: Vec<ViewDef>,
    #[serde(default)]
    pub routines: Vec<RoutineInfo>,
}

impl SchemaInfo {
    /// Adds what [`CatalogExtras`] has.
    pub fn with_extras(mut self, extras: CatalogExtras) -> Self {
        self.indexes = extras.indexes;
        self.checks = extras.checks;
        self.enums = extras.enums;
        self.views = extras.views;
        self.routines = extras.routines;
        self
    }
}

/// What a sample of a column's values shows (the assistant's value profile).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ColumnProfile {
    /// Rows looked at (the sample; at most the sample size).
    pub rows: u64,
    pub nulls: u64,
    /// Distinct non-NULL values in the sample.
    pub distinct: u64,
    /// Smallest and largest value, for types with an order (numbers, dates, text).
    pub min: Option<String>,
    pub max: Option<String>,
    /// The most frequent values in the sample with how often they occur.
    pub top: Vec<(String, u64)>,
    /// Values were read (the range and top values); `false` for statistics only.
    #[serde(default)]
    pub values: bool,
}
