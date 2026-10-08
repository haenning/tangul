//! What AI tools see of a database's rows, as set in its own settings file
//! (`databases/<id>.json`): which columns are redacted (values replaced) or hidden
//! (left out of results), and whether queries may use them beyond reading them.
//!
//! ```json
//! {
//!   "ai": {
//!     "type": "blacklist",
//!     "redact": ["*.name", "orders.adr*", "billing_*"],
//!     "hide": ["*.ssn", "audit_*"],
//!     "show": [],
//!     "block_queries": "none"
//!   }
//! }
//! ```
//!
//! Patterns ignore case and know one wildcard, `*`. With a dot they name columns:
//! `table.column` (`*.email`: in every table) or `schema.table.column`; without one,
//! tables (all their columns). A table in a schema: `schema.table.*`.

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
    /// Blacklist: `redact` and `hide` apply, the rest is visible. Whitelist: only
    /// `show` is visible (`redact` still applies to it), the rest is hidden.
    #[serde(rename = "type")]
    pub kind: ListKind,
    pub redact: Vec<String>,
    pub hide: Vec<String>,
    pub show: Vec<String>,
    pub block_queries: BlockQueries,
}

impl AiColumns {
    /// Whether every column is visible as it is.
    pub fn is_open(&self) -> bool {
        self.kind == ListKind::Blacklist && self.redact.is_empty() && self.hide.is_empty()
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
            r#"{"ai": {"type": "whitelist", "show": ["orders"],
                "block_queries": "redacted_and_hidden"}}"#,
        )
        .unwrap();
        assert_eq!(s.ai.kind, ListKind::Whitelist);
        assert_eq!(s.ai.show, ["orders"]);
        assert_eq!(s.ai.block_queries, BlockQueries::RedactedAndHidden);
        assert!(s.ai.redact.is_empty());
        assert!(!s.ai.is_open());
        let empty: DatabaseSettings = serde_json::from_str("{}").unwrap();
        assert!(empty.ai.is_open());
        let json = serde_json::to_string(&empty).unwrap();
        assert!(json.contains(r#""type":"blacklist""#), "{json}");
        assert!(json.contains(r#""block_queries":"none""#), "{json}");
    }
}
