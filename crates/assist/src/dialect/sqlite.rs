use tangul_core::DriverKind;

use super::{Dialect, Namespace, Syntax, Upsert};

/// tangul's own files (bookmark metrics), opened read-only.
pub(super) struct Sqlite;

impl Dialect for Sqlite {
    fn kind(&self) -> DriverKind {
        DriverKind::Sqlite
    }

    fn namespace(&self) -> Namespace {
        Namespace::Schema
    }

    fn default_schema(&self, _database: &str) -> Option<String> {
        Some("main".into())
    }

    fn plain_letter(&self, c: char) -> bool {
        c.is_ascii_alphabetic()
    }

    fn quote(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }

    fn string(&self, value: &str) -> String {
        format!("'{}'", value.replace('\'', "''"))
    }

    /// `LIKE` ignores ASCII case already.
    fn char_length(&self, expr: &str) -> String {
        format!("length({expr})")
    }

    fn text_match(&self, column: &str) -> (String, &'static str) {
        (format!("CAST({column} AS TEXT)"), "LIKE")
    }

    fn before_next_day(&self, column: &str, day: &str) -> String {
        format!("{column} < date({day}, '+1 day')")
    }

    /// Read-only: only reads are offered.
    fn statement_keywords(&self) -> &'static [&'static str] {
        &["SELECT", "WITH", "EXPLAIN QUERY PLAN"]
    }

    fn functions(&self) -> &'static [&'static str] {
        &[
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
            "julianday",
            "ifnull",
            "substr",
            "replace",
            "abs",
            "row_number",
            "rank",
            "lag",
            "lead",
        ]
    }

    fn cast_types(&self) -> &'static [&'static str] {
        &["INTEGER", "REAL", "TEXT", "NUMERIC", "BLOB"]
    }

    fn string_choices(&self, function: &str, arg: usize) -> &'static [&'static str] {
        match (function, arg) {
            ("strftime", 0) => &["%Y-%m-%d", "%Y-%m", "%Y-%m-%d %H:%M", "%H:%M", "%w"],
            ("date" | "datetime", 1) => &["-7 days", "-1 month", "start of month", "+1 day"],
            _ => &[],
        }
    }

    fn date_values(&self) -> &'static [&'static str] {
        &[
            "DATE('now')",
            "DATETIME('now', '-7 days')",
            "DATE('now', '-30 days')",
            "DATE('now', 'start of month')",
        ]
    }

    /// Only reads are offered, and SQLite has no `OFFSET` without `LIMIT`.
    fn syntax(&self) -> Syntax {
        Syntax {
            ilike: false,
            regexp: false,
            returning: false,
            nulls_order: false,
            full_join: true,
            rollup: false,
            upsert: Upsert::OnConflict,
            default_values: false,
            insert_set: false,
            update_from: false,
            delete_using: false,
        }
    }

    /// Opened read-only, and without extensions that reach outside the file.
    fn side_effect_functions(&self) -> &'static [&'static str] {
        &[]
    }
}
