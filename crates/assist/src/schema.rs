//! Lookup structure over a connection's [`SchemaInfo`].

use std::collections::HashMap;
use std::ops::Range;

use tangul_core::{ColumnMeta, EnumInfo, ForeignKey, SchemaInfo, TableInfo};

use crate::dialect::{Dialect, quote_ident};

pub struct SchemaIndex {
    info: SchemaInfo,
    dialect: &'static dyn Dialect,
    /// Schema that unqualified names resolve to (`public`, or the MySQL database).
    default_schema: Option<String>,
    by_name: HashMap<(String, String), usize>,
    columns: Vec<Range<usize>>,
}

impl SchemaIndex {
    pub fn new(
        info: SchemaInfo,
        dialect: &'static dyn Dialect,
        default_schema: Option<String>,
    ) -> Self {
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
            dialect,
            default_schema,
            by_name,
            columns,
        }
    }

    pub fn info(&self) -> &SchemaInfo {
        &self.info
    }

    pub fn dialect(&self) -> &'static dyn Dialect {
        self.dialect
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

    /// The enum type `column` has, with its values. Postgres names the type
    /// `schema.type`; MySQL's enums are per column, named `table.column`.
    pub fn enum_of(&self, column: &ColumnMeta) -> Option<&EnumInfo> {
        let ty = column.enum_type.as_deref()?;
        self.info.enums.iter().find(|e| {
            ty == format!("{}.{}", e.schema, e.name) || (e.schema == column.schema && e.name == ty)
        })
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
        let name = quote_ident(self.dialect, &t.name);
        let is_default = self
            .default_schema
            .as_deref()
            .is_some_and(|d| d.eq_ignore_ascii_case(&t.schema));
        if is_default {
            name
        } else {
            format!("{}.{name}", quote_ident(self.dialect, &t.schema))
        }
    }

    pub fn quote(&self, ident: &str) -> String {
        quote_ident(self.dialect, ident)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::dialect::dialect_for;
    use tangul_core::{DriverKind, TableKind};

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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
        };
        SchemaIndex::new(info, dialect_for(driver), Some("public".into()))
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
        assert_eq!(
            quote_ident(dialect_for(DriverKind::Mysql), "order"),
            "`order`"
        );
        assert_eq!(
            quote_ident(dialect_for(DriverKind::Mysql), "Users"),
            "Users"
        );
        assert_eq!(
            quote_ident(dialect_for(DriverKind::Postgres), "user"),
            "\"user\""
        );
        assert_eq!(quote_ident(dialect_for(DriverKind::Mysql), "key"), "`key`");
        assert_eq!(
            quote_ident(dialect_for(DriverKind::Postgres), "spatial_ref_sys"),
            "spatial_ref_sys"
        );
    }
}
