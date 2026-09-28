//! Lightweight structural analysis of a statement: which tables it reads (with
//! aliases), which columns it touches and which column pairs it joins on.
//! No full parser: the rules follow the clause keywords, which also works for
//! half-typed SQL.

use crate::lexer::{Lexing, Token, TokenKind, tokenize_with, unquote};

/// Words that end a table reference or can never be an alias.
pub(crate) const RESERVED: &[&str] = &[
    "all",
    "and",
    "any",
    "as",
    "asc",
    "between",
    "by",
    "case",
    "cross",
    "delete",
    "desc",
    "distinct",
    "else",
    "end",
    "except",
    "exists",
    "fetch",
    "for",
    "from",
    "full",
    "group",
    "having",
    "ilike",
    "in",
    "inner",
    "insert",
    "intersect",
    "into",
    "is",
    "join",
    "lateral",
    "left",
    "like",
    "limit",
    "natural",
    "not",
    "null",
    "offset",
    "on",
    "or",
    "order",
    "outer",
    "over",
    "partition",
    "returning",
    "right",
    "select",
    "set",
    "some",
    "straight_join",
    "table",
    "then",
    "union",
    "update",
    "using",
    "values",
    "when",
    "where",
    "window",
    "with",
];

pub(crate) fn is_reserved(word: &str) -> bool {
    RESERVED.iter().any(|k| word.eq_ignore_ascii_case(k))
}

