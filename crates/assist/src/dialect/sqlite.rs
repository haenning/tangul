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
