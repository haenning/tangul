//! Where the columns of a result come from: the table column behind each one, found
//! with the schema loaded for completion, so the grid can mark keys and knows the
//! column types without asking the server again.
//!
//! The last statement's select list is read item by item (`*`, `t.*`, `[t.]col
//! [[AS] alias]`, or any other expression). When the items account for exactly the
//! result's columns they are matched by position, which also tells apart two `id`s
//! of a join. Otherwise a column is matched by name, if exactly one table of the
//! FROM clause has it.

use crate::analyze::{TableRef, analyze, is_reserved, statements};
use crate::lexer::{Token, TokenKind, tokenize, unquote};
use crate::related::numeric_type;
use crate::schema::SchemaIndex;

/// What the schema says about a result column that is a plain table column.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnFacts {
    /// Short table name.
    pub table: String,
    pub column: String,
    /// Type as the server spells it.
    pub data_type: String,
    pub numeric: bool,
    pub primary_key: bool,
    /// `users.id` when the column is part of a foreign key.
    pub references: Option<String>,
}

/// One item of a select list.
#[derive(Debug, PartialEq)]
enum Item {
    /// `*`, or `t.*` (qualifier).
    Star(Option<String>),
    /// `[t.]col`, optionally renamed.
    Column {
        qualifier: Option<String>,
        name: String,
    },
    /// Anything else: one column of unknown origin.
    Expr,
}

/// The facts per result column (`columns` are the result's column names); `None`
/// for expressions and columns that can't be told apart.
pub fn column_facts(
    sql: &str,
    columns: &[String],
    schema: &SchemaIndex,
) -> Vec<Option<ColumnFacts>> {
    let unknown = vec![None; columns.len()];
    let Some(stmt) = statements(sql)
        .into_iter()
        .map(|(s, e)| &sql[s..e])
        .rfind(|s| !s.trim().is_empty())
    else {
        return unknown;
    };
    let Some(query) = main_query(stmt) else {
        return unknown;
    };
    let refs = analyze(stmt).tables;
    let from: Vec<Option<(&TableRef, usize)>> = query
        .from
        .iter()
        .map(|&start| {
            let r = refs.iter().find(|r| r.start == start)?;
            Some((r, schema.resolve(r.schema.as_deref(), &r.name)?))
        })
        .collect();
    // A subquery, function or unknown table in FROM: unqualified names may be its.
    let complete = !query.opaque && from.iter().all(Option::is_some);
    let from: Vec<(&TableRef, usize)> = from.into_iter().flatten().collect();
    let by_qualifier = |q: &str| {
        from.iter()
            .find(|(r, _)| r.answers_to(q))
            .map(|&(_, ix)| ix)
            .or_else(|| {
                // Qualified by a table that is only in a subquery or join condition.
                refs.iter()
                    .find(|r| r.answers_to(q))
                    .and_then(|r| schema.resolve(r.schema.as_deref(), &r.name))
            })
    };
    let facts = |table: usize, name: &str| facts_for(schema, table, name);
    let unqualified = |name: &str| {
        if !complete {
            return None;
        }
        let mut found = from.iter().filter(|(_, ix)| {
            schema
                .columns(*ix)
                .iter()
                .any(|c| c.name.eq_ignore_ascii_case(name))
        });
        match (found.next(), found.next()) {
            (Some(&(_, ix)), None) => Some(ix),
            _ => None,
        }
    };

    // Expand the items to one entry per output column, if every star can be.
    let mut expanded: Option<Vec<Option<ColumnFacts>>> = Some(Vec::new());
    for item in &query.items {
        let Some(out) = expanded.as_mut() else { break };
        match item {
            Item::Star(qualifier) => {
                let tables: Option<Vec<usize>> = match qualifier {
                    Some(q) => by_qualifier(q).map(|ix| vec![ix]),
                    None => complete.then(|| from.iter().map(|&(_, ix)| ix).collect()),
                };
                match tables {
                    Some(tables) => {
                        for ix in tables {
                            for c in schema.columns(ix) {
                                out.push(facts(ix, &c.name));
                            }
                        }
                    }
                    None => expanded = None,
                }
            }
            Item::Column { qualifier, name } => {
                let table = match qualifier {
                    Some(q) => by_qualifier(q),
                    None => unqualified(name),
                };
                out.push(table.and_then(|ix| facts(ix, name)));
            }
            Item::Expr => out.push(None),
        }
    }
    if let Some(expanded) = expanded.filter(|e| e.len() == columns.len()) {
        return expanded;
    }
    columns
        .iter()
        .map(|name| unqualified(name).and_then(|ix| facts(ix, name)))
        .collect()
}

