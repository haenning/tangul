//! Lookup structure over a connection's [`SchemaInfo`].

use std::collections::HashMap;
use std::ops::Range;

use rusdb_core::{ColumnMeta, DriverKind, ForeignKey, SchemaInfo, TableInfo};

use crate::analyze::is_reserved;

pub struct SchemaIndex {
    info: SchemaInfo,
    driver: DriverKind,
    /// Schema that unqualified names resolve to (`public`, or the MySQL database).
    default_schema: Option<String>,
    by_name: HashMap<(String, String), usize>,
    columns: Vec<Range<usize>>,
}

impl SchemaIndex {
    pub fn new(info: SchemaInfo, driver: DriverKind, default_schema: Option<String>) -> Self {
        let by_name = info
            .tables
            .iter()
            .enumerate()
            .map(|(i, t)| ((t.schema.to_lowercase(), t.name.to_lowercase()), i))
            .collect::<HashMap<_, _>>();
        // Columns arrive grouped by table; map each table to its slice.
        let mut columns = vec![0..0; info.tables.len()];
        let mut start = 0;
        for group in info
            .columns
            .chunk_by(|a, b| a.schema == b.schema && a.table == b.table)
        {
            let key = (
                group[0].schema.to_lowercase(),
                group[0].table.to_lowercase(),
            );
            if let Some(&ix) = by_name.get(&key) {
                columns[ix] = start..start + group.len();
            }
            start += group.len();
        }
        Self {
            info,
            driver,
            default_schema,
            by_name,
            columns,
        }
    }

    /// Default schema per driver: `public` for Postgres, the connection's database
    /// for MySQL.
    pub fn default_schema_for(driver: DriverKind, database: &str) -> Option<String> {
        match driver {
            DriverKind::Postgres => Some("public".into()),
            DriverKind::Mysql => (!database.is_empty()).then(|| database.to_string()),
        }
    }

    pub fn info(&self) -> &SchemaInfo {
        &self.info
    }

    pub fn driver(&self) -> DriverKind {
        self.driver
    }

    pub fn tables(&self) -> &[TableInfo] {
        &self.info.tables
    }

    pub fn table(&self, ix: usize) -> &TableInfo {
        &self.info.tables[ix]
    }

    pub fn columns(&self, table: usize) -> &[ColumnMeta] {
        &self.info.columns[self.columns[table].clone()]
    }

    pub fn foreign_keys(&self) -> &[ForeignKey] {
        &self.info.foreign_keys
    }

    pub fn has_schema(&self, name: &str) -> bool {
        self.info
            .tables
            .iter()
            .any(|t| t.schema.eq_ignore_ascii_case(name))
    }

    /// Finds a table by (optional) schema and name. Unqualified names prefer the
    /// default schema, then a unique match in any schema.
    pub fn resolve(&self, schema: Option<&str>, name: &str) -> Option<usize> {
        let name = name.to_lowercase();
        if let Some(schema) = schema {
            return self.by_name.get(&(schema.to_lowercase(), name)).copied();
        }
        if let Some(default) = &self.default_schema
            && let Some(&ix) = self.by_name.get(&(default.to_lowercase(), name.clone()))
        {
            return Some(ix);
        }
        let mut found = self
            .info
            .tables
            .iter()
            .enumerate()
            .filter(|(_, t)| t.name.to_lowercase() == name);
        match (found.next(), found.next()) {
            (Some((ix, _)), None) => Some(ix),
            _ => None,
        }
    }

    /// The name to insert into SQL: schema-qualified unless in the default schema,
    /// quoted where needed.
    pub fn sql_name(&self, table: usize) -> String {
        let t = &self.info.tables[table];
        let name = quote_ident(self.driver, &t.name);
        let is_default = self
            .default_schema
            .as_deref()
            .is_some_and(|d| d.eq_ignore_ascii_case(&t.schema));
        if is_default {
            name
        } else {
            format!("{}.{name}", quote_ident(self.driver, &t.schema))
        }
    }

    pub fn quote(&self, ident: &str) -> String {
        quote_ident(self.driver, ident)
    }
}

/// Quotes an identifier if it would not survive unquoted (Postgres folds unquoted
/// names to lowercase, so any uppercase letter needs quotes there).
pub fn quote_ident(driver: DriverKind, ident: &str) -> String {
    let plain = ident
        .chars()
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && ident.chars().all(|c| {
            c == '_'
                || c.is_ascii_digit()
                || match driver {
                    DriverKind::Postgres => c.is_ascii_lowercase(),
                    DriverKind::Mysql => c.is_ascii_alphabetic(),
                }
        })
        && !is_reserved(ident);
    if plain {
        return ident.to_string();
    }
    match driver {
        DriverKind::Postgres => format!("\"{}\"", ident.replace('"', "\"\"")),
        DriverKind::Mysql => format!("`{}`", ident.replace('`', "``")),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use rusdb_core::TableKind;

    /// `app.users(id pk, email, name)`, `app.orders(id pk, user_id → users.id, total)`,
    /// `public.Logs(id)`.
    pub fn sample() -> SchemaIndex {
        sample_for(DriverKind::Postgres)
    }

    pub fn sample_for(driver: DriverKind) -> SchemaIndex {
        let table = |schema: &str, name: &str| TableInfo {
            schema: schema.into(),
            name: name.into(),
            kind: TableKind::Table,
        };
        let col = |schema: &str, table: &str, name: &str, pk: bool| ColumnMeta {
            schema: schema.into(),
            table: table.into(),
            name: name.into(),
            data_type: if pk || name.ends_with("_id") {
                "integer".into()
            } else {
                "text".into()
            },
            nullable: !pk,
            primary_key: pk,
        };
        let info = SchemaInfo {
            tables: vec![
                table("app", "orders"),
                table("app", "users"),
                table("public", "Logs"),
            ],
            columns: vec![
                col("app", "orders", "id", true),
                col("app", "orders", "user_id", false),
                col("app", "orders", "total", false),
                col("app", "users", "id", true),
                col("app", "users", "email", false),
                col("app", "users", "name", false),
                col("public", "Logs", "id", true),
            ],
            foreign_keys: vec![ForeignKey {
                name: "orders_user_id_fkey".into(),
                schema: "app".into(),
                table: "orders".into(),
                columns: vec!["user_id".into()],
                ref_schema: "app".into(),
                ref_table: "users".into(),
                ref_columns: vec!["id".into()],
            }],
        };
        SchemaIndex::new(info, driver, Some("public".into()))
    }

    #[test]
    fn lookups() {
        let s = sample();
        let users = s.resolve(None, "USERS").unwrap();
        assert_eq!(s.table(users).name, "users");
        assert_eq!(s.columns(users).len(), 3);
        assert_eq!(s.sql_name(users), "app.users");
        assert_eq!(s.sql_name(s.resolve(None, "logs").unwrap()), "\"Logs\"");
        assert_eq!(s.resolve(Some("public"), "orders"), None);
        assert_eq!(quote_ident(DriverKind::Mysql, "order"), "`order`");
        assert_eq!(quote_ident(DriverKind::Mysql, "Users"), "Users");
    }
}
