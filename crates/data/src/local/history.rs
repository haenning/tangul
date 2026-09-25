//! Every executed script, with timing, rows and error.

use anyhow::Result;
use rusqlite::{OptionalExtension as _, params};
use tangul_assist::suggest::HistoryPattern;

use super::{HistoryEntry, Local, NewHistory};

impl Local {
    pub fn add_history(&self, h: &NewHistory) -> Result<()> {
        self.conn.execute(
            "INSERT INTO history
               (database_id, user_id, sql, fingerprint, started_at, duration_ms, rows, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                h.database_id,
                h.user_id,
                h.sql,
                h.fingerprint,
                h.entry.started_at,
                h.entry.duration_ms,
                h.entry.rows,
                h.entry.error,
            ],
        )?;
        Ok(())
    }

    /// Most recent runs for a database, optionally filtered by a substring.
    pub fn history(&self, db_id: &str, search: &str, limit: usize) -> Vec<HistoryEntry> {
        let pattern = format!(
            "%{}%",
            search
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let result = self
            .conn
            .prepare_cached(
                "SELECT sql, started_at, duration_ms, rows, error FROM history
                 WHERE database_id = ?1 AND sql LIKE ?2 ESCAPE '\\'
                 ORDER BY started_at DESC LIMIT ?3",
            )
            .and_then(|mut stmt| {
                stmt.query_map(params![db_id, pattern, limit as i64], |r| {
                    Ok(HistoryEntry {
                        sql: r.get(0)?,
                        started_at: r.get(1)?,
                        duration_ms: r.get(2)?,
                        rows: r.get(3)?,
                        error: r.get(4)?,
                    })
                })?
                .collect()
            });
        result.unwrap_or_else(|e| {
            eprintln!("history query failed: {e}");
            Vec::new()
        })
    }

    /// The user who last ran something on the database.
    pub fn last_user(&self, db_id: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT user_id FROM history WHERE database_id = ?1
                 ORDER BY started_at DESC LIMIT 1",
                params![db_id],
                |r| r.get(0),
            )
            .optional()
            .ok()?
    }

    /// Successful runs of queries with this fingerprint.
    pub fn runs(&self, db_id: &str, fingerprint: &str) -> u32 {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM history
                 WHERE database_id = ?1 AND fingerprint = ?2 AND error IS NULL",
                params![db_id, fingerprint],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }

    /// Distinct successful queries, most frequent first, each with its latest SQL.
    pub fn patterns(&self, db_id: &str, limit: usize) -> Vec<HistoryPattern> {
        let result = self
            .conn
            .prepare_cached(
                "SELECT (SELECT h2.sql FROM history h2
                         WHERE h2.database_id = h.database_id AND h2.fingerprint = h.fingerprint
                         ORDER BY h2.started_at DESC LIMIT 1),
                        COUNT(*)
                 FROM history h
                 WHERE h.database_id = ?1 AND h.error IS NULL
                 GROUP BY h.fingerprint
                 ORDER BY COUNT(*) DESC, MAX(h.started_at) DESC
                 LIMIT ?2",
            )
            .and_then(|mut stmt| {
                stmt.query_map(params![db_id, limit as i64], |r| {
                    Ok(HistoryPattern {
                        sql: r.get(0)?,
                        runs: r.get(1)?,
                    })
                })?
                .collect()
            });
        result.unwrap_or_else(|e| {
            eprintln!("history query failed: {e}");
            Vec::new()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(local: &Local, sql: &str, at: i64, error: Option<&str>) {
        local
            .add_history(&NewHistory {
                database_id: "db",
                user_id: "u",
                sql,
                fingerprint: &tangul_assist::lexer::fingerprint(sql),
                entry: HistoryEntry {
                    sql: sql.into(),
                    started_at: at,
                    duration_ms: 3,
                    rows: Some(1),
                    error: error.map(Into::into),
                },
            })
            .unwrap();
    }

    #[test]
    fn history_patterns_and_search() {
        let l = Local::in_memory();
        run(&l, "select * from t where id = 1", 1, None);
        run(&l, "select * from t where id = 2", 2, None);
        run(&l, "select 100%", 3, None);
        run(&l, "select broken", 4, Some("boom"));
        let p = l.patterns("db", 10);
        assert_eq!(
            (p[0].sql.as_str(), p[0].runs),
            ("select * from t where id = 2", 2)
        );
        assert_eq!(p.len(), 2);
        assert_eq!(
            l.runs(
                "db",
                &tangul_assist::lexer::fingerprint("select * from t where id = 9")
            ),
            2
        );
        assert_eq!(l.history("db", "", 10)[0].error.as_deref(), Some("boom"));
        assert_eq!(l.last_user("db").as_deref(), Some("u"));
        assert_eq!(l.last_user("other"), None);
        assert_eq!(l.history("db", "%", 10).len(), 1);
    }
}