fn facts_for(schema: &SchemaIndex, table: usize, name: &str) -> Option<ColumnFacts> {
    let col = schema
        .columns(table)
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(name))?;
    let t = schema.table(table);
    let references = schema.foreign_keys().iter().find_map(|fk| {
        if !(fk.schema.eq_ignore_ascii_case(&t.schema) && fk.table.eq_ignore_ascii_case(&t.name)) {
            return None;
        }
        let i = fk.columns.iter().position(|c| c == &col.name)?;
        Some(format!("{}.{}", fk.ref_table, fk.ref_columns.get(i)?))
    });
    Some(ColumnFacts {
        table: t.name.clone(),
        column: col.name.clone(),
        data_type: col.data_type.clone(),
        numeric: numeric_type(&col.data_type),
        primary_key: col.primary_key,
        references,
    })
}

/// The outermost SELECT of a statement.
struct MainQuery {
    items: Vec<Item>,
    /// Start offsets of the table references in its FROM clause.
    from: Vec<usize>,
    /// FROM has an item that is not a plain table (subquery, function call, LATERAL).
    opaque: bool,
}

fn main_query(sql: &str) -> Option<MainQuery> {
    // Top-level tokens only; parenthesized parts (CTEs, subqueries, calls) are kept as
    // one `(` token so items and FROM entries can still be told apart.
    let mut top: Vec<Token> = Vec::new();
    let mut depth = 0usize;
    for t in tokenize(sql).into_iter().filter(|t| !t.is_trivia()) {
        if t.is_punct(sql, '(') {
            if depth == 0 {
                top.push(t);
            }
            depth += 1;
        } else if t.is_punct(sql, ')') {
            depth = depth.saturating_sub(1);
        } else if depth == 0 {
            top.push(t);
        }
    }
    let select = top.iter().position(|t| t.is_word(sql, "select"))?;
    let mut i = select + 1;
    while top
        .get(i)
        .is_some_and(|t| t.is_word(sql, "distinct") || t.is_word(sql, "all"))
    {
        i += 1;
        // Postgres `DISTINCT ON (…)`.
        if top.get(i).is_some_and(|t| t.is_word(sql, "on")) {
            i += 2;
        }
    }
    let list_end = top[i..]
        .iter()
        .position(|t| {
            t.kind == TokenKind::Word
                && [
                    "from", "into", "where", "group", "order", "limit", "union", "window",
                ]
                .iter()
                .any(|k| t.is_word(sql, k))
        })
        .map_or(top.len(), |p| i + p);
    let items = top[i..list_end]
        .split(|t| t.is_punct(sql, ','))
        .map(|item| parse_item(sql, item))
        .collect();

    let mut from = Vec::new();
    let mut opaque = false;
    if top.get(list_end).is_some_and(|t| t.is_word(sql, "from")) {
        let mut expect = true;
        for t in &top[list_end + 1..] {
            let word = |k: &str| t.is_word(sql, k);
            if [
                "where",
                "group",
                "having",
                "order",
                "limit",
                "union",
                "intersect",
                "except",
                "window",
                "offset",
                "fetch",
                "for",
            ]
            .iter()
            .any(|k| word(k))
            {
                break;
            }
            if expect {
                if word("lateral") || word("only") {
                    opaque |= word("lateral");
                    continue;
                }
                if t.is_ident() && !(t.kind == TokenKind::Word && is_reserved(t.text(sql))) {
                    from.push(t.start);
                } else {
                    opaque = true;
                }
                expect = false;
            } else if word("join") || t.is_punct(sql, ',') {
                expect = true;
            } else if t.is_punct(sql, '(') && !from.is_empty() {
                // `name(` right after an entry: a function call, not a table.
                let prev = top.iter().rposition(|p| p.end <= t.start);
                if prev.is_some_and(|p| top[p].start == *from.last().unwrap_or(&usize::MAX)) {
                    from.pop();
                    opaque = true;
                }
            }
        }
    }
    Some(MainQuery {
        items,
        from,
        opaque,
    })
}

