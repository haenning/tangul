//! What differs between SQL databases, as text: names and quoting, literals, the SQL
//! tangul writes itself, the editor's vocabulary and the functions with side effects.
//! Pure, no I/O; connecting and running belong to the driver (`tangul-drivers`).
//!
//! Only this module, the drivers, `DriverKind` and the connection form name concrete
//! databases. Everything else asks the connection's [`Dialect`]. A new database is a
//! `DriverKind` variant, a module here and a driver.

mod mysql;
mod postgres;
mod sqlite;

use tangul_core::DriverKind;

use crate::lexer::Lexing;

/// The dialect of a database.
pub fn dialect_for(kind: DriverKind) -> &'static dyn Dialect {
    match kind {
        DriverKind::Postgres => &postgres::Postgres,
        DriverKind::Mysql => &mysql::Mysql,
        DriverKind::Sqlite => &sqlite::Sqlite,
    }
}

/// Every known dialect.
pub fn all() -> impl Iterator<Item = &'static dyn Dialect> {
    DriverKind::ALL.into_iter().map(dialect_for)
}

pub trait Dialect: Sync {
    fn kind(&self) -> DriverKind;

    /// What the level above tables is called.
    fn namespace(&self) -> Namespace;

    /// The schema unqualified names resolve to; `database` is the connection's.
    fn default_schema(&self, database: &str) -> Option<String>;

    /// Whether a letter may stay unquoted in an identifier (Postgres folds unquoted
    /// names to lower case, so upper case letters need quotes there).
    fn plain_letter(&self, c: char) -> bool;

    /// `ident` in identifier quotes.
    fn quote(&self, ident: &str) -> String;

    /// `value` as a string literal, quotes included.
    fn string(&self, value: &str) -> String;

    /// The clause that keeps `limit` rows after skipping `offset`.
    fn limit(&self, limit: usize, offset: usize) -> String {
        match offset {
            0 => format!("LIMIT {limit}"),
            _ => format!("LIMIT {limit} OFFSET {offset}"),
        }
    }

    /// The number of characters of `expr`.
    fn char_length(&self, expr: &str) -> String {
        format!("char_length({expr})")
    }

    /// `column` as text, and the operator matching it case-insensitively with `LIKE`
    /// patterns.
    fn text_match(&self, column: &str) -> (String, &'static str);

    /// `column` before the day after `day` (a date literal): the end of a day range.
    fn before_next_day(&self, column: &str, day: &str) -> String;

    /// Statement starts offered at the beginning of a statement.
    fn statement_keywords(&self) -> &'static [&'static str];

