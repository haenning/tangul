//! What kind of token the cursor position expects, from a left-to-right walk over the
//! statement: the current clause (SELECT list, FROM, WHERE, …) and whether an
//! operand, an operator/continuation, a table name or specific keywords come next.
//! Plus the keyword and function vocabulary per dialect.
//!
//! This is a state machine over clause keywords, not a parser: it only needs to be
//! right about the position in front of the cursor, and must cope with unfinished SQL.

use rusdb_core::DriverKind;

use crate::lexer::{Token, TokenKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clause {
    None,
    Select,
    From,
    Join,
    On,
    Using,
    Where,
    GroupBy,
    Having,
    OrderBy,
    Limit,
    Update,
    Set,
    InsertInto,
    InsertColumns,
    Values,
    DeleteFrom,
    Returning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// Start of a statement.
    Statement,
    /// A value: column, function call, literal, `NOT …`. `first` = start of the
    /// SELECT list (where `DISTINCT` and `*` fit).
    Operand {
        first: bool,
    },
    /// Right after a complete operand: an operator or the next clause.
    /// `star` = the operand was `*` (only `FROM` or `,` make sense then).
    AfterOperand {
        star: bool,
    },
    Table,
    /// Right after a table reference: alias, join, next clause.
    AfterTable {
        alias: bool,
    },
    /// Exactly one of these keywords fits (`BY` after `GROUP`, …).
    Words(&'static [&'static str]),
    /// A name the user makes up (alias, CTE name) or a type: nothing to suggest.
    Name,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    pub clause: Clause,
    pub expect: Expect,
}

