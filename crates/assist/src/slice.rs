//! Finding the part of a schema a question is about: tables and columns whose names,
//! comments or enum values match its words, plus their direct neighbours in the join
//! graph. What an assistant needs to write the query, instead of the whole schema.

use std::collections::HashMap;

use crate::join_graph::JoinGraph;
use crate::schema::SchemaIndex;
use crate::usage::{Usage, UsageKind, table_key};

/// A table or column that matches, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub table: usize,
    /// Index into the table's columns.
    pub column: Option<usize>,
    /// E.g. `name`, `comment: "Registered customers"`, `value 'paid'`.
    pub why: String,
    pub score: u32,
}

/// Values seen in columns (sampled, where the user allows it), by (schema, table,
/// column) as the catalog spells them.
pub type KnownValues = HashMap<(String, String, String), Vec<String>>;

/// Words that say nothing about where data is.
const STOP_WORDS: &[&str] = &[
    "a", "all", "an", "and", "are", "by", "each", "for", "from", "get", "give", "how", "in", "is",
    "last", "list", "many", "me", "much", "of", "on", "or", "per", "show", "that", "the", "their",
    "to", "what", "which", "who", "with", "der", "die", "das", "und", "wie", "viele", "welche",
    "von", "mit", "pro", "alle", "zeig",
];

/// The words of `text`, lower case.
pub fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The words of a question worth looking up.
fn terms(question: &str) -> Vec<String> {
    let mut terms: Vec<String> = words(question)
        .into_iter()
        .filter(|w| w.len() > 1 && !STOP_WORDS.contains(&w.as_str()))
        .collect();
    terms.dedup();
    terms
}

fn singular(term: &str) -> Option<&str> {
    term.strip_suffix('s').filter(|s| s.len() > 2)
}

/// How well `term` fits a name: exact, prefix, a word of it, part of it.
fn name_score(name: &str, term: &str) -> u32 {
    let name = name.to_lowercase();
    let singular = singular(term);
    let is = |t: &str| name == t;
    if is(term) || singular.is_some_and(is) {
        10
    } else if name.starts_with(term) || singular.is_some_and(|s| name.starts_with(s)) {
        6
    } else if words(&name)
        .iter()
        .any(|w| w == term || Some(w.as_str()) == singular)
    {
        5
    } else if name.contains(term) {
        3
    } else {
        0
    }
}

/// A word of the comment: 3 for the exact word, 2 for its singular.
fn comment_score(comment: Option<&str>, term: &str) -> u32 {
    let singular = singular(term);
    comment.map_or(0, |c| {
        let w = words(c);
        if w.iter().any(|w| w == term) {
            3
        } else if w.iter().any(|w| Some(w.as_str()) == singular) {
            2
        } else {
            0
        }
    })
}

/// Tables and columns matching the words of `query`, best first (a table before its
/// columns on a tie). Enum values count, and `known` sampled values.
pub fn matches(schema: &SchemaIndex, query: &str, known: &KnownValues) -> Vec<Match> {
    let terms = words(query);
    let mut out = Vec::new();
    for (ix, table) in schema.tables().iter().enumerate() {
        let (mut score, mut why) = (0, Vec::new());
        for t in &terms {
            let n = name_score(&table.name, t);
            let c = comment_score(table.comment.as_deref(), t);
            if n > 0 {
                why.push("name".to_string());
            } else if c > 0 {
                why.push(format!(
                    "comment: \"{}\"",
                    table.comment.as_deref().unwrap_or_default()
                ));
            }
            score += n.max(c);
        }
        if score > 0 {
            why.dedup();
            out.push(Match {
                table: ix,
                column: None,
                why: why.join(", "),
                score: score + 1,
            });
        }
        for (cix, column) in schema.columns(ix).iter().enumerate() {
            let key = (
                column.schema.clone(),
                column.table.clone(),
                column.name.clone(),
            );
            let values = schema
                .enum_of(column)
                .map(|e| &e.values)
                .or_else(|| known.get(&key));
            let (mut score, mut why) = (0, Vec::new());
            for t in &terms {
                let value = values
                    .and_then(|vs| vs.iter().find(|v| v.to_lowercase() == *t))
                    .map_or((0, String::new()), |v| (8, format!("value '{v}'")));
                let comment = format!(
                    "comment: \"{}\"",
                    column.comment.as_deref().unwrap_or_default()
                );
                let (best, reason) = [
                    (name_score(&column.name, t), "name".to_string()),
                    value,
                    (comment_score(column.comment.as_deref(), t), comment),
                ]
                .into_iter()
                .max_by_key(|(s, _)| *s)
                .expect("three candidates");
                if best > 0 {
                    why.push(reason);
                }
                score += best;
            }
            if score > 0 {
                why.dedup();
                out.push(Match {
                    table: ix,
                    column: Some(cix),
                    why: why.join(", "),
                    score,
                });
            }
        }
    }
    out.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| schema.sql_name(a.table).cmp(&schema.sql_name(b.table)))
            .then_with(|| a.column.cmp(&b.column))
    });
    out
}

