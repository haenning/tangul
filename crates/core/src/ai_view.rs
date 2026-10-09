//! What AI tools see of a database's rows, as set in its own settings file
//! (`databases/<id>.json`): which columns are redacted (values replaced) or hidden
//! (left out of results), and whether queries may use them beyond reading them.
//!
//! ```json
//! {
//!   "ai": {
//!     "content_blocking": {
//!       "redact": ["..name", "orders.adr_*.", "public.billing_*"],
//!       "hide": ["..ssn", ".audit_*"],
//!       "block_queries": ["redacted"]
//!     }
//!   }
//! }
//! ```
//!
//! A pattern names tables, `schema.table`, or columns, `schema.table.column` (on
//! MySQL the schema is the database). An empty part matches any name: `..id` is
//! the column `id` in every table of every schema, `orders.` every table in the
//! schema `orders`. `*` matches any text within a part (`orders.adr_*`), and case
//! is ignored. A pattern without a dot is an error. A leading `!` makes an
//! exception, and within a list the last pattern that matches decides:
//! `"hide": [".", "!orders.", "orders.payments.card_no"]` hides everything but
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
    #[serde(deserialize_with = "patterns")]
    pub redact: Vec<String>,
    #[serde(deserialize_with = "patterns")]
    pub hide: Vec<String>,
    /// Columns in these modes may only be selected (empty: used anywhere).
    pub block_queries: Vec<Blocked>,
}

impl AiColumns {
    /// Every column hidden.
    pub fn closed() -> Self {
        Self {
            hide: vec![".".into()],
            ..Self::default()
        }
    }

    /// Whether every column is visible as it is: no pattern but exceptions.
    pub fn is_open(&self) -> bool {
        let none = |l: &[String]| l.iter().all(|p| p.trim().starts_with('!'));
        none(&self.redact) && none(&self.hide)
    }
}

/// A list of patterns, each checked: one that can't be read fails the file.
fn patterns<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let list = Vec::<String>::deserialize(d)?;
    for text in &list {
        Pattern::parse(text).map_err(serde::de::Error::custom)?;
    }
    Ok(list)
}

/// One pattern of `redact` or `hide`, its parts in lower case, `*` for an empty one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    /// `!`: an exception to the patterns before it in its list.
    pub except: bool,
    pub schema: String,
    pub table: String,
    /// `None`: all of the table's columns.
    pub column: Option<String>,
}

impl Pattern {
    pub fn parse(text: &str) -> Result<Self, String> {
        let trimmed = text.trim();
        let (except, body) = match trimmed.strip_prefix('!') {
            Some(rest) => (true, rest.trim()),
            None => (false, trimmed),
        };
        let part = |p: &str| match p.trim() {
            "" => "*".to_string(),
            p => p.to_lowercase(),
        };
        let parts: Vec<&str> = body.split('.').collect();
        let (schema, table, column) = match parts.as_slice() {
            [schema, table] => (part(schema), part(table), None),
            [schema, table, column] => (part(schema), part(table), Some(part(column))),
            [_] => {
                let example = match body {
                    "" => String::new(),
                    b if b.contains('*') => {
                        format!(" (\".{b}\": tables named like that in every schema)")
                    }
                    b => format!(" (\".{b}\": the table {b} in every schema)"),
                };
                return Err(format!(
                    "{text:?} has no dot: write \"schema.table\" for tables or \"schema.table.column\" \
                     for columns; an empty part matches any name{example}"
                ));
            }
            _ => {
                return Err(format!(
                    "{text:?} has too many dots: at most \"schema.table.column\""
                ));
            }
        };
        Ok(Self {
            except,
            schema,
            table,
            column,
        })
    }

    pub fn matches(&self, schema: &str, table: &str, column: &str) -> bool {
        glob(&self.schema, schema)
            && glob(&self.table, table)
            && self.column.as_deref().is_none_or(|p| glob(p, column))
    }

    /// Whether it names every column of every table.
    pub fn is_everything(&self) -> bool {
        let any = |p: &str| p.chars().all(|c| c == '*');
        any(&self.schema) && any(&self.table) && self.column.as_deref().is_none_or(any)
    }
}

/// `pattern` (lower case, `*` for any text) against `text`, ignoring case.
pub fn glob(pattern: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    let parts: Vec<&str> = pattern.split('*').collect();
    let [first, middle @ .., last] = parts.as_slice() else {
        return pattern == text;
    };
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
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
            r#"{"ai": {"content_blocking": {"hide": [".", "!orders.."],
                "block_queries": ["redacted", "hidden"]}}}"#,
        )
        .unwrap();
        let c = &s.ai.content_blocking;
        assert_eq!(c.hide, [".", "!orders.."]);
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
            hide: vec![" !orders.".into()],
            ..AiColumns::default()
        };
        assert!(c.is_open());
        assert!(!AiColumns::closed().is_open());
    }

    #[test]
    fn unknown_or_outdated_keys_fail_instead_of_opening() {
        for text in [
            r#"{"ai": {"type": "blacklist", "redact": ["..email"]}}"#,
            r#"{"ai": {"content_blocking": {"hdie": ["."]}}}"#,
            r#"{"ai": {"content_blocking": {"hide": ["users"]}}}"#,
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
            hide: vec![" !orders.".into()],
            ..AiColumns::default()
        };
        assert!(c.is_open());
        assert!(!AiColumns::closed().is_open());
        let typo = r#"{"ai": {"content_blocking": {"hdie": ["."]}}}"#;
        assert!(serde_json::from_str::<DatabaseSettings>(typo).is_err());
    }

    #[test]
    fn parts_go_by_position_and_empty_ones_match_anything() {
        let p = |t: &str| Pattern::parse(t).unwrap();
        assert_eq!(
            p(" !..ID "),
            Pattern {
                except: true,
                schema: "*".into(),
                table: "*".into(),
                column: Some("id".into()),
            }
        );
        let orders = p("orders.");
        assert!(orders.matches("Orders", "payments", "card_no"));
        assert!(!orders.matches("public", "orders", "id"));
        let adr = p("orders.adr_*");
        assert!(adr.matches("orders", "ADR_home", "x"));
        assert!(!adr.matches("orders", "home_adr", "x"));
        let id = p("..id");
        assert!(id.matches("a", "b", "id"));
        assert!(!id.matches("a", "b", "user_id"));
        assert!(p(".").is_everything() && p("!..").is_everything() && p("*.*.*").is_everything());
        assert!(!p("..id").is_everything());
        assert!(glob("a*c*e", "abcde") && !glob("a*c*e", "abdx") && !glob("name", "username"));
    }

    #[test]
    fn a_pattern_without_a_dot_or_with_too_many_is_refused() {
        let e = Pattern::parse("users").unwrap_err();
        assert!(e.contains("no dot") && e.contains("\".users\""), "{e}");
        assert!(
            Pattern::parse("*orders*")
                .unwrap_err()
                .contains("named like that")
        );
        assert!(Pattern::parse("!").is_err() && Pattern::parse("").is_err());
        assert!(Pattern::parse("a.b.c.d").unwrap_err().contains("too many"));
    }
}
