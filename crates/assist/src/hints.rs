//! Values the editor offers for a column: the ones past queries compared it with,
//! and the most frequent ones in its data (the editor's quick look,
//! [`crate::profile::quick_values_sql`]).

use std::collections::{HashMap, HashSet};

use tangul_core::usage::HistoryPattern;

use crate::analyze::{analyze, significant, statements};
use crate::complete::{Op, column_meta, comparison, resolve_scope};
use crate::dialect::Dialect;
use crate::lexer::TokenKind;
use crate::profile::is_secret;
use crate::schema::SchemaIndex;
use crate::usage::column_key;

/// Longest literal taken from history.
const LITERAL_CHARS: usize = 64;
/// Values kept per column from history.
const PER_COLUMN: usize = 20;

/// A value for a column, as SQL (`'paid'`, `42`).
#[derive(Debug, Clone, PartialEq)]
pub struct Hint {
    pub literal: String,
    /// Compared with `LIKE` (a pattern, `'%x%'`), not `=`.
    pub like: bool,
    /// How often it was used, or how frequent it is in the data.
    pub weight: f64,
    /// From the data rather than from history.
    pub from_data: bool,
}

/// Values per column (keyed by [`column_key`]), most used first: history's, then
/// the data's.
#[derive(Debug, Clone, Default)]
pub struct ValueHints {
    by_column: HashMap<String, Vec<Hint>>,
    /// Columns whose data was looked at, also those without a value.
    looked_at: HashSet<String>,
}

impl ValueHints {
    /// Literals compared with columns in `history` (`col = 'x'`, `col IN (1, 2)`,
    /// `col LIKE '%x%'`), counted by how often their query ran. Columns that look
    /// like secrets are left out.
    pub fn from_history(history: &[HistoryPattern], schema: &SchemaIndex) -> Self {
        let mut counts: HashMap<(String, String, bool), f64> = HashMap::new();
        for p in history {
            for (s, e) in statements(&p.sql) {
                let sql = &p.sql[s..e];
                let tokens = significant(sql);
                let scope = analyze(sql).tables;
                let resolved = resolve_scope(&scope, schema);
                for (i, t) in tokens.iter().enumerate() {
                    let literal = t.text(sql);
                    let quoted = t.kind == TokenKind::String && literal.starts_with('\'');
                    if !(quoted || t.kind == TokenKind::Number)
                        || literal.chars().count() > LITERAL_CHARS
                    {
                        continue;
                    }
                    let Some((column, op)) = comparison(sql, &tokens[..i]) else {
                        continue;
                    };
                    let Some(meta) = column_meta(schema, &scope, &resolved, &column) else {
                        continue;
                    };
                    if is_secret(&meta.name) {
                        continue;
                    }
                    let key = column_key(&meta.schema, &meta.table, &meta.name);
                    *counts
                        .entry((key, literal.to_string(), op == Op::Like))
                        .or_default() += f64::from(p.runs.max(1));
                }
            }
        }
        let mut by_column: HashMap<String, Vec<Hint>> = HashMap::new();
        for ((key, literal, like), weight) in counts {
            by_column.entry(key).or_default().push(Hint {
                literal,
                like,
                weight,
                from_data: false,
            });
        }
        for hints in by_column.values_mut() {
            hints.sort_by(|a, b| {
                b.weight
                    .total_cmp(&a.weight)
                    .then_with(|| a.literal.cmp(&b.literal))
            });
            hints.truncate(PER_COLUMN);
        }
        Self {
            by_column,
            looked_at: HashSet::new(),
        }
    }

    /// The most frequent `values` in the data of the column `key` (text, most
    /// frequent first), after the ones from history.
    pub fn add_data(&mut self, key: String, values: &[String], d: &dyn Dialect) {
        self.looked_at.insert(key.clone());
        let hints = self.by_column.entry(key).or_default();
        for (n, v) in values.iter().enumerate() {
            let literal = d.string(v);
            if hints.iter().any(|h| h.literal == literal) {
                continue;
            }
            hints.push(Hint {
                literal,
                like: false,
                weight: (values.len() - n) as f64,
                from_data: true,
            });
        }
    }

    pub fn for_column(&self, key: &str) -> &[Hint] {
        self.by_column.get(key).map_or(&[], Vec::as_slice)
    }

    /// Whether values from the data of `key` are known (or known to be none).
    pub fn has_data(&self, key: &str) -> bool {
        self.looked_at.contains(key)
    }
}

#[cfg(test)]
mod tests {
    use tangul_core::DriverKind;

    use super::*;
    use crate::dialect::dialect_for;
    use crate::schema::tests::sample;

    fn literals(h: &ValueHints, column: &str) -> Vec<(String, bool)> {
        h.for_column(&column_key("app", "users", column))
            .iter()
            .map(|h| (h.literal.clone(), h.like))
            .collect()
    }

    #[test]
    fn values_compared_in_history_by_runs() {
        let history = [
            HistoryPattern {
                sql: "select * from app.users where name = 'ann' and id in (1, 2)".into(),
                runs: 2,
            },
            HistoryPattern {
                sql: "select * from app.users u where u.name = 'bob'; \
                      select * from app.users where email like '%@x.io'"
                    .into(),
                runs: 5,
            },
            HistoryPattern {
                sql: "select * from app.users where name <> 'ann' limit 10".into(),
                runs: 1,
            },
        ];
        let h = ValueHints::from_history(&history, &sample());
        assert_eq!(
            literals(&h, "name"),
            [("'bob'".into(), false), ("'ann'".into(), false)]
        );
        assert_eq!(
            literals(&h, "id"),
            [("1".into(), false), ("2".into(), false)]
        );
        assert_eq!(literals(&h, "email"), [("'%@x.io'".into(), true)]);
        assert!(!h.has_data(&column_key("app", "users", "name")));
    }

    #[test]
    fn secrets_and_unknown_columns_are_left_out() {
        let history = [HistoryPattern {
            sql: "select * from app.users where password_hash = 'x' or nope = 'y'".into(),
            runs: 1,
        }];
        let h = ValueHints::from_history(&history, &sample());
        assert!(h.by_column.is_empty());
    }

    #[test]
    fn data_after_history() {
        let history = [HistoryPattern {
            sql: "select * from app.users where name = 'bob'".into(),
            runs: 1,
        }];
        let mut h = ValueHints::from_history(&history, &sample());
        let key = column_key("app", "users", "name");
        let pg = dialect_for(DriverKind::Postgres);
        h.add_data(
            key.clone(),
            &["ann".into(), "bob".into(), "o'neil".into()],
            pg,
        );
        assert_eq!(
            h.for_column(&key)
                .iter()
                .map(|h| (h.literal.as_str(), h.from_data))
                .collect::<Vec<_>>(),
            [("'bob'", false), ("'ann'", true), ("'o''neil'", true)]
        );
        assert!(h.has_data(&key));
        // Looked at without a value: known all the same.
        let key = column_key("app", "users", "email");
        h.add_data(key.clone(), &[], pg);
        assert!(h.has_data(&key) && h.for_column(&key).is_empty());
    }
}
