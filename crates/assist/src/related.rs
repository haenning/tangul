//! Following foreign keys from result rows: when a result comes from a single table,
//! its rows can open the rows they reference (outgoing keys) and the rows that
//! reference them (incoming keys), filtered by the exact key values.

use crate::analyze::{analyze, statements};
use crate::dialect::{Dialect, literal};
use crate::schema::SchemaIndex;
use crate::usage::{Usage, UsageKind, join_key, table_key};

/// Most rows turned into one filter; more would make an unwieldy query.
const MAX_KEYS: usize = 1000;
/// Having joined over a link's key says more than having queried its target table.
const JOIN_WEIGHT: f64 = 3.0;
/// A bookmark counts like this many recent runs.
const BOOKMARK_WEIGHT: f64 = 5.0;

/// A table reachable from the result's table through a foreign key.
#[derive(Debug, Clone, PartialEq)]
pub struct RelatedLink {
    /// E.g. `users (user_id → id)` or `orders (user_id = id)`.
    pub title: String,
    /// The target references the result's table (rather than the other way round).
    pub incoming: bool,
    /// Short target table name, for the tab title.
    pub table: String,
    target_sql: String,
    target_cols: Vec<String>,
    /// Result column index per key column.
    source_cols: Vec<usize>,
    numeric: Vec<bool>,
    dialect: &'static dyn Dialect,
    /// Usage key of the target table.
    target_key: String,
    /// Usage key per key column pair (a join over the link).
    join_keys: Vec<String>,
}

impl RelatedLink {
    /// Result columns whose values the query needs, in key order.
    pub fn source_cols(&self) -> &[usize] {
        &self.source_cols
    }

