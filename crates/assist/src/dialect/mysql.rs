use tangul_core::DriverKind;

use crate::lexer::Lexing;

use super::{AI_CONNECTION_LIMIT, Dialect, Namespace, Syntax, Upsert, quote_ident};

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

    /// Tables it may write to, alter or drop: by a global, schema or table grant
    /// of the current account (`'user'@'host'` in the privilege tables).
    fn writable_tables_sql(&self) -> Option<&'static str> {
        Some(
            "SELECT count(*) FROM information_schema.tables t \
             JOIN (SELECT CONCAT(QUOTE(SUBSTRING_INDEX(CURRENT_USER(), '@', 1)), '@', \
                                 QUOTE(SUBSTRING_INDEX(CURRENT_USER(), '@', -1))) AS me) a \
             WHERE t.table_type = 'BASE TABLE' \
               AND t.table_schema NOT IN ('mysql', 'information_schema', 'performance_schema', 'sys') \
               AND (EXISTS (SELECT 1 FROM information_schema.user_privileges p \
                            WHERE p.grantee = a.me \
                              AND p.privilege_type IN ('INSERT', 'UPDATE', 'DELETE', 'DROP', 'ALTER')) \
                 OR EXISTS (SELECT 1 FROM information_schema.schema_privileges p \
                            WHERE p.grantee = a.me AND p.table_schema = t.table_schema \
                              AND p.privilege_type IN ('INSERT', 'UPDATE', 'DELETE', 'DROP', 'ALTER')) \
                 OR EXISTS (SELECT 1 FROM information_schema.table_privileges p \
                            WHERE p.grantee = a.me AND p.table_schema = t.table_schema \
                              AND p.table_name = t.table_name \
                              AND p.privilege_type IN ('INSERT', 'UPDATE', 'DELETE', 'DROP', 'ALTER')))",
        )
    }

    /// Schemas are databases here: `SELECT` (and `SHOW VIEW`) on each.
    fn readonly_account_sql(
        &self,
        user: &str,
        password: &str,
        _database: &str,
        schemas: &[String],
    ) -> Option<Vec<String>> {
        let account = format!("{}@'%'", self.string(user));
        let mut sql = vec![format!(
            "CREATE USER {account} IDENTIFIED BY {} WITH MAX_USER_CONNECTIONS \
             {AI_CONNECTION_LIMIT}",
            self.string(password)
        )];
        for schema in schemas {
            sql.push(format!(
                "GRANT SELECT, SHOW VIEW ON {}.* TO {account}",
                quote_ident(self, schema)
            ));
        }
        Some(sql)
    }

    /// `DELIMITER` is a client command (the `mysql` shell's), used in scripts that
    /// create procedures and triggers.
    fn lexing(&self) -> Lexing {
        Lexing {
            hash_comments: true,
            nested_comments: false,
            dollar_quotes: false,
            delimiter_command: true,
        }
    }

    fn side_effect_functions(&self) -> &'static [&'static str] {
        &[
            "load_file",
            // `SELECT … INTO OUTFILE / DUMPFILE` writes a file on the server; also
            // found where a string hides it from the tokens.
            "outfile",
            "dumpfile",
            "get_lock",
            "release_lock",
            "sys_exec",
            "sys_eval",
        ]
    }
}
