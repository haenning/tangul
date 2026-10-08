//! The editor's quick look at a column: its most frequent values among the first
//! few hundred rows ([`tangul_assist::profile::quick_values_sql`]), so they can be
//! offered while a condition is typed. Cheap on any table size: the server reads at
//! most [`QUICK_ROWS`](tangul_assist::profile::QUICK_ROWS) rows. Never on views: one
//! that sorts, groups or deduplicates is computed in full before any limit applies.
//! Runs through the gate as `Caller::Editor` (reads only, rolled back).

use tangul_assist::profile::{QUICK_VALUES, VALUE_CHARS, quick_values_sql};
use tangul_assist::schema::SchemaIndex;
use tangul_core::{ColumnMeta, ConnId, QueryId, TableKind};
use tangul_engine::{Caller, EngineHandle, QueryOptions};

/// The most frequent values of `column` on `conn`, most frequent first; values
/// longer than [`VALUE_CHARS`] are left out (a cut one would never match). None
/// for a view (not looked at). The error is the server's message.
pub async fn quick_values(
    engine: &EngineHandle,
    query: QueryId,
    conn: ConnId,
    schema: &SchemaIndex,
    column: &ColumnMeta,
) -> Result<Vec<String>, String> {
    let table = schema
        .resolve(Some(&column.schema), &column.table)
        .ok_or_else(|| format!("no table {}", column.table))?;
    if schema.table(table).kind == TableKind::View {
        return Ok(Vec::new());
    }
    let sql = quick_values_sql(schema.dialect(), &schema.sql_name(table), &column.name);
    let options = QueryOptions {
        row_limit: QUICK_VALUES,
        ..QueryOptions::new(Caller::Editor)
    };
    let out = engine
        .query(query, conn, sql, options)
        .await
        .map_err(|e| e.message)?;
    Ok(out
        .results
        .last()
        .map(|t| {
            (0..t.row_count())
                .filter_map(|r| t.cell(r, 0))
                .filter(|v| v.chars().count() <= VALUE_CHARS)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use tangul_assist::dialect::dialect_for;
    use tangul_core::{DriverKind, SchemaInfo, TableInfo};
    use tangul_data::local::{HistoryEntry, Local, NewHistory, Source};

    use super::*;
    use crate::connections::Connections;

    #[tokio::test(flavor = "multi_thread")]
    async fn the_most_frequent_values_of_a_column() {
        let dir = std::env::temp_dir().join(format!("tangul-values-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let local = Local::open(Some(&dir));
        let long = format!("select '{}'", "x".repeat(64));
        for (sql, n) in [
            ("select 1", 3),
            ("select 2", 1),
            ("select 3", 2),
            (long.as_str(), 5),
        ] {
            for _ in 0..n {
                let entry = NewHistory {
                    database_id: "db",
                    user_id: "u",
                    sql,
                    fingerprint: sql,
                    entry: HistoryEntry {
                        sql: sql.into(),
                        source: Source::App,
                        started_at: 0,
                        duration_ms: 0,
                        rows: None,
                        error: None,
                    },
                };
                local.add_history(&entry).unwrap();
            }
        }
        drop(local);
        let path = dir.join("local.db").to_string_lossy().into_owned();

        let (engine, _events) = tangul_engine::start_with(tangul_engine::EngineConfig::isolated());
        let (conn, _) = Connections::default().open_file(&engine, &path);
        let column = ColumnMeta {
            schema: "main".into(),
            table: "history".into(),
            name: "sql".into(),
            data_type: "TEXT".into(),
            ..Default::default()
        };
        let schema = SchemaIndex::new(
            SchemaInfo {
                tables: vec![
                    TableInfo {
                        schema: "main".into(),
                        name: "history".into(),
                        kind: TableKind::Table,
                        ..Default::default()
                    },
                    TableInfo {
                        schema: "main".into(),
                        name: "history_view".into(),
                        kind: TableKind::View,
                        ..Default::default()
                    },
                ],
                columns: vec![column.clone()],
                ..Default::default()
            },
            dialect_for(DriverKind::Sqlite),
            Some("main".into()),
        );
        let values = quick_values(&engine, QueryId(1), conn, &schema, &column)
            .await
            .unwrap();
        assert_eq!(values, ["select 1", "select 3", "select 2"]);

        // A view isn't looked at: this one doesn't even exist on the server.
        let view = ColumnMeta {
            table: "history_view".into(),
            ..column.clone()
        };
        assert!(
            quick_values(&engine, QueryId(3), conn, &schema, &view)
                .await
                .unwrap()
                .is_empty()
        );

        let gone = ColumnMeta {
            table: "nope".into(),
            ..column
        };
        assert!(
            quick_values(&engine, QueryId(2), conn, &schema, &gone)
                .await
                .is_err()
        );
    }
}