fn parse_item(sql: &str, tokens: &[Token]) -> Item {
    let ident =
        |t: &Token| t.is_ident() && !(t.kind == TokenKind::Word && is_reserved(t.text(sql)));
    let dot = |t: &Token| t.is_punct(sql, '.');
    let star = |t: &Token| t.is_punct(sql, '*');
    // Leave off an alias: `x AS a` or `x a`.
    let body = match tokens {
        [rest @ .., as_kw, alias] if as_kw.is_word(sql, "as") && alias.is_ident() => rest,
        [rest @ .., last, alias] if ident(alias) && !dot(last) => &tokens[..rest.len() + 1],
        _ => tokens,
    };
    match body {
        [s] if star(s) => Item::Star(None),
        [q, d, s] if ident(q) && dot(d) && star(s) => Item::Star(Some(unquote(q.text(sql)))),
        [c] if ident(c) => column(None, c, sql),
        [q, d, c] if ident(q) && dot(d) && ident(c) => column(Some(q), c, sql),
        // `schema.table.column`
        [_, d1, q, d2, c] if dot(d1) && ident(q) && dot(d2) && ident(c) => column(Some(q), c, sql),
        _ => Item::Expr,
    }
}

fn column(qualifier: Option<&Token>, name: &Token, sql: &str) -> Item {
    Item::Column {
        qualifier: qualifier.map(|q| unquote(q.text(sql))),
        name: unquote(name.text(sql)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::sample;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// `table.column` per result column, `-` for unknown.
    fn origins(sql: &str, cols: &[&str]) -> Vec<String> {
        column_facts(sql, &names(cols), &sample())
            .into_iter()
            .map(|f| f.map_or("-".into(), |f| format!("{}.{}", f.table, f.column)))
            .collect()
    }

    #[test]
    fn single_table() {
        assert_eq!(
            origins(
                "select id, total, 1 as one from orders",
                &["id", "total", "one"]
            ),
            ["orders.id", "orders.total", "-"]
        );
        let f = column_facts(
            "select * from orders",
            &names(&["id", "user_id", "total"]),
            &sample(),
        );
        let f: Vec<_> = f.into_iter().map(Option::unwrap).collect();
        assert!(f[0].primary_key && f[0].numeric && f[0].references.is_none());
        assert_eq!(f[1].references.as_deref(), Some("users.id"));
        assert!(!f[2].primary_key && !f[2].numeric);
    }

    #[test]
    fn joins_by_position() {
        let sql = "select * from orders o join users u on u.id = o.user_id";
        assert_eq!(
            origins(sql, &["id", "user_id", "total", "id", "email", "name"]),
            [
                "orders.id",
                "orders.user_id",
                "orders.total",
                "users.id",
                "users.email",
                "users.name"
            ]
        );
        assert_eq!(
            origins(
                "select o.id, u.id as uid, name, count(*) from orders o, users u group by 1",
                &["id", "uid", "name", "count"]
            ),
            ["orders.id", "users.id", "users.name", "-"]
        );
        assert_eq!(
            origins(
                "select u.*, o.total from app.users u join orders o on true",
                &["id", "email", "name", "total"]
            ),
            ["users.id", "users.email", "users.name", "orders.total"]
        );
        // `id` is in both tables.
        assert_eq!(origins("select id from orders, users", &["id"]), ["-"]);
    }

    #[test]
    fn unknown_sources() {
        // A subquery in FROM could have any column.
        assert_eq!(
            origins("select id from (select 1 as id) x, orders", &["id"]),
            ["-"]
        );
        assert_eq!(
            origins(
                "with t as (select 1) select total, email from orders, t",
                &["total", "email"]
            ),
            ["-", "-"]
        );
        assert_eq!(
            origins("select * from generate_series(1, 3)", &["generate_series"]),
            ["-"]
        );
        // A mismatch in count (the schema is stale) falls back to names.
        assert_eq!(
            origins("select * from orders", &["id", "total"]),
            ["orders.id", "orders.total"]
        );
        assert_eq!(origins("", &["x"]), ["-"]);
    }
}
