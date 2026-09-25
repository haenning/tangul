//! UPDATE and DELETE statements for result rows. Changing data stays explicit SQL:
//! the grid generates the statement, the user reviews and runs it.
//!
//! Rows are matched by primary key. Without one in the result (the table has none, or
//! the query left a key column out) they are matched by all of their columns, and the
//! statement starts with a warning comment.

use tangul_core::DriverKind;

use crate::related::{literal, numeric_type, result_table};
use crate::schema::SchemaIndex;

/// A table column shown in the result.
#[derive(Debug, Clone, PartialEq)]
struct EditColumn {
    /// Quoted column name.
    sql: String,
    numeric: bool,
}

/// How to write statements against the rows of a single-table result.
#[derive(Debug, Clone, PartialEq)]
pub struct RowEdit {
    /// Short table name, for the tab title.
    pub table: String,
    table_sql: String,
    driver: DriverKind,
    /// Per result column; `None` for expressions and repeated columns.
    columns: Vec<Option<EditColumn>>,
    /// Result columns that identify a row.
    key: Vec<usize>,
    /// Set when rows are not matched by primary key.
    warning: Option<String>,
}

/// Statement builder for a result, if it comes from exactly one known table.
/// `columns` are the result's column names.
pub fn row_edit(sql: &str, columns: &[String], schema: &SchemaIndex) -> Option<RowEdit> {
    let ix = result_table(sql, schema)?;
    let table_cols = schema.columns(ix);
    let mut seen = Vec::new();
    let edit_cols: Vec<Option<EditColumn>> = columns
        .iter()
        .map(|name| {
            let col = table_cols
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(name))?;
            if seen.contains(&col.name) {
                return None;
            }
            seen.push(col.name.clone());
            Some(EditColumn {
                sql: schema.quote(&col.name),
                numeric: numeric_type(&col.data_type),
            })
        })
        .collect();
    let result_col = |name: &str| {
        columns
            .iter()
            .enumerate()
            .position(|(i, c)| c.eq_ignore_ascii_case(name) && edit_cols[i].is_some())
    };
    let pk: Vec<&str> = table_cols
        .iter()
        .filter(|c| c.primary_key)
        .map(|c| c.name.as_str())
        .collect();
    let table = &schema.table(ix).name;
    let pk_cols = pk.iter().map(|c| result_col(c)).collect::<Option<Vec<_>>>();
    let (key, warning) = match pk_cols {
        Some(key) if !key.is_empty() => (key, None),
        _ => {
            let key: Vec<usize> = (0..columns.len())
                .filter(|&i| edit_cols[i].is_some())
                .collect();
            if key.is_empty() {
                return None;
            }
            let why = if pk.is_empty() {
                format!("{table} has no primary key")
            } else {
                format!(
                    "The result lacks the primary key of {table} ({})",
                    pk.join(", ")
                )
            };
            let warning = format!(
                "-- {why}: rows are matched by all their columns,\n\
                 -- so identical rows are affected too. Check before running.\n"
            );
            (key, Some(warning))
        }
    };
    Some(RowEdit {
        table: table.clone(),
        table_sql: schema.sql_name(ix),
        driver: schema.driver(),
        columns: edit_cols,
        key,
        warning,
    })
}

impl RowEdit {
    /// Whether the result column is a table column (and so can be updated).
    pub fn editable(&self, col: usize) -> bool {
        self.columns.get(col).is_some_and(Option::is_some)
    }

    /// Whether rows are matched by primary key.
    pub fn by_primary_key(&self) -> bool {
        self.warning.is_none()
    }

    /// `UPDATE … SET col = <value> WHERE <rows>`: one value for the column of all
    /// rows, starting from `value` (the clicked cell's). `rows` hold all result
    /// columns.
    pub fn update_cell(
        &self,
        col: usize,
        value: Option<&str>,
        rows: &[Vec<Option<&str>>],
    ) -> Option<String> {
        let column = self.columns.get(col)?.as_ref()?;
        Some(format!(
            "{}UPDATE {}\nSET {} = {}\nWHERE {};",
            self.warning(),
            self.table_sql,
            column.sql,
            self.value(column, value),
            self.filter(rows)?
        ))
    }

    /// One `UPDATE` per row, each setting the column to that row's own value (rows
    /// with the same key once).
    pub fn update_cells(&self, col: usize, rows: &[Vec<Option<&str>>]) -> Option<String> {
        let column = self.columns.get(col)?.as_ref()?;
        let mut statements: Vec<String> = Vec::new();
        for row in rows {
            let stmt = format!(
                "UPDATE {} SET {} = {} WHERE {};",
                self.table_sql,
                column.sql,
                self.value(column, row.get(col).copied().flatten()),
                self.filter(std::slice::from_ref(row))?
            );
            if !statements.contains(&stmt) {
                statements.push(stmt);
            }
        }
        (!statements.is_empty()).then(|| format!("{}{}", self.warning(), statements.join("\n")))
    }

    /// `UPDATE … SET` every table column of the row (except the key it is matched
    /// by) to its current value.
    pub fn update_row(&self, row: &[Option<&str>]) -> Option<String> {
        let by_key = self.by_primary_key();
        let sets: Vec<String> = self
            .columns
            .iter()
            .enumerate()
            .filter(|(i, _)| !(by_key && self.key.contains(i)))
            .filter_map(|(i, c)| {
                let c = c.as_ref()?;
                Some(format!(
                    "{} = {}",
                    c.sql,
                    self.value(c, row.get(i).copied().flatten())
                ))
            })
            .collect();
        if sets.is_empty() {
            return None;
        }
        Some(format!(
            "{}UPDATE {}\nSET {}\nWHERE {};",
            self.warning(),
            self.table_sql,
            sets.join(",\n    "),
            self.filter(&[row.to_vec()])?
        ))
    }