    /// The query for the given rows; each row holds the values of [`Self::source_cols`].
    /// Rows with a NULL key are skipped; `None` if none are left.
    pub fn query(&self, rows: &[Vec<Option<&str>>]) -> Option<String> {
        let mut keys: Vec<Vec<&str>> = Vec::new();
        for row in rows {
            let Some(key) = row.iter().copied().collect::<Option<Vec<&str>>>() else {
                continue;
            };
            if !keys.contains(&key) {
                keys.push(key);
            }
            if keys.len() == MAX_KEYS {
                break;
            }
        }
        if keys.is_empty() {
            return None;
        }
        let lit = |i: usize, v: &str| literal(self.dialect, v, self.numeric[i]);
        let filter = if let [col] = &self.target_cols[..] {
            match &keys[..] {
                [key] => format!("{col} = {}", lit(0, key[0])),
                keys => format!(
                    "{col} IN ({})",
                    keys.iter()
                        .map(|k| lit(0, k[0]))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        } else {
            keys.iter()
                .map(|key| {
                    let parts: Vec<String> = self
                        .target_cols
                        .iter()
                        .enumerate()
                        .map(|(i, col)| format!("{col} = {}", lit(i, key[i])))
                        .collect();
                    format!("({})", parts.join(" AND "))
                })
                .collect::<Vec<_>>()
                .join("\n   OR ")
        };
        Some(format!(
            "SELECT *\nFROM {}\nWHERE {filter}",
            self.target_sql
        ))
    }
}

/// The foreign-key links of a result, if it comes from exactly one known table and
/// contains the key columns. `columns` are the result's column names.
pub fn related_links(sql: &str, columns: &[String], schema: &SchemaIndex) -> Vec<RelatedLink> {
    let Some(ix) = result_table(sql, schema) else {
        return Vec::new();
    };
    let result_col = |name: &str| columns.iter().position(|c| c.eq_ignore_ascii_case(name));
    let is_numeric = |name: &str| {
        schema
            .columns(ix)
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
            .is_some_and(|c| numeric_type(&c.data_type))
    };
    let dialect = schema.dialect();
    let own_table = schema.table(ix);

    let mut links = Vec::new();
    for fk in schema.foreign_keys() {
        let (Some(child), Some(parent)) = (
            schema.resolve(Some(&fk.schema), &fk.table),
            schema.resolve(Some(&fk.ref_schema), &fk.ref_table),
        ) else {
            continue;
        };
        // (target table, key columns in the result's table, key columns in the target)
        let mut sides = Vec::new();
        if child == ix {
            sides.push((false, parent, &fk.columns, &fk.ref_columns));
        }
        if parent == ix {
            sides.push((true, child, &fk.ref_columns, &fk.columns));
        }
        for (incoming, target, own, theirs) in sides {
            let Some(source_cols) = own
                .iter()
                .map(|c| result_col(c))
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            let target_table = schema.table(target);
            let name = target_table.name.clone();
            let join_keys = own
                .iter()
                .zip(theirs.iter())
                .map(|(o, t)| {
                    join_key(
                        (&own_table.schema, &own_table.name, o),
                        (&target_table.schema, &target_table.name, t),
                    )
                })
                .collect();
            let arrow = if incoming { "=" } else { "→" };
            let link = RelatedLink {
                title: format!("{name} ({} {arrow} {})", theirs.join(", "), own.join(", ")),
                incoming,
                table: name,
                target_sql: schema.sql_name(target),
                target_cols: theirs.iter().map(|c| schema.quote(c)).collect(),
                source_cols,
                numeric: own.iter().map(|c| is_numeric(c)).collect(),
                dialect,
                target_key: table_key(&target_table.schema, &target_table.name),
                join_keys,
            };
            if !links.contains(&link) {
                links.push(link);
            }
        }
    }
    // References first (usually one row each), then referencing tables.
    links.sort_by_key(|l| l.incoming);
    links
}

/// Sorts links by how much the user works with them: joins over the link's key and
/// queries of its target table, from the history (`usage`) and from bookmarks
/// (`bookmarked`: what [`crate::usage::learn`] finds in each bookmark's SQL).
/// Ties keep the given order.
pub fn rank_links(links: &mut [RelatedLink], usage: &Usage, bookmarked: &[(UsageKind, String)]) {
    let score = |kind: UsageKind, key: &str| {
        let marks = bookmarked
            .iter()
            .filter(|(k, s)| *k == kind && s == key)
            .count();
        usage.score(kind, key) + BOOKMARK_WEIGHT * marks as f64
    };
    let rank = |l: &RelatedLink| {
        let joins: f64 = l.join_keys.iter().map(|k| score(UsageKind::Join, k)).sum();
        JOIN_WEIGHT * joins + score(UsageKind::Table, &l.target_key)
    };
    links.sort_by(|a, b| rank(b).total_cmp(&rank(a)));
}

/// The table a result comes from, if the script's last statement (the one the grid
/// shows) reads exactly one known table.
pub(crate) fn result_table(sql: &str, schema: &SchemaIndex) -> Option<usize> {
    let stmt = statements(sql)
        .into_iter()
        .map(|(s, e)| &sql[s..e])
        .rfind(|s| !s.trim().is_empty())?;
    let refs = analyze(stmt).tables;
    let [table_ref] = &refs[..] else {
        return None;
    };
    schema.resolve(table_ref.schema.as_deref(), &table_ref.name)
}

pub(crate) fn numeric_type(data_type: &str) -> bool {
    let t = data_type.to_ascii_lowercase();
    [
        "int",
        "bigint",
        "smallint",
        "tinyint",
        "mediumint",
        "numeric",
        "decimal",
        "real",
        "double",
        "float",
        "serial",
        "bigserial",
        "smallserial",
    ]
    .iter()
    .any(|p| t.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::{sample, sample_for};

    fn cols(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn links_both_directions() {
        let s = sample();
        let orders = related_links(
            "select * from app.orders",
            &cols(&["id", "user_id", "total"]),
            &s,
        );
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].title, "users (id → user_id)");
        assert_eq!(orders[0].source_cols(), [1]);
        assert_eq!(
            orders[0].query(&[vec![Some("7")]]).unwrap(),
            "SELECT *\nFROM app.users\nWHERE id = 7"
        );

        let users = related_links(
            "SELECT id, email FROM app.users u WHERE id > 3",
            &cols(&["id", "email"]),
            &s,
        );
        assert_eq!(users[0].title, "orders (user_id = id)");
        assert!(users[0].incoming);
        assert_eq!(
            users[0]
                .query(&[
                    vec![Some("1")],
                    vec![Some("2")],
                    vec![Some("1")],
                    vec![None]
                ])
                .unwrap(),
            "SELECT *\nFROM app.orders\nWHERE user_id IN (1, 2)"
        );
        assert_eq!(users[0].query(&[vec![None]]), None);
    }

    #[test]
    fn needs_one_table_and_the_key_columns() {
        let s = sample();
        assert!(related_links("select total from app.orders", &cols(&["total"]), &s).is_empty());
        assert!(
            related_links(
                "select * from app.orders o join app.users u on u.id = o.user_id",
                &cols(&["id", "user_id"]),
                &s
            )
            .is_empty()
        );
        assert!(related_links("select 1", &cols(&["?column?"]), &s).is_empty());
        // Last statement counts.
        assert_eq!(
            related_links(
                "select 1; select * from app.orders;",
                &cols(&["user_id"]),
                &s
            )
            .len(),
            1
        );
    }

    #[test]
    fn ranks_by_joins_tables_and_bookmarks() {
        use tangul_core::{ColumnMeta, ForeignKey, SchemaInfo, TableInfo, TableKind};
        let table = |name: &str| TableInfo {
            schema: "public".into(),
            name: name.into(),
            kind: TableKind::Table,
        };
        let col = |table: &str, name: &str| ColumnMeta {
            schema: "public".into(),
            table: table.into(),
            name: name.into(),
            data_type: "integer".into(),
            nullable: false,
            primary_key: name == "id",
        };
        let fk = |table: &str| ForeignKey {
            name: format!("{table}_fk"),
            schema: "public".into(),
            table: table.into(),
            columns: vec!["user_id".into()],
            ref_schema: "public".into(),
            ref_table: "users".into(),
            ref_columns: vec!["id".into()],
        };
        let names = ["users", "logins", "orders", "tickets"];
        let s = SchemaIndex::new(
            SchemaInfo {
                tables: names.iter().map(|n| table(n)).collect(),
                columns: std::iter::once(col("users", "id"))
                    .chain(
                        names[1..]
                            .iter()
                            .flat_map(|t| [col(t, "id"), col(t, "user_id")]),
                    )
                    .collect(),
                foreign_keys: names[1..].iter().map(|t| fk(t)).collect(),
            },
            crate::dialect::dialect_for(tangul_core::DriverKind::Postgres),
            Some("public".into()),
        );
        let mut links = related_links("select * from users", &cols(&["id"]), &s);
        let order =
            |links: &[RelatedLink]| links.iter().map(|l| l.table.clone()).collect::<Vec<_>>();
        assert_eq!(order(&links), ["logins", "orders", "tickets"]);

        let mut usage = Usage::new(0);
        // Queried tickets twice, joined orders once: the join weighs more.
        usage.record(UsageKind::Table, table_key("public", "tickets"));
        usage.record(UsageKind::Table, table_key("public", "tickets"));
        usage.record(
            UsageKind::Join,
            join_key(("public", "users", "id"), ("public", "orders", "user_id")),
        );
        rank_links(&mut links, &usage, &[]);
        assert_eq!(order(&links), ["orders", "tickets", "logins"]);

        // A bookmark on logins outweighs both.
        let bookmarked = crate::usage::learn("select * from logins where user_id = 1", &s);
        rank_links(&mut links, &usage, &bookmarked);
        assert_eq!(order(&links), ["logins", "orders", "tickets"]);
    }

    #[test]
    fn numeric_types() {
        assert!(numeric_type("bigint") && numeric_type("int(11)") && !numeric_type("text"));
        let _ = sample_for(tangul_core::DriverKind::Mysql);
    }
}
