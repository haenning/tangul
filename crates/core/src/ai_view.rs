//! What AI tools see of a database's rows, as set in its own settings file
//! (`databases/<id>.json`): which columns are redacted (values replaced) or hidden
//! (left out of results), and whether queries may use them beyond reading them.
//!
//! ```json
//! {
//!   "ai": {
//!     "content_blocking": {
//!       "redact": ["*.name", "orders.adr*", "billing_*"],
//!       "hide": ["*.ssn", "audit_*"],
//!       "block_queries": ["redacted"]
//!     }
//!   }
//! }
//! ```
//!
//! Patterns ignore case and know one wildcard, `*`. With a dot they name columns:
//! `table.column` (`*.email`: in every table) or `schema.table.column`; without one,
//! tables (all their columns). A table in a schema: `schema.table.*`. A leading `!`
//! makes an exception, and within a list the last pattern that matches decides:
//! `"hide": ["*", "!orders.*.*", "orders.payments.card_no"]` hides everything but
//! the schema `orders`, and in it `payments.card_no`.

use serde::{Deserialize, Serialize};

/// The settings of one database (its own file).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DatabaseSettings {
    pub ai: DatabaseAi,
}

/// What AI tools may do with this database. Unknown keys here and in
/// [`AiColumns`] are an error, so a misspelt or outdated rule hides everything
/// instead of being ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseAi {
    pub content_blocking: AiColumns,
}

/// Unknown keys are an error: a misspelt list must not leave columns open.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AiColumns {
    /// What matches is redacted, unless hidden too (the stricter wins).
    pub redact: Vec<String>,
    pub hide: Vec<String>,
    /// Columns in these modes may only be selected (empty: used anywhere).
    pub block_queries: Vec<Blocked>,
}

impl AiColumns {
    /// Every column hidden.
    pub fn closed() -> Self {
        Self {
            hide: vec!["*".into()],
            ..Self::default()
        }
    }

    /// Whether every column is visible as it is: no pattern but exceptions.
    pub fn is_open(&self) -> bool {
        let none = |l: &[String]| l.iter().all(|p| p.trim().starts_with('!'));
        none(&self.redact) && none(&self.hide)
    }
}

/// Columns queries may only read (select), never filter, join, sort or group by:
/// those leak values bit by bit (`WHERE email LIKE 'a%'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Blocked {
    Redacted,
    Hidden,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_file_and_defaults_what_is_missing() {
        let s: DatabaseSettings = serde_json::from_str(
            r#"{"ai": {"content_blocking": {"hide": ["*", "!orders.*.*"],
                "block_queries": ["redacted", "hidden"]}}}"#,
        )
        .unwrap();
        let c = &s.ai.content_blocking;
        assert_eq!(c.hide, ["*", "!orders.*.*"]);
        assert_eq!(c.block_queries, [Blocked::Redacted, Blocked::Hidden]);
        assert!(c.redact.is_empty());
        assert!(!c.is_open());
        let empty: DatabaseSettings = serde_json::from_str("{}").unwrap();
        assert!(empty.ai.content_blocking.is_open());
        let json = serde_json::to_string(&empty).unwrap();
        assert!(json.contains(r#""hide":[]"#), "{json}");
        assert!(json.contains(r#""block_queries":[]"#), "{json}");
    }

    #[test]
    fn only_exceptions_hide_nothing() {
        let c = AiColumns {
            hide: vec![" !orders".into()],
            ..AiColumns::default()
        };
        assert!(c.is_open());
        assert!(!AiColumns::closed().is_open());
    }

    #[test]
    fn unknown_or_outdated_keys_fail_instead_of_opening() {
        for text in [
            r#"{"ai": {"type": "blacklist", "redact": ["*.email"]}}"#,
            r#"{"ai": {"content_blocking": {"hdie": ["*"]}}}"#,
        ] {
            assert!(
                serde_json::from_str::<DatabaseSettings>(text).is_err(),
                "{text}"
            );
        }
        assert!(serde_json::from_str::<DatabaseSettings>(r#"{"later": 1}"#).is_ok());
    }

    #[test]
    fn only_exceptions_hide_nothing_and_unknown_keys_fail() {
        let c = AiColumns {
            hide: vec![" !orders".into()],
            ..AiColumns::default()
        };
        assert!(c.is_open());
        assert!(!AiColumns::closed().is_open());
        let typo = r#"{"ai": {"content_blocking": {"hdie": ["*"]}}}"#;
        assert!(serde_json::from_str::<DatabaseSettings>(typo).is_err());
    }
}
