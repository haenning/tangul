//! What AI tools see of a database's rows, as set in its own settings file
//! (`databases/<id>.json`): which columns are redacted (values replaced) or hidden
//! (left out of results), and whether queries may use them beyond reading them.
//!
//! ```json
//! {
//!   "ai": {
//!     "type": "blacklist",
//!     "redact_columns": ["name", "orders.adr*"],
//!     "redact_tables": ["billing_*"],
//!     "hide_columns": ["ssn"],
//!     "hide_tables": ["audit_*"],
//!     "show_columns": [],
//!     "show_tables": [],
//!     "block_queries": "none"
//!   }
//! }
//! ```
//!
//! Patterns ignore case and know one wildcard, `*`. A column pattern is `column`
//! (in every table), `table.column` or `schema.table.column`; a table pattern is
//! `table` or `schema.table`.

use serde::{Deserialize, Serialize};

/// The settings of one database (its own file).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DatabaseSettings {
    pub ai: AiColumns,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiColumns {
    /// Blacklist: the redact and hide lists apply, the rest is visible. Whitelist:
    /// only the show lists are visible (the redact lists still apply to them), the
    /// rest is hidden.
    #[serde(rename = "type")]
    pub kind: ListKind,
    pub redact_columns: Vec<String>,
    pub redact_tables: Vec<String>,
    pub hide_columns: Vec<String>,
    pub hide_tables: Vec<String>,
    pub show_columns: Vec<String>,
    pub show_tables: Vec<String>,
    pub block_queries: BlockQueries,
}

impl AiColumns {
    /// Whether every column is visible as it is.
    pub fn is_open(&self) -> bool {
        self.kind == ListKind::Blacklist
            && [
                &self.redact_columns,
                &self.redact_tables,
                &self.hide_columns,
                &self.hide_tables,
            ]
            .iter()
            .all(|l| l.is_empty())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListKind {
    #[default]
    Blacklist,
    Whitelist,
}

/// Which columns queries may only read (select), never filter, join, sort or
/// group by: those leak values bit by bit (`WHERE email LIKE 'a%'`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockQueries {
    #[default]
    None,
    Redacted,
    RedactedAndHidden,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_file_and_defaults_what_is_missing() {
        let s: DatabaseSettings = serde_json::from_str(
            r#"{"ai": {"type": "whitelist", "show_tables": ["orders"],
                "block_queries": "redacted_and_hidden"}}"#,
        )
        .unwrap();
        assert_eq!(s.ai.kind, ListKind::Whitelist);
        assert_eq!(s.ai.show_tables, ["orders"]);
        assert_eq!(s.ai.block_queries, BlockQueries::RedactedAndHidden);
        assert!(s.ai.redact_columns.is_empty());
        assert!(!s.ai.is_open());
        let empty: DatabaseSettings = serde_json::from_str("{}").unwrap();
        assert!(empty.ai.is_open());
        let json = serde_json::to_string(&empty).unwrap();
        assert!(json.contains(r#""type":"blacklist""#), "{json}");
        assert!(json.contains(r#""block_queries":"none""#), "{json}");
    }
}
