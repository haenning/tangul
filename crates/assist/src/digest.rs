//! A table in a few lines: what an assistant needs to write SQL against it (types,
//! keys, references, enum values, comments, checks), without the DDL's noise.
//! Deterministic, so it can be compared and cached.

use crate::schema::SchemaIndex;

/// Enum values listed at most; more are cut with `…`.
const ENUM_VALUES: usize = 12;

/// `app.orders (~300000 rows) — Customer orders` and the columns on the next line,
/// e.g. `id bigint pk, user_id integer → app.users.id, status app.order_status
/// {open, paid}, ordered date "Day the order was placed"`, then its checks.
pub fn digest(schema: &SchemaIndex, table: usize) -> String {
    let info = schema.info();
    let t = schema.table(table);
    let mut head = Vec::new();
    if t.kind != tangul_core::TableKind::Table {
        head.push(t.kind.label().to_string());
    }
    if let Some(n) = t.row_estimate {
        head.push(format!("~{n} rows"));
    }
    let mut out = schema.sql_name(table);
    if !head.is_empty() {
        out.push_str(&format!(" ({})", head.join(", ")));
    }
    if let Some(c) = &t.comment {
        out.push_str(&format!(" — {c}"));
    }
    let keys: Vec<_> = info
        .foreign_keys
        .iter()
        .filter(|f| f.schema == t.schema && f.table == t.name && f.columns.len() == 1)
        .collect();
    let columns: Vec<String> = schema
        .columns(table)
        .iter()
        .map(|c| {
            let mut s = format!("{} {}", c.name, c.data_type);
            if c.primary_key {
                s.push_str(" pk");
            }
            if let Some(f) = keys.iter().find(|f| f.columns[0] == c.name) {
                let target = schema
                    .resolve(Some(&f.ref_schema), &f.ref_table)
                    .map_or_else(
                        || format!("{}.{}", f.ref_schema, f.ref_table),
                        |i| schema.sql_name(i),
                    );
                s.push_str(&format!(" → {target}.{}", f.ref_columns[0]));
            }
            if let Some(e) = schema.enum_of(c) {
                let mut values: Vec<&str> = e
                    .values
                    .iter()
                    .take(ENUM_VALUES)
                    .map(String::as_str)
                    .collect();
                if e.values.len() > ENUM_VALUES {
                    values.push("…");
                }
                s.push_str(&format!(" {{{}}}", values.join(", ")));
            }
            if let Some(comment) = &c.comment {
                s.push_str(&format!(" \"{comment}\""));
            }
            s
        })
        .collect();
    if !columns.is_empty() {
        out.push_str(&format!("\n  {}", columns.join(", ")));
    }
    for check in info
        .checks
        .iter()
        .filter(|c| c.schema == t.schema && c.table == t.name)
    {
        out.push_str(&format!("\n  check {}", check.expression));
    }
    out
}

#[cfg(test)]
mod tests {
    use tangul_core::{
        CheckInfo, ColumnMeta, DriverKind, EnumInfo, ForeignKey, SchemaInfo, TableInfo, TableKind,
    };

    use super::digest;
    use crate::dialect::dialect_for;
    use crate::schema::SchemaIndex;

    #[test]
    fn a_table_in_a_few_lines() {
        let col = |name: &str, ty: &str| ColumnMeta {
            schema: "app".into(),
            table: "orders".into(),
            name: name.into(),
            data_type: ty.into(),
            ..Default::default()
        };
        let info = SchemaInfo {
            tables: vec![
                TableInfo {
                    schema: "app".into(),
                    name: "orders".into(),
                    comment: Some("Customer orders".into()),
                    row_estimate: Some(300_000),
                    ..Default::default()
                },
                TableInfo {
                    schema: "app".into(),
                    name: "users".into(),
                    ..Default::default()
                },
                TableInfo {
                    schema: "app".into(),
                    name: "daily".into(),
                    kind: TableKind::MaterializedView,
                    ..Default::default()
                },
            ],
            columns: vec![
                ColumnMeta {
                    primary_key: true,
                    ..col("id", "bigint")
                },
                col("user_id", "integer"),
                ColumnMeta {
                    enum_type: Some("app.status".into()),
                    ..col("status", "app.status")
                },
                ColumnMeta {
                    comment: Some("Day placed".into()),
                    ..col("ordered", "date")
                },
            ],
            foreign_keys: vec![ForeignKey {
                name: "fk".into(),
                schema: "app".into(),
                table: "orders".into(),
                columns: vec!["user_id".into()],
                ref_schema: "app".into(),
                ref_table: "users".into(),
                ref_columns: vec!["id".into()],
            }],
            checks: vec![CheckInfo {
                schema: "app".into(),
                table: "orders".into(),
                name: "c".into(),
                expression: "total >= 0".into(),
            }],
            enums: vec![EnumInfo {
                schema: "app".into(),
                name: "status".into(),
                values: (1..=13).map(|i| format!("s{i}")).collect(),
                comment: None,
            }],
            ..Default::default()
        };
        let s = SchemaIndex::new(
            info,
            dialect_for(DriverKind::Postgres),
            Some("public".into()),
        );
        let orders = s.resolve(Some("app"), "orders").unwrap();
        assert_eq!(
            digest(&s, orders),
            "app.orders (~300000 rows) — Customer orders\n  \
             id bigint pk, user_id integer → app.users.id, \
             status app.status {s1, s2, s3, s4, s5, s6, s7, s8, s9, s10, s11, s12, …}, \
             ordered date \"Day placed\"\n  \
             check total >= 0"
        );
        let daily = s.resolve(Some("app"), "daily").unwrap();
        assert_eq!(digest(&s, daily), "app.daily (materialized view)");
    }
}
