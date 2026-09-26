use tangul_core::DriverKind;

use super::{Dialect, Namespace, Syntax, Upsert};

pub(super) struct Postgres;

impl Dialect for Postgres {
    fn kind(&self) -> DriverKind {
        DriverKind::Postgres
    }

    fn namespace(&self) -> Namespace {
        Namespace::Schema
    }

    fn default_schema(&self, _database: &str) -> Option<String> {
        Some("public".into())
    }

    fn plain_letter(&self, c: char) -> bool {
        c.is_ascii_lowercase()
    }

    fn quote(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }

    fn string(&self, value: &str) -> String {
        format!("'{}'", value.replace('\'', "''"))
    }

    fn text_match(&self, column: &str) -> (String, &'static str) {
        (format!("CAST({column} AS text)"), "ILIKE")
    }

    fn before_next_day(&self, column: &str, day: &str) -> String {
        format!("{column} < CAST({day} AS date) + 1")
    }

    fn statement_keywords(&self) -> &'static [&'static str] {
        &[
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
        ]
    }

    fn syntax(&self) -> Syntax {
        Syntax {
            ilike: true,
            regexp: false,
            returning: true,
            nulls_order: true,
            full_join: true,
            rollup: false,
            upsert: Upsert::OnConflict,
            default_values: true,
            insert_set: false,
            update_from: true,
            delete_using: true,
        }
    }

    fn side_effect_functions(&self) -> &'static [&'static str] {
        &[
            "pg_terminate_backend",
            "pg_cancel_backend",
            "pg_reload_conf",
            "pg_rotate_logfile",
            "pg_promote",
            "pg_notify",
            "pg_advisory",
            "set_config",
            "nextval",
            "setval",
            "lo_import",
            "lo_export",
            "lo_unlink",
            "pg_read_file",
            "pg_read_binary_file",
            "pg_ls_dir",
            "pg_file_write",
            "dblink",
        ]
    }
}
