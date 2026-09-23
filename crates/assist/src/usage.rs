//! What the user actually works with, learned from successfully executed queries,
//! used to rank completions and suggestions ("frecency": frequent and recent wins).

use std::collections::{HashMap, HashSet};

use crate::analyze::{analyze, is_reserved, significant, statements};
use crate::lexer::{TokenKind, unquote};
use crate::schema::SchemaIndex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageKind {
    Table,
    Column,
    Join,
}

impl UsageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UsageKind::Table => "table",
            UsageKind::Column => "column",
            UsageKind::Join => "join",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "table" => UsageKind::Table,
            "column" => UsageKind::Column,
            "join" => UsageKind::Join,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UsageStat {
    pub count: u32,
    /// Unix seconds.
    pub last_used: i64,
}

/// Key parts are separated by the unit separator; join sides by the record separator.
const SEP: char = '\u{1f}';
const SIDE: char = '\u{1e}';

pub fn table_key(schema: &str, table: &str) -> String {
    format!("{}{SEP}{}", schema.to_lowercase(), table.to_lowercase())
}

pub fn column_key(schema: &str, table: &str, column: &str) -> String {
    format!("{}{SEP}{}", table_key(schema, table), column.to_lowercase())
}

/// Order-independent key of a join condition between two columns.
pub fn join_key(a: (&str, &str, &str), b: (&str, &str, &str)) -> String {
    let (a, b) = (column_key(a.0, a.1, a.2), column_key(b.0, b.1, b.2));
    if a <= b {
        format!("{a}{SIDE}{b}")
    } else {
        format!("{b}{SIDE}{a}")
    }
}

/// Splits a join key into its two `(schema, table, column)` sides (lowercase).
pub fn parse_join_key(key: &str) -> Option<[(String, String, String); 2]> {
    let (a, b) = key.split_once(SIDE)?;
    let side = |s: &str| {
        let mut it = s.split(SEP).map(String::from);
        Some((it.next()?, it.next()?, it.next()?))
    };
    Some([side(a)?, side(b)?])
}

#[derive(Debug, Clone, Default)]
pub struct Usage {
    stats: HashMap<(UsageKind, String), UsageStat>,
    now: i64,
}

impl Usage {
    /// `now` in unix seconds; scores decay relative to it.
    pub fn new(now: i64) -> Self {
        Self {
            stats: HashMap::new(),
            now,
        }
    }

    pub fn insert(&mut self, kind: UsageKind, key: String, stat: UsageStat) {
        self.stats.insert((kind, key), stat);
    }

    /// Counts one use now.
    pub fn record(&mut self, kind: UsageKind, key: String) {
        let now = self.now;
        let stat = self.stats.entry((kind, key)).or_insert(UsageStat {
            count: 0,
            last_used: now,
        });
        stat.count += 1;
        stat.last_used = now;
    }

    /// Uses, discounted by age: halves after two weeks without use.
    pub fn score(&self, kind: UsageKind, key: &str) -> f64 {
        self.stats
            .get(&(kind, key.to_string()))
            .map_or(0.0, |s| self.decay(s))
    }

    fn decay(&self, s: &UsageStat) -> f64 {
        let age_days = (self.now - s.last_used).max(0) as f64 / 86_400.0;
        f64::from(s.count) / (1.0 + age_days / 14.0)
    }

    /// Learned join conditions with their scores.
    pub fn joins(&self) -> impl Iterator<Item = (&str, f64)> {
        self.stats
            .iter()
            .filter(|((k, _), _)| *k == UsageKind::Join)
            .map(|((_, key), s)| (key.as_str(), self.decay(s)))
    }
}

/// The tables, columns and joins a successfully executed script used, resolved
/// against the schema (unknown names are ignored). Each key appears once.
pub fn learn(sql: &str, schema: &SchemaIndex) -> Vec<(UsageKind, String)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |kind: UsageKind, key: String, out: &mut Vec<_>| {
        if seen.insert((kind, key.clone())) {
            out.push((kind, key));
        }
    };
    for (start, end) in statements(sql) {
        let stmt = &sql[start..end];
        let a = analyze(stmt);
        let resolved: Vec<Option<usize>> = a
            .tables
            .iter()
            .map(|t| schema.resolve(t.schema.as_deref(), &t.name))
            .collect();
        for ix in resolved.iter().flatten() {
            let t = schema.table(*ix);
            push(UsageKind::Table, table_key(&t.schema, &t.name), &mut out);
        }
        let column_of = |table: Option<usize>, name: &str| {
            let ix = resolved[table?]?;
            let t = schema.table(ix);
            let col = schema
                .columns(ix)
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(name))?;
            Some((t.schema.as_str(), t.name.as_str(), col.name.as_str()))
        };
        for c in &a.columns {
            if let Some((s, t, c)) = column_of(c.table, &c.name) {
                push(UsageKind::Column, column_key(s, t, c), &mut out);
            }
        }
        // With a single table, bare words that are its columns count as well.
        if let [Some(ix)] = resolved[..] {
            let t = schema.table(ix);
            let tokens = significant(stmt);
            for (i, tok) in tokens.iter().enumerate() {
                let is_call = tokens.get(i + 1).is_some_and(|n| n.is_punct(stmt, '('));
                let after_dot = i > 0 && tokens[i - 1].is_punct(stmt, '.');
                let text = tok.text(stmt);
                if !tok.is_ident()
                    || is_call
                    || after_dot
                    || (tok.kind == TokenKind::Word && is_reserved(text))
                {
                    continue;
                }
                let name = unquote(text);
                if let Some(col) = schema
                    .columns(ix)
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(&name))
                {
                    push(
                        UsageKind::Column,
                        column_key(&t.schema, &t.name, &col.name),
                        &mut out,
                    );
                }
            }
        }
        for &(l, r) in &a.joins {
            let (l, r) = (&a.columns[l], &a.columns[r]);
            if let (Some(l), Some(r)) = (column_of(l.table, &l.name), column_of(r.table, &r.name)) {
                push(UsageKind::Join, join_key(l, r), &mut out);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::sample;

    #[test]
    fn learns_tables_columns_joins() {
        let s = sample();
        let learned = learn(
            "SELECT u.email FROM app.users u JOIN app.orders o ON o.user_id = u.id; select total from app.orders",
            &s,
        );
        let has = |k: UsageKind, key: String| learned.contains(&(k, key));
        assert!(has(UsageKind::Table, table_key("app", "users")));
        assert!(has(UsageKind::Column, column_key("app", "users", "email")));
        assert!(has(UsageKind::Column, column_key("app", "orders", "total")));
        let join = join_key(("app", "orders", "user_id"), ("app", "users", "id"));
        assert!(has(UsageKind::Join, join.clone()));
        let [a, b] = parse_join_key(&join).unwrap();
        assert_eq!((a.1.as_str(), b.1.as_str()), ("orders", "users"));
        assert!(!learned.iter().any(|(_, k)| k.contains("nope")));
    }

    #[test]
    fn recency_decays() {
        let mut u = Usage::new(100 * 86_400);
        u.insert(
            UsageKind::Table,
            "old".into(),
            UsageStat {
                count: 10,
                last_used: 72 * 86_400,
            },
        );
        u.insert(
            UsageKind::Table,
            "new".into(),
            UsageStat {
                count: 4,
                last_used: 100 * 86_400,
            },
        );
        assert!(u.score(UsageKind::Table, "new") > u.score(UsageKind::Table, "old"));
        assert_eq!(u.score(UsageKind::Table, "missing"), 0.0);
    }
}