    /// Common functions offered as operands.
    fn functions(&self) -> &'static [&'static str];

    /// Clauses and operators the editor offers beyond standard SQL.
    fn syntax(&self) -> Syntax;

    /// Units after `INTERVAL 1`, most used first; none where there is no `INTERVAL`.
    fn interval_units(&self) -> &'static [&'static str] {
        &[]
    }

    /// Units written inside the interval's text (`INTERVAL '7 days'`), singular.
    fn interval_text_units(&self) -> &'static [&'static str] {
        &[]
    }

    /// Fields of `EXTRACT(… FROM x)`.
    fn date_fields(&self) -> &'static [&'static str] {
        &[]
    }

    /// Types offered in casts.
    fn cast_types(&self) -> &'static [&'static str];

    /// The string literals argument `arg` (from 0) of `function` (lower case)
    /// takes: units, fields, formats.
    fn string_choices(&self, _function: &str, _arg: usize) -> &'static [&'static str] {
        &[]
    }

    /// Values offered when comparing a date or timestamp column: today, the last
    /// days, ….
    fn date_values(&self) -> &'static [&'static str];

    /// Comments, quoting and terminators the database's lexer knows beyond the
    /// common ones.
    fn lexing(&self) -> Lexing {
        Lexing::default()
    }

    /// `[…]` arrays and their operators (`@>`, `&&`), which the formatter must
    /// know not to break apart.
    fn array_syntax(&self) -> bool {
        false
    }

    /// A query answering with one number: how many tables the connected user may
    /// change (insert, update, delete, truncate) or drop. `None` where the database
    /// has no users to ask about (SQLite).
    fn writable_tables_sql(&self) -> Option<&'static str> {
        None
    }

    /// Statements an admin runs to create a login that may only read: `user` with
    /// `password` may connect to `database` and select from the tables of `schemas`
    /// (for Postgres also from tables created there later by the admin), with at
    /// most [`AI_CONNECTION_LIMIT`] connections (and, on Postgres, time limits for
    /// sessions outside tangul too). `None` where the database has no users (SQLite).
    fn readonly_account_sql(
        &self,
        _user: &str,
        _password: &str,
        _database: &str,
        _schemas: &[String],
    ) -> Option<Vec<String>> {
        None
    }

    /// Functions a `SELECT` can call that change something (sessions, files,
    /// sequences, settings, other servers) or read files. Lower case; the guard
    /// matches them as substrings.
    fn side_effect_functions(&self) -> &'static [&'static str];

    /// `EXPLAIN` that plans `sql` without running it and answers with the plan as
    /// JSON in one cell, for [`Dialect::plan_cost`]. `None` where there is none.
    fn explain_sql(&self, _sql: &str) -> Option<String> {
        None
    }

    /// The planner's estimated cost in `plan` (the cell [`Dialect::explain_sql`]
    /// answers with), in its own units.
    fn plan_cost(&self, _plan: &str) -> Option<f64> {
        None
    }
}

/// Above this estimated cost a query an AI writes waits for the user: about a full
/// scan of tens of millions of rows, in both Postgres' and MySQL's units.
pub const HEAVY_COST: f64 = 1_000_000.;

impl std::fmt::Debug for dyn Dialect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.kind())
    }
}

impl PartialEq for dyn Dialect {
    fn eq(&self, other: &Self) -> bool {
        self.kind() == other.kind()
    }
}

/// Connections the read-only AI user may have at once: its own pool, and tabs if
/// someone opens it as their user too. Limits the load a runaway client can put on
/// the server.
pub const AI_CONNECTION_LIMIT: u32 = 10;

/// What a dialect calls the level above tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace {
    Schema,
    Database,
}

/// Syntax beyond standard SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Syntax {
    /// `ILIKE`, `NOT ILIKE`.
    pub ilike: bool,
    /// `REGEXP`.
    pub regexp: bool,
    /// `RETURNING` after `WHERE`, `SET`, `VALUES` and `DELETE`.
    pub returning: bool,
    /// `NULLS FIRST` / `NULLS LAST` and `OFFSET` after `ORDER BY`.
    pub nulls_order: bool,
    pub full_join: bool,
    /// `WITH ROLLUP` after `GROUP BY`.
    pub rollup: bool,
    pub upsert: Upsert,
    /// `INSERT … DEFAULT VALUES`.
    pub default_values: bool,
    /// `INSERT … SET`.
    pub insert_set: bool,
    /// `UPDATE … SET … FROM`.
    pub update_from: bool,
    /// `DELETE … USING`.
    pub delete_using: bool,
}

/// How an insert updates an existing row instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    /// `ON CONFLICT`
    OnConflict,
    /// `ON DUPLICATE KEY UPDATE`
    OnDuplicateKey,
}