/// A table of a slice and why it is in it.
#[derive(Debug, Clone, PartialEq)]
pub struct SliceTable {
    pub table: usize,
    /// What matched, or `joins <table>` for a neighbour.
    pub why: Vec<String>,
}

/// The tables a question is about: the best matching ones (up to `max`), then their
/// direct neighbours in the join graph (up to half as many again), so the joins
/// between them are known. Learned usage breaks ties.
pub fn slice(
    schema: &SchemaIndex,
    usage: &Usage,
    known: &KnownValues,
    question: &str,
    max: usize,
) -> Vec<SliceTable> {
    let terms = terms(question);
    let found = matches(schema, &terms.join(" "), known);
    // Per table: its score (own + best columns) and the reasons.
    let mut tables: Vec<(usize, u32, Vec<String>)> = Vec::new();
    for m in &found {
        let why = match m.column {
            Some(c) => format!("{}: {}", schema.columns(m.table)[c].name, m.why),
            None => m.why.clone(),
        };
        match tables.iter_mut().find(|t| t.0 == m.table) {
            Some(t) => {
                t.1 += m.score;
                t.2.push(why);
            }
            None => tables.push((m.table, m.score, vec![why])),
        }
    }
    let used = |ix: usize| {
        let t = schema.table(ix);
        usage.score(UsageKind::Table, &table_key(&t.schema, &t.name))
    };
    tables.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| used(b.0).total_cmp(&used(a.0)))
            .then_with(|| a.0.cmp(&b.0))
    });
    tables.truncate(max);
    let mut out: Vec<SliceTable> = tables
        .into_iter()
        .map(|(table, _, why)| SliceTable { table, why })
        .collect();
    let graph = JoinGraph::new(schema, usage);
    let chosen: Vec<usize> = out.iter().map(|t| t.table).collect();
    let mut extra: Vec<(usize, usize)> = Vec::new();
    for &t in &chosen {
        for step in graph.neighbours(t) {
            if !chosen.contains(&step.to) && !extra.iter().any(|(n, _)| *n == step.to) {
                extra.push((step.to, t));
            }
        }
    }
    extra.sort_by(|a, b| used(b.0).total_cmp(&used(a.0)).then_with(|| a.0.cmp(&b.0)));
    out.extend(
        extra
            .into_iter()
            .take(max.div_ceil(2))
            .map(|(table, of)| SliceTable {
                table,
                why: vec![format!("joins {}", schema.sql_name(of))],
            }),
    );
    out
}

#[cfg(test)]
mod tests {
    use tangul_core::{ColumnMeta, DriverKind, EnumInfo, ForeignKey, SchemaInfo, TableInfo};

    use super::*;
    use crate::dialect::dialect_for;

