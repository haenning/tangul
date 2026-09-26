//! Query suggestions for a table: what the user ran against it before (from history),
//! plus queries generated from its keys and foreign keys.

use crate::analyze::analyze;
use crate::schema::SchemaIndex;

#[derive(Debug, Clone, PartialEq)]
pub struct QuerySuggestion {
    pub title: String,
    pub sql: String,
    /// From history rather than generated.
    pub learned: bool,
}

pub use tangul_core::usage::HistoryPattern;

const MAX_LEARNED: usize = 5;

pub fn suggest_for_table(
    table: usize,
    schema: &SchemaIndex,
    history: &[HistoryPattern],
) -> Vec<QuerySuggestion> {
    let mut out = learned(table, schema, history);
    out.extend(generated(table, schema));
    out
}

fn learned(table: usize, schema: &SchemaIndex, history: &[HistoryPattern]) -> Vec<QuerySuggestion> {
    let mut hits: Vec<&HistoryPattern> = history
        .iter()
        .filter(|p| {
            analyze(&p.sql)
                .tables
                .iter()
                .any(|t| schema.resolve(t.schema.as_deref(), &t.name) == Some(table))
        })
        .collect();
    hits.sort_by_key(|p| std::cmp::Reverse(p.runs));
    hits.into_iter()
        .take(MAX_LEARNED)
        .map(|p| QuerySuggestion {
            title: format!("{} · run {}×", one_line(&p.sql, 60), p.runs),
            sql: p.sql.clone(),
            learned: true,
        })
        .collect()
}

fn generated(table: usize, s: &SchemaIndex) -> Vec<QuerySuggestion> {
    let t = s.table(table);
    let name = s.sql_name(table);
    let pk: Vec<String> = s
        .columns(table)
        .iter()
        .filter(|c| c.primary_key)
        .map(|c| s.quote(&c.name))
        .collect();
    let mut out = vec![QuerySuggestion {
        title: "All rows".into(),
        sql: format!("SELECT *\nFROM {name}\nLIMIT 100"),
        learned: false,
    }];
    if !pk.is_empty() {
        out.push(QuerySuggestion {
            title: "Latest rows".into(),
            sql: format!(
                "SELECT *\nFROM {name}\nORDER BY {} DESC\nLIMIT 100",
                pk.join(" DESC, ")
            ),
            learned: false,
        });
    }
    out.push(QuerySuggestion {
        title: "Row count".into(),
        sql: format!("SELECT COUNT(*)\nFROM {name}"),
        learned: false,
    });

    let alias = |n: &str, taken: &str| {
        let a: String = n
            .split('_')
            .filter_map(|p| p.chars().next())
            .flat_map(char::to_lowercase)
            .filter(char::is_ascii_alphabetic)
            .collect();
        let a = if a.is_empty() { "t".into() } else { a };
        if a == taken { format!("{a}2") } else { a }
    };
    let on = |l: &str, lc: &[String], r: &str, rc: &[String]| {
        lc.iter()
            .zip(rc)
            .map(|(a, b)| format!("{l}.{} = {r}.{}", s.quote(a), s.quote(b)))
            .collect::<Vec<_>>()
            .join(" AND ")
    };

    for fk in s.foreign_keys() {
        let (Some(child), Some(parent)) = (
            s.resolve(Some(&fk.schema), &fk.table),
            s.resolve(Some(&fk.ref_schema), &fk.ref_table),
        ) else {
            continue;
        };
        if child == table {
            // Outgoing: rows with the referenced row alongside.
            let a = alias(&t.name, "");
            let b = alias(&fk.ref_table, &a);
            out.push(QuerySuggestion {
                title: format!("With {}", fk.ref_table),
                sql: format!(
                    "SELECT {a}.*, {b}.*\nFROM {name} {a}\nJOIN {} {b} ON {}\nLIMIT 100",
                    s.sql_name(parent),
                    on(&a, &fk.columns, &b, &fk.ref_columns),
                ),
                learned: false,
            });
        }
        if parent == table && !pk.is_empty() {
            // Incoming: how many referencing rows each row has.
            let a = alias(&t.name, "");
            let b = alias(&fk.table, &a);
            let count = s.quote(&format!("{}_count", fk.table));
            out.push(QuerySuggestion {
                title: format!("{} per {}", fk.table, t.name),
                sql: format!(
                    "SELECT {a}.*, COUNT({b}.{}) AS {count}\nFROM {name} {a}\nLEFT JOIN {} {b} ON {}\nGROUP BY {}\nORDER BY {count} DESC\nLIMIT 100",
                    s.quote(&fk.columns[0]),
                    s.sql_name(child),
                    on(&b, &fk.columns, &a, &fk.ref_columns),
                    pk.iter().map(|c| format!("{a}.{c}")).collect::<Vec<_>>().join(", "),
                ),
                learned: false,
            });
        }
    }
    out
}

/// Whitespace collapsed and shortened with an ellipsis.
pub fn one_line(sql: &str, max: usize) -> String {
    let flat = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut s: String = flat.chars().take(max).collect();
    s.push('…');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::sample;

    #[test]
    fn generated_from_keys() {
        let s = sample();
        let users = s.resolve(None, "users").unwrap();
        let titles: Vec<_> = suggest_for_table(users, &s, &[])
            .into_iter()
            .map(|q| q.title)
            .collect();
        assert_eq!(
            titles,
            ["All rows", "Latest rows", "Row count", "orders per users"]
        );

        let orders = s.resolve(None, "orders").unwrap();
        let with = suggest_for_table(orders, &s, &[])
            .into_iter()
            .find(|q| q.title == "With users")
            .unwrap();
        assert_eq!(
            with.sql,
            "SELECT o.*, u.*\nFROM app.orders o\nJOIN app.users u ON o.user_id = u.id\nLIMIT 100"
        );
    }

    #[test]
    fn learned_first_by_runs() {
        let s = sample();
        let users = s.resolve(None, "users").unwrap();
        let history = [
            HistoryPattern {
                sql: "select * from app.orders".into(),
                runs: 9,
            },
            HistoryPattern {
                sql: "select email from app.users where id = 3".into(),
                runs: 2,
            },
            HistoryPattern {
                sql: "select name from app.users".into(),
                runs: 5,
            },
        ];
        let got = suggest_for_table(users, &s, &history);
        assert!(got[0].learned && got[0].sql == "select name from app.users");
        assert!(got[1].learned && got[1].title.ends_with("run 2×"));
        assert!(!got[2].learned);
    }
}