    fn warning(&self) -> &str {
        self.warning.as_deref().unwrap_or_default()
    }

    fn value(&self, column: &EditColumn, value: Option<&str>) -> String {
        match value {
            Some(v) => literal(v, column.numeric, self.driver),
            None => "NULL".into(),
        }
    }

    /// `DELETE FROM … WHERE <rows>`. `rows` hold all result columns.
    pub fn delete(&self, rows: &[Vec<Option<&str>>]) -> Option<String> {
        Some(format!(
            "{}DELETE FROM {}\nWHERE {};",
            self.warning(),
            self.table_sql,
            self.filter(rows)?
        ))
    }

    /// Matches exactly the given rows (duplicates once); `None` without rows.
    fn filter(&self, rows: &[Vec<Option<&str>>]) -> Option<String> {
        let mut keys: Vec<Vec<Option<&str>>> = Vec::new();
        for row in rows {
            let key: Vec<Option<&str>> = self
                .key
                .iter()
                .map(|&i| row.get(i).copied().flatten())
                .collect();
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        if keys.is_empty() {
            return None;
        }
        let column = |i: usize| self.columns[self.key[i]].as_ref().expect("key column");
        let lit = |i: usize, v: &str| literal(v, column(i).numeric, self.driver);
        let single_non_null = self.key.len() == 1 && keys.iter().all(|k| k[0].is_some());
        if single_non_null {
            let col = &column(0).sql;
            return Some(match &keys[..] {
                [key] => format!("{col} = {}", lit(0, key[0].unwrap_or_default())),
                keys => format!(
                    "{col} IN ({})",
                    keys.iter()
                        .map(|k| lit(0, k[0].unwrap_or_default()))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
        let conditions: Vec<String> = keys
            .iter()
            .map(|key| {
                key.iter()
                    .enumerate()
                    .map(|(i, v)| match v {
                        Some(v) => format!("{} = {}", column(i).sql, lit(i, v)),
                        None => format!("{} IS NULL", column(i).sql),
                    })
                    .collect::<Vec<_>>()
                    .join(" AND ")
            })
            .collect();
        Some(match &conditions[..] {
            [one] => one.clone(),
            many => many
                .iter()
                .map(|c| format!("({c})"))
                .collect::<Vec<_>>()
                .join("\n   OR "),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::tests::sample;

    fn cols(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn by_primary_key() {
        let s = sample();
        let e = row_edit(
            "select id, email, lower(name) as name from app.users",
            &cols(&["id", "email", "name"]),
            &s,
        )
        .unwrap();
        assert!(e.by_primary_key());
        assert!(e.editable(1));
        // An alias that names a table column counts as that column.
        assert!(e.editable(2));
        let rows = [
            vec![Some("7"), Some("o'k@x"), None],
            vec![Some("8"), None, None],
            vec![Some("7"), Some("o'k@x"), None],
        ];
        assert_eq!(
            e.update_cell(1, Some("o'k@x"), &rows[..1]).unwrap(),
            "UPDATE app.users\nSET email = 'o''k@x'\nWHERE id = 7;"
        );
        assert_eq!(
            e.update_cell(1, None, &rows).unwrap(),
            "UPDATE app.users\nSET email = NULL\nWHERE id IN (7, 8);"
        );
        assert_eq!(
            e.delete(&rows).unwrap(),
            "DELETE FROM app.users\nWHERE id IN (7, 8);"
        );
        assert_eq!(e.delete(&[]), None);
        assert_eq!(
            e.update_cells(1, &rows).unwrap(),
            "UPDATE app.users SET email = 'o''k@x' WHERE id = 7;\n\
             UPDATE app.users SET email = NULL WHERE id = 8;"
        );
        // The key is what matches the row, not something to set.
        assert_eq!(
            e.update_row(&rows[0]).unwrap(),
            "UPDATE app.users\nSET email = 'o''k@x',\n    name = NULL\nWHERE id = 7;"
        );
    }

    #[test]
    fn falls_back_to_all_columns_with_a_warning() {
        let s = sample();
        let e = row_edit(
            "select user_id, total, 1 + 1 from app.orders",
            &cols(&["user_id", "total", "?column?"]),
            &s,
        )
        .unwrap();
        assert!(!e.by_primary_key());
        assert!(!e.editable(2));
        let sql = e
            .delete(&[
                vec![Some("3"), None, Some("2")],
                vec![Some("4"), Some("9.5"), Some("2")],
            ])
            .unwrap();
        assert_eq!(
            sql,
            "-- The result lacks the primary key of orders (id): rows are matched by all their columns,\n\
             -- so identical rows are affected too. Check before running.\n\
             DELETE FROM app.orders\n\
             WHERE (user_id = 3 AND total IS NULL)\n   OR (user_id = 4 AND total = '9.5');"
        );
    }

    #[test]
    fn needs_one_known_table() {
        let s = sample();
        assert!(row_edit("select 1", &cols(&["?column?"]), &s).is_none());
        assert!(
            row_edit(
                "select * from app.orders o join app.users u on u.id = o.user_id",
                &cols(&["id"]),
                &s
            )
            .is_none()
        );
        // Only expressions: nothing to match rows by.
        assert!(row_edit("select count(*) from app.users", &cols(&["count"]), &s).is_none());
    }
}