    fn schema() -> SchemaIndex {
        let table = |name: &str, comment: Option<&str>| TableInfo {
            schema: "app".into(),
            name: name.into(),
            comment: comment.map(str::to_string),
            ..Default::default()
        };
        let col = |table: &str, name: &str| ColumnMeta {
            schema: "app".into(),
            table: table.into(),
            name: name.into(),
            data_type: "text".into(),
            ..Default::default()
        };
        let info = SchemaInfo {
            tables: vec![
                table("customers", Some("People who buy")),
                table("invoices", Some("Bills sent to customers")),
                table("orders", None),
                table("warehouses", None),
            ],
            columns: vec![
                col("customers", "id"),
                col("customers", "email"),
                col("invoices", "id"),
                col("invoices", "order_id"),
                col("orders", "id"),
                col("orders", "customer_id"),
                ColumnMeta {
                    enum_type: Some("app.order_status".into()),
                    ..col("orders", "status")
                },
                col("warehouses", "city"),
            ],
            foreign_keys: vec![
                ForeignKey {
                    name: "orders_customer".into(),
                    schema: "app".into(),
                    table: "orders".into(),
                    columns: vec!["customer_id".into()],
                    ref_schema: "app".into(),
                    ref_table: "customers".into(),
                    ref_columns: vec!["id".into()],
                },
                ForeignKey {
                    name: "invoices_order".into(),
                    schema: "app".into(),
                    table: "invoices".into(),
                    columns: vec!["order_id".into()],
                    ref_schema: "app".into(),
                    ref_table: "orders".into(),
                    ref_columns: vec!["id".into()],
                },
            ],
            enums: vec![EnumInfo {
                schema: "app".into(),
                name: "order_status".into(),
                values: vec!["open".into(), "cancelled".into()],
                comment: None,
            }],
            ..Default::default()
        };
        SchemaIndex::new(
            info,
            dialect_for(DriverKind::Postgres),
            Some("public".into()),
        )
    }

    fn names(s: &SchemaIndex, slice: &[SliceTable]) -> Vec<String> {
        slice
            .iter()
            .map(|t| s.table(t.table).name.clone())
            .collect()
    }

    #[test]
    fn a_question_finds_its_tables_and_their_neighbours() {
        let s = schema();
        let got = slice(
            &s,
            &Usage::default(),
            &KnownValues::new(),
            "How many cancelled orders per customer?",
            2,
        );
        // orders (name + a status value) and customers (name); invoices joins orders.
        assert_eq!(names(&s, &got), ["orders", "customers", "invoices"]);
        assert_eq!(
            got[0].why,
            ["name", "status: value 'cancelled'", "customer_id: name"]
        );
        assert_eq!(got[2].why, ["joins app.orders"]);
    }

    #[test]
    fn comments_count_and_stop_words_dont() {
        let s = schema();
        let got = slice(
            &s,
            &Usage::default(),
            &KnownValues::new(),
            "Show me all the bills",
            3,
        );
        assert_eq!(names(&s, &got), ["invoices", "orders"]);
        assert_eq!(got[0].why, ["comment: \"Bills sent to customers\""]);
        assert!(
            slice(
                &s,
                &Usage::default(),
                &KnownValues::new(),
                "show me all of the",
                5
            )
            .is_empty()
        );
    }

    #[test]
    fn sampled_values_count_like_enum_values() {
        let s = schema();
        let none = KnownValues::new();
        assert!(slice(&s, &Usage::default(), &none, "stock in Berlin", 3).is_empty());
        let known = KnownValues::from([(
            (
                "app".to_string(),
                "warehouses".to_string(),
                "city".to_string(),
            ),
            vec!["Berlin".to_string(), "Hamburg".to_string()],
        )]);
        let got = slice(&s, &Usage::default(), &known, "stock in Berlin", 3);
        assert_eq!(names(&s, &got), ["warehouses"]);
        assert_eq!(got[0].why, ["city: value 'Berlin'"]);
    }

    #[test]
    fn usage_breaks_ties() {
        let s = schema();
        // One column each: a tie, in schema order …
        let got = slice(&s, &Usage::default(), &KnownValues::new(), "email city", 2);
        assert_eq!(names(&s, &got)[..2], ["customers", "warehouses"]);
        // … unless the user works with one of them.
        let mut usage = Usage::default();
        usage.record(UsageKind::Table, table_key("app", "warehouses"));
        let got = slice(&s, &usage, &KnownValues::new(), "email city", 2);
        assert_eq!(names(&s, &got)[..2], ["warehouses", "customers"]);
    }
}