/// Keywords after which a table name follows.
pub(crate) const TABLE_KEYWORDS: &[&str] = &["from", "join", "update", "into", "table"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRef {
    pub schema: Option<String>,
    pub name: String,
    pub alias: Option<String>,
    /// Byte range of the reference (name and alias) in the source.
    pub start: usize,
    pub end: usize,
}

impl TableRef {
    /// Whether `qualifier` (from `qualifier.column`) refers to this table.
    pub fn answers_to(&self, qualifier: &str) -> bool {
        match &self.alias {
            Some(a) => a.eq_ignore_ascii_case(qualifier),
            None => self.name.eq_ignore_ascii_case(qualifier),
        }
    }

    /// The name to qualify columns with: alias, else table name.
    pub fn handle(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnRef {
    /// Index into [`Analysis::tables`] when the qualifier resolved.
    pub table: Option<usize>,
    pub qualifier: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Analysis {
    pub tables: Vec<TableRef>,
    pub columns: Vec<ColumnRef>,
    /// `a.x = b.y` between two different tables (index into `columns`).
    pub joins: Vec<(usize, usize)>,
}

/// Byte ranges of the statements in `sql`, split at top-level `;`.
pub fn statements(sql: &str) -> Vec<(usize, usize)> {
    statements_with(sql, Lexing::default())
}

/// [`statements`] with a database's lexical rules.
pub fn statements_with(sql: &str, lexing: Lexing) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for t in tokenize_with(sql, lexing) {
        if t.is_punct(sql, ';') {
            out.push((start, t.start));
            start = t.end;
        }
    }
    out.push((start, sql.len()));
    out
}

/// Significant tokens only.
pub(crate) fn significant(sql: &str) -> Vec<Token> {
    significant_with(sql, Lexing::default())
}

/// [`significant`] with a database's lexical rules.
pub(crate) fn significant_with(sql: &str, lexing: Lexing) -> Vec<Token> {
    tokenize_with(sql, lexing)
        .into_iter()
        .filter(|t| !t.is_trivia())
        .collect()
}

/// Parses `[schema.]name [[AS] alias]` starting at `tokens[i]`. Returns the reference
/// and the index after it.
/// `column_list` allows `name (` as in `INSERT INTO t (a, b)`.
pub(crate) fn parse_table_ref(
    sql: &str,
    tokens: &[Token],
    mut i: usize,
    column_list: bool,
) -> Option<(TableRef, usize)> {
    let first = tokens.get(i).filter(|t| t.is_ident())?;
    if first.kind == TokenKind::Word && is_reserved(first.text(sql)) {
        return None;
    }
    let start = first.start;
    let mut end = first.end;
    let mut schema = None;
    let mut name = unquote(first.text(sql));
    i += 1;
    if tokens.get(i).is_some_and(|t| t.is_punct(sql, '.'))
        && let Some(t) = tokens.get(i + 1).filter(|t| t.is_ident())
    {
        schema = Some(name);
        name = unquote(t.text(sql));
        end = t.end;
        i += 2;
    }
    // A function call (`FROM generate_series(...)`) is not a table.
    if tokens.get(i).is_some_and(|t| t.is_punct(sql, '(')) {
        if column_list {
            return Some((
                TableRef {
                    schema,
                    name,
                    alias: None,
                    start,
                    end,
                },
                i,
            ));
        }
        return None;
    }
    let mut alias = None;
    let explicit = tokens.get(i).is_some_and(|t| t.is_word(sql, "as"));
    let j = if explicit { i + 1 } else { i };
    if let Some(t) = tokens.get(j).filter(|t| t.is_ident())
        && !(t.kind == TokenKind::Word && is_reserved(t.text(sql)))
    {
        alias = Some(unquote(t.text(sql)));
        end = t.end;
        i = j + 1;
    }
    Some((
        TableRef {
            schema,
            name,
            alias,
            start,
            end,
        },
        i,
    ))
}

pub fn analyze(sql: &str) -> Analysis {
    let tokens = significant(sql);
    let mut a = Analysis::default();

    // Table references: after FROM/JOIN/UPDATE/INTO/TABLE, and after commas in a
    // FROM list. Parentheses (subqueries) save and restore the FROM-list state.
    let mut in_from = false;
    let mut stack: Vec<bool> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        if t.is_punct(sql, '(') {
            stack.push(in_from);
            in_from = false;
        } else if t.is_punct(sql, ')') {
            in_from = stack.pop().unwrap_or(false);
        }
        let after_table_kw = TABLE_KEYWORDS.iter().any(|k| t.is_word(sql, k));
        let after_comma = t.is_punct(sql, ',') && in_from;
        if t.kind == TokenKind::Word && !after_table_kw && is_reserved(t.text(sql)) {
            // Any other clause keyword ends a FROM list (JOIN … ON keeps it going).
            in_from = false;
        }
        if after_table_kw || after_comma {
            if t.is_word(sql, "from") || t.is_word(sql, "join") {
                in_from = true;
            }
            let column_list = t.is_word(sql, "into");
            if let Some((r, next)) = parse_table_ref(sql, &tokens, i + 1, column_list) {
                a.tables.push(r);
                i = next;
                continue;
            }
        }
        i += 1;
    }

    // Column references: `q.col` anywhere, excluding the table references themselves.
    let in_table_ref =
        |pos: usize, a: &Analysis| a.tables.iter().any(|r| pos >= r.start && pos < r.end);
    let mut i = 0;
    while i + 2 < tokens.len() {
        let (q, dot, c) = (tokens[i], tokens[i + 1], tokens[i + 2]);
        if q.is_ident() && dot.is_punct(sql, '.') && c.is_ident() && !in_table_ref(q.start, &a) {
            // `schema.table.column`: use the last two parts.
            let qualifier = unquote(q.text(sql));
            let table = a.tables.iter().position(|r| r.answers_to(&qualifier));
            a.columns.push(ColumnRef {
                table,
                qualifier: Some(qualifier),
                name: unquote(c.text(sql)),
            });
            i += 3;
            continue;
        }
        i += 1;
    }

    // Joins: `x.a = y.b` between different resolved tables.
    let mut i = 0;
    while i + 6 < tokens.len() {
        let w = &tokens[i..i + 7];
        let colref =
            |q: &Token, d: &Token, c: &Token| q.is_ident() && d.is_punct(sql, '.') && c.is_ident();
        if colref(&w[0], &w[1], &w[2]) && w[3].is_punct(sql, '=') && colref(&w[4], &w[5], &w[6]) {
            let find = |q: &Token, c: &Token| {
                let (q, c) = (unquote(q.text(sql)), unquote(c.text(sql)));
                a.columns.iter().position(|r| {
                    r.qualifier
                        .as_deref()
                        .is_some_and(|x| x.eq_ignore_ascii_case(&q))
                        && r.name.eq_ignore_ascii_case(&c)
                })
            };
            if let (Some(l), Some(r)) = (find(&w[0], &w[2]), find(&w[4], &w[6]))
                && a.columns[l].table.is_some()
                && a.columns[r].table.is_some()
                && a.columns[l].table != a.columns[r].table
            {
                a.joins.push((l, r));
            }
            i += 7;
            continue;
        }
        i += 1;
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables(sql: &str) -> Vec<(Option<String>, String, Option<String>)> {
        analyze(sql)
            .tables
            .into_iter()
            .map(|t| (t.schema, t.name, t.alias))
            .collect()
    }

    fn t(
        schema: Option<&str>,
        name: &str,
        alias: Option<&str>,
    ) -> (Option<String>, String, Option<String>) {
        (schema.map(Into::into), name.into(), alias.map(Into::into))
    }

    #[test]
    fn table_refs() {
        assert_eq!(
            tables("SELECT * FROM app.users u JOIN orders AS o ON o.user_id = u.id WHERE u.id = 1"),
            [
                t(Some("app"), "users", Some("u")),
                t(None, "orders", Some("o"))
            ]
        );
        assert_eq!(
            tables("select * from a, b x, \"C\" where 1=1"),
            [
                t(None, "a", None),
                t(None, "b", Some("x")),
                t(None, "C", None)
            ]
        );
        assert_eq!(
            tables("select * from users order by id"),
            [t(None, "users", None)]
        );
        assert_eq!(tables("update users set a = 1"), [t(None, "users", None)]);
        assert_eq!(
            tables("insert into logs (a) values (1)"),
            [t(None, "logs", None)]
        );
        assert_eq!(tables("select * from generate_series(1, 3) g"), []);
        assert_eq!(
            tables("select * from (select * from inner_t) s, outer_t"),
            [t(None, "inner_t", None), t(None, "outer_t", None)]
        );
    }

    #[test]
    fn columns_and_joins() {
        let a = analyze("SELECT u.name, o.total FROM users u JOIN orders o ON o.user_id = u.id");
        let cols: Vec<_> = a
            .columns
            .iter()
            .map(|c| (c.table, c.name.as_str()))
            .collect();
        assert_eq!(
            cols,
            [
                (Some(0), "name"),
                (Some(1), "total"),
                (Some(1), "user_id"),
                (Some(0), "id")
            ]
        );
        assert_eq!(a.joins, [(2, 3)]);
    }

    #[test]
    fn splits_statements() {
        assert_eq!(statements("select 1; select ';'"), [(0, 8), (9, 20)]);
    }
}
