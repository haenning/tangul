//! What a database's server has for the performance boards
//! ([`tangul_assist::perf`]): its version and whether statement statistics are
//! there, read once as a board's read (through the gate as `Caller::Board`). The
//! boards themselves are created with [`crate::boards::create_from`].

use tangul_assist::perf::Facts;
use tangul_core::{ConnId, QueryId};
use tangul_engine::{Caller, EngineHandle, QueryOptions};

/// The facts of the server behind `conn`, from its dialect's facts query `sql`
/// ([`tangul_assist::dialect::Dialect::perf_facts_sql`]). The error is the server's
/// message.
pub async fn facts(
    engine: &EngineHandle,
    query: QueryId,
    conn: ConnId,
    sql: &str,
) -> Result<Facts, String> {
    let out = engine
        .query(query, conn, sql, QueryOptions::new(Caller::Board))
        .await
        .map_err(|e| e.message)?;
    let row = out
        .results
        .last()
        .filter(|r| r.row_count() > 0)
        .ok_or("the server answered without a row")?;
    let cells: Vec<(&str, Option<&str>)> = (row.columns().iter().enumerate())
        .map(|(c, col)| (col.name.as_str(), row.cell(0, c)))
        .collect();
    Ok(Facts::read(&cells))
}

#[cfg(test)]
mod tests {
    use tangul_assist::perf::Stats;

    use super::*;
    use crate::connections::Connections;

    /// Through the real engine and gate, on a SQLite file: the row is read as the
    /// facts; a query without a row, one that fails or one that writes is an error.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_facts_are_one_row_read_as_a_board_reads() {
        let dir = std::env::temp_dir().join(format!("tangul-perf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Any SQLite file: tangul's own local.db.
        drop(tangul_data::local::Local::open(Some(&dir)));
        let path = dir.join("local.db").to_string_lossy().into_owned();
        let (engine, _events) = tangul_engine::start_with(tangul_engine::EngineConfig::isolated());
        let (conn, _) = Connections::default().open_file(&engine, &path);
        let ask = async |n: u64, sql: &str| facts(&engine, QueryId(n), conn, sql).await;

        let facts = ask(
            1,
            "SELECT 16 AS major, 4 AS minor, 0 AS mariadb, 'not loaded' AS statements",
        )
        .await
        .unwrap();
        assert_eq!(facts.version, (16, 4));
        assert_eq!(facts.statements, Stats::NotLoaded);
        assert!(
            ask(2, "SELECT 1 AS major WHERE 0")
                .await
                .unwrap_err()
                .contains("without a row")
        );
        assert!(ask(3, "SELECT * FROM missing").await.is_err());
        assert!(ask(4, "CREATE TABLE t (x INT)").await.is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
