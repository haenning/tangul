//! Following foreign keys from result rows: when a result comes from a single table,
//! its rows can open the rows they reference (outgoing keys) and the rows that
//! reference them (incoming keys), filtered by the exact key values.

use rusdb_core::DriverKind;

use crate::analyze::{analyze, statements};
use crate::schema::SchemaIndex;

/// Most rows turned into one filter; more would make an unwieldy query.
const MAX_KEYS: usize = 1000;

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
    driver: DriverKind,
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
        let lit = |i: usize, v: &str| literal(v, self.numeric[i], self.driver);
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
    // The grid shows the last statement's result.
    let Some(stmt) = statements(sql)
        .into_iter()
        .map(|(s, e)| &sql[s..e])
        .rfind(|s| !s.trim().is_empty())
    else {
        return Vec::new();
    };
    let refs = analyze(stmt).tables;
    let [table_ref] = &refs[..] else {
        return Vec::new();
    };
    let Some(ix) = schema.resolve(table_ref.schema.as_deref(), &table_ref.name) else {
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
    let driver = schema.driver();

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
            let name = schema.table(target).name.clone();
            let arrow = if incoming { "=" } else { "→" };
            let link = RelatedLink {
                title: format!("{name} ({} {arrow} {})", theirs.join(", "), own.join(", ")),
                incoming,
                table: name,
                target_sql: schema.sql_name(target),
                target_cols: theirs.iter().map(|c| schema.quote(c)).collect(),
                source_cols,
                numeric: own.iter().map(|c| is_numeric(c)).collect(),
                driver,
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

fn numeric_type(data_type: &str) -> bool {
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

/// A SQL literal for a text-protocol value: numbers as they are (for numeric
/// columns), everything else as a quoted string.
fn literal(value: &str, numeric: bool, driver: DriverKind) -> String {
    let plain_number = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))
        && value.parse::<f64>().is_ok_and(f64::is_finite);
    if numeric && plain_number {
        return value.to_string();
    }
    let mut escaped = value.replace('\'', "''");
    if driver == DriverKind::Mysql {
        escaped = escaped.replace('\\', "\\\\");
    }
    format!("'{escaped}'")
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
    fn literals_per_type_and_dialect() {
        assert_eq!(literal("12", true, DriverKind::Postgres), "12");
        assert_eq!(literal("12", false, DriverKind::Postgres), "'12'");
        assert_eq!(literal("1; drop", true, DriverKind::Postgres), "'1; drop'");
        assert_eq!(literal("o'k\\", false, DriverKind::Postgres), "'o''k\\'");
        assert_eq!(literal("o'k\\", false, DriverKind::Mysql), "'o''k\\\\'");
        assert!(numeric_type("bigint") && numeric_type("int(11)") && !numeric_type("text"));
        let _ = sample_for(DriverKind::Mysql);
    }
}
