use tangul_core::DriverKind;

use super::{Dialect, Namespace, Syntax, Upsert};

pub(super) struct Mysql;

impl Dialect for Mysql {
    fn kind(&self) -> DriverKind {
        DriverKind::Mysql
    }

    fn namespace(&self) -> Namespace {
        Namespace::Database
    }

    fn default_schema(&self, database: &str) -> Option<String> {
        (!database.is_empty()).then(|| database.to_string())
    }

    fn plain_letter(&self, c: char) -> bool {
        c.is_ascii_alphabetic()
    }

    fn quote(&self, ident: &str) -> String {
        format!("`{}`", ident.replace('`', "``"))
    }

    /// Backslashes escape in MySQL strings (unless `NO_BACKSLASH_ESCAPES`).
    fn string(&self, value: &str) -> String {
        format!("'{}'", value.replace('\'', "''").replace('\\', "\\\\"))
    }

    /// The usual collations already ignore case.
    fn text_match(&self, column: &str) -> (String, &'static str) {
        (format!("CAST({column} AS CHAR)"), "LIKE")
    }

    fn before_next_day(&self, column: &str, day: &str) -> String {
        format!("{column} < {day} + INTERVAL 1 DAY")
    }

    fn statement_keywords(&self) -> &'static [&'static str] {
        &[
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
        ]
    }

    fn functions(&self) -> &'static [&'static str] {
        &[
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
        ]
    }

    fn syntax(&self) -> Syntax {
        Syntax {
            ilike: false,
            regexp: true,
            returning: false,
            nulls_order: false,
            full_join: false,
            rollup: true,
            upsert: Upsert::OnDuplicateKey,
            default_values: false,
            insert_set: true,
            update_from: false,
            delete_using: false,
        }
    }

    fn side_effect_functions(&self) -> &'static [&'static str] {
        &[
            "load_file",
            "get_lock",
            "release_lock",
            "sys_exec",
            "sys_eval",
        ]
    }
}
