//! Shared types between UI, engine and drivers. No I/O in here.

mod profile;
mod result;

pub use profile::*;
pub use result::*;

use std::fmt;

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
#[derive(Debug, Clone)]
pub struct TableInfo {
    pub schema: String,
    pub name: String,
    pub kind: TableKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TableKind {
    Table,
    View,
}