struct Frame {
    /// Clause to restore at `)`.
    clause: Clause,
    kind: Paren,
    /// A SELECT started inside: a subquery, not a function call.
    subquery: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Paren {
    /// Grouping, function call, `IN (…)`, `VALUES (…)`.
    Expr,
    /// `FROM (SELECT …) alias` or a table function.
    FromSubquery,
    /// `INSERT INTO t (a, b)`.
    InsertColumns,
    /// `WITH name AS (…)`.
    Cte,
}

/// Every word the walker treats as a keyword; anything else is an identifier.
const KEYWORDS: &[&str] = &[
    "all",
    "and",
    "any",
    "as",
    "asc",
    "between",
    "by",
    "case",
    "cross",
    "current_date",
    "current_timestamp",
    "delete",
    "desc",
    "describe",
    "distinct",
    "do",
    "else",
    "end",
    "escape",
    "except",
    "exists",
    "explain",
    "false",
    "fetch",
    "from",
    "full",
    "group",
    "having",
    "ilike",
    "in",
    "inner",
    "insert",
    "intersect",
    "interval",
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
    "nulls",
    "offset",
    "on",
    "or",
    "order",
    "outer",
    "over",
    "partition",
    "regexp",
    "returning",
    "right",
    "rlike",
    "select",
    "set",
    "show",
    "similar",
    "some",
    "straight_join",
    "table",
    "then",
    "true",
    "truncate",
    "union",
    "update",
    "using",
    "values",
    "when",
    "where",
    "with",
];

fn is_keyword(lower: &str) -> bool {
    KEYWORDS.contains(&lower)
}

/// The state in front of the cursor. `tokens` are the statement's significant tokens
/// before the word being typed.
pub fn state_at(sql: &str, tokens: &[Token]) -> State {
    let mut st = State {
        clause: Clause::None,
        expect: Expect::Statement,
    };
    let mut stack: Vec<Frame> = Vec::new();
    let word_at = |i: usize| -> Option<String> {
        tokens
            .get(i)
            .filter(|t| t.kind == TokenKind::Word)
            .map(|t| t.text(sql).to_ascii_lowercase())
    };
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        let lower = t.text(sql).to_ascii_lowercase();
        let prev = i.checked_sub(1).and_then(word_at);
        let prev = prev.as_deref();
        let next_is_paren = tokens.get(i + 1).is_some_and(|n| n.is_punct(sql, '('));

        let keyword = t.kind == TokenKind::Word
            && is_keyword(&lower)
            // `left(...)`, `replace(...)`: function calls, not keywords.
            && !(next_is_paren && matches!(lower.as_str(), "left" | "right"))
            // `NULLS FIRST` is the only place where these are keywords.
            || (matches!(lower.as_str(), "first" | "last") && prev == Some("nulls"));

        if t.is_ident() && !keyword {
            // An identifier, possibly dotted (`s.t`, `u.*`).
            let mut j = i + 1;
            let mut star = false;
            while tokens.get(j).is_some_and(|d| d.is_punct(sql, '.')) {
                match tokens.get(j + 1) {
                    Some(n) if n.is_ident() => j += 2,
                    Some(n) if n.is_punct(sql, '*') => {
                        star = true;
                        j += 2;
                    }
                    _ => {
                        j += 1;
                        break;
                    }
                }
            }
            st.expect = after_ident(st, star);
            i = j;
            continue;
        }

        if keyword {
            if lower == "select"
                && let Some(frame) = stack.last_mut()
            {
                frame.subquery = true;
            }
            let in_call = stack
                .last()
                .is_some_and(|f| f.kind == Paren::Expr && !f.subquery);
            keyword_step(&mut st, &lower, prev, in_call);
        } else {
            match t.kind {
                TokenKind::Number | TokenKind::String | TokenKind::Param => {
                    st.expect = Expect::AfterOperand { star: false };
                }
                TokenKind::Punct => {
                    let c = t.text(sql);
                    match c {
                        "," => st.expect = after_comma(st.clause),
                        "(" => {
                            let kind = match (st.expect, st.clause) {
                                (Expect::AfterTable { .. }, Clause::InsertInto) => {
                                    Paren::InsertColumns
                                }
                                (
                                    Expect::Table | Expect::AfterTable { .. },
                                    Clause::From | Clause::Join,
                                ) => Paren::FromSubquery,
                                (_, Clause::None) if prev == Some("as") => Paren::Cte,
                                _ => Paren::Expr,
                            };
                            stack.push(Frame {
                                clause: st.clause,
                                kind,
                                subquery: false,
                            });
                            if kind == Paren::InsertColumns {
                                st.clause = Clause::InsertColumns;
                            }
                            st.expect = Expect::Operand { first: false };
                        }
                        ")" => {
                            let Some(Frame { clause, kind, .. }) = stack.pop() else {
                                st.expect = Expect::AfterOperand { star: false };
                                i += 1;
                                continue;
                            };
                            st.clause = clause;
                            st.expect = match kind {
                                Paren::Expr => Expect::AfterOperand { star: false },
                                Paren::FromSubquery => Expect::AfterTable { alias: false },
                                Paren::InsertColumns => Expect::Words(&["VALUES", "SELECT"]),
                                Paren::Cte => Expect::Statement,
                            };
                        }
                        "*" => {
                            st.expect = match st.expect {
                                Expect::Operand { .. } => Expect::AfterOperand { star: true },
                                _ => Expect::Operand { first: false },
                            }
                        }
                        ":" if tokens.get(i + 1).is_some_and(|n| n.is_punct(sql, ':')) => {
                            // Postgres cast `x::type`.
                            st.expect = Expect::Name;
                            i += 2;
                            continue;
                        }
                        "=" | "<" | ">" | "!" | "+" | "-" | "/" | "%" | "|" | "^" | "&" | "~" => {
                            st.expect = Expect::Operand { first: false };
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    st
}

fn after_ident(st: State, star: bool) -> Expect {
    match st.expect {
        Expect::Table => Expect::AfterTable { alias: false },
        Expect::AfterTable { .. } => Expect::AfterTable { alias: true },
        Expect::Operand { .. } => Expect::AfterOperand { star },
        Expect::Name => match st.clause {
            Clause::From | Clause::Join | Clause::Update | Clause::DeleteFrom => {
                Expect::AfterTable { alias: true }
            }
            // `WITH name` → `AS`.
            Clause::None => Expect::Words(&["AS"]),
            _ => Expect::AfterOperand { star: false },
        },
        // Implicit alias (`SELECT a b`) or unknown: stay.
        other @ Expect::AfterOperand { .. } => other,
        _ => Expect::Name,
    }
}

fn after_comma(clause: Clause) -> Expect {
    match clause {
        Clause::From | Clause::Join => Expect::Table,
        Clause::None => Expect::Name,
        _ => Expect::Operand { first: false },
    }
}

/// `in_call`: inside a function call's parentheses (`EXTRACT(year FROM d)`).
fn keyword_step(st: &mut State, kw: &str, prev: Option<&str>, in_call: bool) {
    let operand = Expect::Operand { first: false };
    let after = Expect::AfterOperand { star: false };
    let in_condition = matches!(st.clause, Clause::Where | Clause::On | Clause::Having);
    match kw {
        "select" => {
            *st = State {
                clause: Clause::Select,
                expect: Expect::Operand { first: true },
            }
        }
        "distinct" | "all" if st.clause == Clause::Select => {}
        "distinct" if prev == Some("is") => st.expect = Expect::Words(&["FROM"]),
        "from" if prev == Some("distinct") && in_condition => st.expect = operand,
        // `EXTRACT(year FROM x)`, `SUBSTRING(a FROM 2)`.
        "from" if in_call => st.expect = operand,
        "from" if st.clause == Clause::DeleteFrom => st.expect = Expect::Table,
        "from" => {
            *st = State {
                clause: Clause::From,
                expect: Expect::Table,
            }
        }
        "join" | "straight_join" | "lateral" => {
            *st = State {
                clause: Clause::Join,
                expect: Expect::Table,
            }
        }
        "left" | "right" | "full" => st.expect = Expect::Words(&["JOIN", "OUTER JOIN"]),
        "inner" | "cross" | "natural" | "outer" => st.expect = Expect::Words(&["JOIN"]),
        "on" if matches!(st.clause, Clause::Values | Clause::InsertInto) => {
            st.expect = Expect::Words(&["CONFLICT", "DUPLICATE KEY UPDATE"])
        }
        "on" => {
            *st = State {
                clause: Clause::On,
                expect: operand,
            }
        }
        "using" if st.clause == Clause::DeleteFrom => st.expect = Expect::Table,
        "using" => {
            *st = State {
                clause: Clause::Using,
                expect: operand,
            }
        }
        "where" => {
            *st = State {
                clause: Clause::Where,
                expect: operand,
            }
        }
        "having" => {
            *st = State {
                clause: Clause::Having,
                expect: operand,
            }
        }
        "group" | "partition" => {
            *st = State {
                clause: Clause::GroupBy,
                expect: Expect::Words(&["BY"]),
            }
        }
        "order" => {
            *st = State {
                clause: Clause::OrderBy,
                expect: Expect::Words(&["BY"]),
            }
        }
        "by" => st.expect = operand,
        "limit" | "offset" | "fetch" => {
            *st = State {
                clause: Clause::Limit,
                expect: operand,
            }
        }
        "is" => st.expect = Expect::Words(&["NULL", "NOT NULL", "TRUE", "FALSE", "DISTINCT FROM"]),
        "not" | "and" | "or" | "like" | "ilike" | "in" | "between" | "when" | "then" | "else"
        | "case" | "exists" | "any" | "some" | "regexp" | "rlike" | "escape" | "interval" => {
            st.expect = operand
        }
        "similar" => st.expect = Expect::Words(&["TO"]),
        "null" | "true" | "false" | "end" | "current_date" | "current_timestamp" | "asc"
        | "first" | "last" => st.expect = after,
        "desc" | "describe" if st.expect == Expect::Statement => st.expect = Expect::Table,
        "desc" => st.expect = after,
        "nulls" => st.expect = Expect::Words(&["FIRST", "LAST"]),
        "as" => st.expect = Expect::Name,
        "update" if prev == Some("key") => {
            *st = State {
                clause: Clause::Set,
                expect: operand,
            }
        }
        "update" if matches!(prev, Some("do" | "for")) => st.expect = Expect::Words(&["SET"]),
        "update" => {
            *st = State {
                clause: Clause::Update,
                expect: Expect::Table,
            }
        }
        "set" => {
            *st = State {
                clause: Clause::Set,
                expect: operand,
            }
        }
        "insert" => {
            *st = State {
                clause: Clause::InsertInto,
                expect: Expect::Words(&["INTO"]),
            }
        }
        "into" => {
            *st = State {
                clause: Clause::InsertInto,
                expect: Expect::Table,
            }
        }
        "values" => {
            *st = State {
                clause: Clause::Values,
                expect: operand,
            }
        }
        "delete" => {
            *st = State {
                clause: Clause::DeleteFrom,
                expect: Expect::Words(&["FROM"]),
            }
        }
        "returning" => {
            *st = State {
                clause: Clause::Returning,
                expect: operand,
            }
        }
        "union" | "intersect" | "except" => {
            *st = State {
                clause: Clause::None,
                expect: Expect::Words(&["SELECT", "ALL"]),
            }
        }
        "all" => st.expect = Expect::Words(&["SELECT"]),
        "with" => {
            *st = State {
                clause: Clause::None,
                expect: Expect::Name,
            }
        }
        "explain" => st.expect = Expect::Statement,
        "table" | "truncate" => st.expect = Expect::Table,
        "do" => st.expect = Expect::Words(&["NOTHING", "UPDATE SET"]),
        "show" => {
            st.expect = Expect::Words(&[
                "TABLES",
                "DATABASES",
                "COLUMNS FROM",
                "CREATE TABLE",
                "PROCESSLIST",
            ])
        }
        "over" => st.expect = Expect::Name,
        _ => {}
    }
}

// ---- vocabulary ---------------------------------------------------------------------

pub fn statement_keywords(d: DriverKind) -> &'static [&'static str] {
    match d {
        // Only the app's own data, read-only.
        DriverKind::Sqlite => &["SELECT", "WITH", "EXPLAIN QUERY PLAN"],
        DriverKind::Postgres => &[
            "SELECT",
            "WITH",
            "INSERT INTO",
            "UPDATE",
            "DELETE FROM",
            "EXPLAIN ANALYZE",
            "BEGIN",
            "COMMIT",
            "ROLLBACK",
            "TRUNCATE",
            "CREATE TABLE",
            "ALTER TABLE",
            "DROP TABLE",
            "VACUUM",
        ],
        DriverKind::Mysql => &[
            "SELECT",
            "WITH",
            "INSERT INTO",
            "UPDATE",
            "DELETE FROM",
            "EXPLAIN",
            "SHOW",
            "DESCRIBE",
            "BEGIN",
            "COMMIT",
            "ROLLBACK",
            "TRUNCATE TABLE",
            "CREATE TABLE",
            "ALTER TABLE",
            "DROP TABLE",
            "REPLACE INTO",
        ],
    }
}

/// Keywords that fit right after a complete operand in `clause`.
pub fn after_operand_keywords(d: DriverKind, clause: Clause, star: bool) -> Vec<&'static str> {
    let pg = d == DriverKind::Postgres;
    let mut out: Vec<&'static str> = Vec::new();
    let condition = |out: &mut Vec<&'static str>| {
        out.extend([
            "AND",
            "OR",
            "LIKE",
            "NOT LIKE",
            "IN",
            "NOT IN",
            "IS NULL",
            "IS NOT NULL",
            "BETWEEN",
        ]);
        if pg {
            out.extend(["ILIKE", "NOT ILIKE"]);
        } else {
            out.extend(["REGEXP"]);
        }
    };
    match clause {
        Clause::Select if star => out.push("FROM"),
        Clause::Select => out.extend(["FROM", "AS"]),
        Clause::Where => {
            condition(&mut out);
            out.extend(["GROUP BY", "ORDER BY", "LIMIT"]);
            if pg {
                out.push("RETURNING");
            }
        }
        Clause::On | Clause::Using => {
            condition(&mut out);
            out.extend([
                "JOIN",
                "LEFT JOIN",
                "INNER JOIN",
                "WHERE",
                "GROUP BY",
                "ORDER BY",
                "LIMIT",
            ]);
        }
        Clause::Having => {
            condition(&mut out);
            out.extend(["ORDER BY", "LIMIT"]);
        }
        Clause::GroupBy => {
            out.extend(["HAVING", "ORDER BY", "LIMIT"]);
            if !pg {
                out.push("WITH ROLLUP");
            }
        }
        Clause::OrderBy => {
            out.extend(["ASC", "DESC", "LIMIT"]);
            if pg {
                out.extend(["NULLS FIRST", "NULLS LAST", "OFFSET"]);
            }
        }
        Clause::Limit => out.extend(["OFFSET", "FOR UPDATE"]),
        Clause::Set => {
            out.push("WHERE");
            if pg {
                out.extend(["FROM", "RETURNING"]);
            }
        }
        Clause::Values if pg => out.extend(["ON CONFLICT", "RETURNING"]),
        Clause::Values => out.push("ON DUPLICATE KEY UPDATE"),
        Clause::Returning => out.push("AS"),
        _ => {}
    }
    out
}

/// Keywords that fit right after a table reference in `clause`.
pub fn after_table_keywords(d: DriverKind, clause: Clause, alias: bool) -> Vec<&'static str> {
    let pg = d == DriverKind::Postgres;
    let mut out: Vec<&'static str> = Vec::new();
    match clause {
        Clause::From | Clause::Join => {
            if clause == Clause::Join {
                out.extend(["ON", "USING"]);
            }
            out.extend([
                "WHERE",
                "JOIN",
                "LEFT JOIN",
                "INNER JOIN",
                "RIGHT JOIN",
                "CROSS JOIN",
            ]);
            if pg {
                out.push("FULL JOIN");
            }
            out.extend(["GROUP BY", "ORDER BY", "LIMIT"]);
        }
        Clause::Update => out.push("SET"),
        Clause::InsertInto => {
            out.extend(["VALUES", "SELECT"]);
            if pg {
                out.push("DEFAULT VALUES");
            } else {
                out.push("SET");
            }
        }
        Clause::DeleteFrom => {
            out.push("WHERE");
            if pg {
                out.extend(["USING", "RETURNING"]);
            }
        }
        _ => {}
    }
    if !alias && matches!(clause, Clause::From | Clause::Join | Clause::Update) {
        out.push("AS");
    }
    out
}

/// Operand keywords per clause (besides columns and functions).
pub fn operand_keywords(clause: Clause, first: bool) -> &'static [&'static str] {
    match clause {
        Clause::Select if first => &["DISTINCT", "*", "CASE"],
        Clause::Select | Clause::Returning => &["CASE"],
        Clause::Where | Clause::On | Clause::Having => &["NOT", "EXISTS", "CASE", "NULL"],
        Clause::Set => &["NULL", "DEFAULT"],
        _ => &[],
    }
}

/// Common functions; inserted with an opening parenthesis.
pub fn functions(d: DriverKind) -> &'static [&'static str] {
    match d {
        DriverKind::Sqlite => &[
            "count",
            "sum",
            "avg",
            "min",
            "max",
            "coalesce",
            "nullif",
            "lower",
            "upper",
            "length",
            "trim",
            "round",
            "datetime",
            "date",
            "strftime",
            "group_concat",
        ],
        DriverKind::Postgres => &[
            "count",
            "sum",
            "avg",
            "min",
            "max",
            "coalesce",
            "nullif",
            "lower",
            "upper",
            "length",
            "trim",
            "now",
            "date_trunc",
            "extract",
            "to_char",
            "string_agg",
            "array_agg",
            "jsonb_build_object",
            "jsonb_agg",
            "generate_series",
            "cast",
            "round",
            "concat",
        ],
        DriverKind::Mysql => &[
            "count",
            "sum",
            "avg",
            "min",
            "max",
            "coalesce",
            "ifnull",
            "nullif",
            "lower",
            "upper",
            "length",
            "trim",
            "now",
            "curdate",
            "date_format",
            "date_add",
            "concat",
            "group_concat",
            "json_extract",
            "json_object",
            "cast",
            "round",
        ],
    }
}