/// Reserved words (in any dialect) beyond the clause keywords the analysis knows;
/// a table or column with such a name must be quoted.
const QUOTE_RESERVED: &[&str] = &[
    "analyse",
    "analyze",
    "array",
    "asymmetric",
    "both",
    "cast",
    "check",
    "collate",
    "column",
    "constraint",
    "create",
    "current_catalog",
    "current_date",
    "current_role",
    "current_time",
    "current_timestamp",
    "current_user",
    "default",
    "deferrable",
    "do",
    "false",
    "foreign",
    "grant",
    "index",
    "initially",
    "key",
    "keys",
    "leading",
    "localtime",
    "localtimestamp",
    "only",
    "placing",
    "primary",
    "range",
    "rank",
    "references",
    "row",
    "rows",
    "session_user",
    "symmetric",
    "trailing",
    "true",
    "unique",
    "user",
    "variadic",
];

/// `ident`, quoted only if it wouldn't survive unquoted.
pub fn quote_ident(d: &dyn Dialect, ident: &str) -> String {
    let plain = ident
        .chars()
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && ident
            .chars()
            .all(|c| c == '_' || c.is_ascii_digit() || d.plain_letter(c))
        && !crate::analyze::is_reserved(ident)
        && !QUOTE_RESERVED.iter().any(|k| ident.eq_ignore_ascii_case(k));
    if plain {
        ident.to_string()
    } else {
        d.quote(ident)
    }
}

/// A literal for a text-protocol value: numbers as they are (for numeric columns),
/// everything else as a string.
pub fn literal(d: &dyn Dialect, value: &str, numeric: bool) -> String {
    let plain_number = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))
        && value.parse::<f64>().is_ok_and(f64::is_finite);
    if numeric && plain_number {
        value.to_string()
    } else {
        d.string(value)
    }
}

