use tangul_core::DriverKind;

use crate::lexer::Lexing;

use super::{AI_CONNECTION_LIMIT, Dialect, Namespace, Syntax, Upsert, quote_ident};

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

    /// Tables it may write to, or drop: owned by it or a role it belongs to
    /// (superusers pass every check).
    fn writable_tables_sql(&self) -> Option<&'static str> {
        Some(
            "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind IN ('r', 'p') \
               AND n.nspname NOT IN ('pg_catalog', 'information_schema') \
               AND n.nspname NOT LIKE 'pg_toast%' AND n.nspname NOT LIKE 'pg_temp%' \
               AND (has_table_privilege(c.oid, 'INSERT, UPDATE, DELETE, TRUNCATE') \
                    OR pg_has_role(c.relowner, 'MEMBER'))",
        )
    }

    /// Read-only by grants, and by default for its sessions as a second line; its
    /// sessions stop a statement after 5 minutes and an idle transaction after one
    /// (tangul sets its own statement limit per session).
    fn readonly_account_sql(
        &self,
        user: &str,
        password: &str,
        database: &str,
        schemas: &[String],
    ) -> Option<Vec<String>> {
        let role = quote_ident(self, user);
        let mut sql = vec![
            format!(
                "CREATE ROLE {role} LOGIN PASSWORD {} CONNECTION LIMIT {AI_CONNECTION_LIMIT}",
                self.string(password)
            ),
            format!("ALTER ROLE {role} SET default_transaction_read_only = on"),
            format!("ALTER ROLE {role} SET statement_timeout = '5min'"),
            format!("ALTER ROLE {role} SET idle_in_transaction_session_timeout = '1min'"),
            format!(
                "GRANT CONNECT ON DATABASE {} TO {role}",
                quote_ident(self, database)
            ),
        ];
        for schema in schemas {
            let schema = quote_ident(self, schema);
            sql.push(format!("GRANT USAGE ON SCHEMA {schema} TO {role}"));
            sql.push(format!(
                "GRANT SELECT ON ALL TABLES IN SCHEMA {schema} TO {role}"
            ));
            sql.push(format!(
                "ALTER DEFAULT PRIVILEGES IN SCHEMA {schema} GRANT SELECT ON TABLES TO {role}"
            ));
        }
        Some(sql)
    }

    fn lexing(&self) -> Lexing {
        Lexing {
            nested_comments: true,
            ..Lexing::default()
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