/// Whether the clause lists columns of the tables in scope as operands.
pub fn operand_takes_columns(clause: Clause) -> bool {
    !matches!(clause, Clause::Limit | Clause::Values | Clause::None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::significant;

    fn at(sql: &str) -> State {
        state_at(sql, &significant(sql))
    }

    fn expect(sql: &str) -> Expect {
        at(sql).expect
    }

    #[test]
    fn select_list() {
        assert_eq!(expect("select "), Expect::Operand { first: true });
        assert_eq!(expect("select * "), Expect::AfterOperand { star: true });
        assert_eq!(expect("select u.* "), Expect::AfterOperand { star: true });
        assert_eq!(expect("select a * "), Expect::Operand { first: false });
        assert_eq!(expect("select a, "), Expect::Operand { first: false });
        assert_eq!(
            expect("select count(*) "),
            Expect::AfterOperand { star: false }
        );
        assert_eq!(expect("select a as "), Expect::Name);
        assert_eq!(
            expect("select a as x "),
            Expect::AfterOperand { star: false }
        );
    }

    #[test]
    fn from_and_joins() {
        assert_eq!(expect("select * from "), Expect::Table);
        assert_eq!(
            expect("select * from t "),
            Expect::AfterTable { alias: false }
        );
        assert_eq!(
            expect("select * from s.t x "),
            Expect::AfterTable { alias: true }
        );
        assert_eq!(expect("select * from a, "), Expect::Table);
        assert_eq!(
            expect("select * from a left "),
            Expect::Words(&["JOIN", "OUTER JOIN"])
        );
        let s = at("select * from a join b on ");
        assert_eq!(
            (s.clause, s.expect),
            (Clause::On, Expect::Operand { first: false })
        );
        assert_eq!(
            expect("select * from (select 1) "),
            Expect::AfterTable { alias: false }
        );
        assert_eq!(
            at("select * from (select 1) s where ").clause,
            Clause::Where
        );
    }

    #[test]
    fn conditions() {
        assert_eq!(
            expect("select * from t where "),
            Expect::Operand { first: false }
        );
        assert_eq!(
            expect("select * from t where a "),
            Expect::AfterOperand { star: false }
        );
        assert_eq!(
            expect("select * from t where a = "),
            Expect::Operand { first: false }
        );
        assert_eq!(
            expect("select * from t where a = 1 "),
            Expect::AfterOperand { star: false }
        );
        assert_eq!(
            expect("select * from t where a is "),
            Expect::Words(&["NULL", "NOT NULL", "TRUE", "FALSE", "DISTINCT FROM"])
        );
        assert_eq!(
            expect("select * from t where a in (1, 2) "),
            Expect::AfterOperand { star: false }
        );
        assert_eq!(
            expect("select * from t where extract(year from d) "),
            Expect::AfterOperand { star: false }
        );
        assert_eq!(expect("select * from t where a::"), Expect::Name);
        assert_eq!(
            at("select * from t where id in (select id from u) and ").clause,
            Clause::Where
        );
    }

    #[test]
    fn other_clauses() {
        assert_eq!(expect("select * from t group "), Expect::Words(&["BY"]));
        assert_eq!(at("select * from t order by a ").clause, Clause::OrderBy);
        assert_eq!(expect("update "), Expect::Table);
        assert_eq!(expect("update t "), Expect::AfterTable { alias: false });
        assert_eq!(at("update t set ").clause, Clause::Set);
        assert_eq!(expect("insert "), Expect::Words(&["INTO"]));
        assert_eq!(at("insert into t (").clause, Clause::InsertColumns);
        assert_eq!(
            expect("insert into t (a, b) "),
            Expect::Words(&["VALUES", "SELECT"])
        );
        assert_eq!(at("insert into t values (1) ").clause, Clause::Values);
        assert_eq!(expect("delete "), Expect::Words(&["FROM"]));
        assert_eq!(at("delete from t where ").clause, Clause::Where);
        assert_eq!(expect("with x as (select 1) "), Expect::Statement);
        assert_eq!(expect("with x "), Expect::Words(&["AS"]));
        assert_eq!(
            at("insert into t values (1) on duplicate key update ").clause,
            Clause::Set
        );
        assert_eq!(
            expect("select left(name, 2) "),
            Expect::AfterOperand { star: false }
        );
        assert_eq!(at("select extract(year from d) ").clause, Clause::Select);
    }
}