/// The first rows of a table, for opening it: `SELECT * FROM name LIMIT n`.
pub fn select_rows(d: &dyn Dialect, table: &str, limit: usize) -> String {
    format!("SELECT * FROM {table} {}", d.limit(limit, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_its_dialect() {
        for kind in DriverKind::ALL {
            assert_eq!(dialect_for(kind).kind(), kind);
        }
    }

    #[test]
    fn identifiers() {
        let (pg, my) = (
            dialect_for(DriverKind::Postgres),
            dialect_for(DriverKind::Mysql),
        );
        assert_eq!(quote_ident(my, "order"), "`order`");
        assert_eq!(quote_ident(my, "Users"), "Users");
        assert_eq!(quote_ident(pg, "Users"), "\"Users\"");
        assert_eq!(quote_ident(pg, "user"), "\"user\"");
        assert_eq!(quote_ident(pg, "a\"b"), "\"a\"\"b\"");
        assert_eq!(quote_ident(my, "a`b"), "`a``b`");
        assert_eq!(quote_ident(pg, "spatial_ref_sys"), "spatial_ref_sys");
    }

    #[test]
    fn literals() {
        let (pg, my) = (
            dialect_for(DriverKind::Postgres),
            dialect_for(DriverKind::Mysql),
        );
        assert_eq!(literal(pg, "12", true), "12");
        assert_eq!(literal(pg, "12", false), "'12'");
        assert_eq!(literal(pg, "1; drop", true), "'1; drop'");
        assert_eq!(literal(pg, "o'k\\", false), "'o''k\\'");
        assert_eq!(literal(my, "o'k\\", false), "'o''k\\\\'");
    }

    #[test]
    fn generated_sql() {
        for d in all() {
            assert_eq!(select_rows(d, "t", 100), "SELECT * FROM t LIMIT 100");
            assert_eq!(d.limit(10, 20), "LIMIT 10 OFFSET 20");
        }
        let day = |kind| dialect_for(kind).before_next_day("c", "'2026-01-31'");
        assert_eq!(
            day(DriverKind::Postgres),
            "c < CAST('2026-01-31' AS date) + 1"
        );
        assert_eq!(day(DriverKind::Mysql), "c < '2026-01-31' + INTERVAL 1 DAY");
        assert_eq!(day(DriverKind::Sqlite), "c < date('2026-01-31', '+1 day')");
    }

    #[test]
    fn namespaces_and_default_schemas() {
        let d = |kind| dialect_for(kind);
        assert_eq!(d(DriverKind::Mysql).namespace(), Namespace::Database);
        assert_eq!(d(DriverKind::Postgres).namespace(), Namespace::Schema);
        assert_eq!(
            d(DriverKind::Postgres).default_schema("x").as_deref(),
            Some("public")
        );
        assert_eq!(
            d(DriverKind::Mysql).default_schema("app").as_deref(),
            Some("app")
        );
        assert_eq!(d(DriverKind::Mysql).default_schema(""), None);
        assert_eq!(
            d(DriverKind::Sqlite).default_schema("").as_deref(),
            Some("main")
        );
    }

    #[test]
    fn plans_and_their_costs() {
        let (pg, my) = (
            dialect_for(DriverKind::Postgres),
            dialect_for(DriverKind::Mysql),
        );
        assert_eq!(
            pg.explain_sql("select 1").as_deref(),
            Some("EXPLAIN (FORMAT JSON) select 1")
        );
        assert_eq!(
            my.explain_sql("select 1").as_deref(),
            Some("EXPLAIN FORMAT=JSON select 1")
        );
        assert_eq!(
            dialect_for(DriverKind::Sqlite).explain_sql("select 1"),
            None
        );
        let plan = r#"[{"Plan": {"Node Type": "Seq Scan", "Total Cost": 1234.5, "Plan Rows": 9}}]"#;
        assert_eq!(pg.plan_cost(plan), Some(1234.5));
        assert_eq!(pg.plan_cost("nonsense"), None);
        let plan = r#"{"query_block": {"select_id": 1, "cost_info": {"query_cost": "2500.75"}}}"#;
        assert_eq!(my.plan_cost(plan), Some(2500.75));
        // MariaDB: no cost.
        assert_eq!(my.plan_cost(r#"{"query_block": {"select_id": 1}}"#), None);
        assert_eq!(dialect_for(DriverKind::Sqlite).plan_cost(plan), None);
    }

    #[test]
    fn read_only_accounts() {
        let d = dialect_for;
        let schemas = ["public".to_string(), "Sales".to_string()];
        assert_eq!(
            d(DriverKind::Postgres)
                .readonly_account_sql("tangul_ai", "p'w", "shop", &schemas)
                .unwrap(),
            [
                "CREATE ROLE tangul_ai LOGIN PASSWORD 'p''w' CONNECTION LIMIT 10",
                "ALTER ROLE tangul_ai SET default_transaction_read_only = on",
                "ALTER ROLE tangul_ai SET statement_timeout = '5min'",
                "ALTER ROLE tangul_ai SET idle_in_transaction_session_timeout = '1min'",
                "GRANT CONNECT ON DATABASE shop TO tangul_ai",
                "GRANT USAGE ON SCHEMA public TO tangul_ai",
                "GRANT SELECT ON ALL TABLES IN SCHEMA public TO tangul_ai",
                "ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO tangul_ai",
                "GRANT USAGE ON SCHEMA \"Sales\" TO tangul_ai",
                "GRANT SELECT ON ALL TABLES IN SCHEMA \"Sales\" TO tangul_ai",
                "ALTER DEFAULT PRIVILEGES IN SCHEMA \"Sales\" GRANT SELECT ON TABLES TO tangul_ai",
            ]
        );
        assert_eq!(
            d(DriverKind::Mysql)
                .readonly_account_sql("tangul_ai", "pw", "shop", &["shop".to_string()])
                .unwrap(),
            [
                "CREATE USER 'tangul_ai'@'%' IDENTIFIED BY 'pw' WITH MAX_USER_CONNECTIONS 10",
                "GRANT SELECT, SHOW VIEW ON shop.* TO 'tangul_ai'@'%'",
            ]
        );
        assert_eq!(
            d(DriverKind::Sqlite).readonly_account_sql("a", "b", "c", &[]),
            None
        );
    }
}
