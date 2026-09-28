//! Every script run on a database, with timing, rows and error, and who ran it
//! (you, the assistant, an MCP client). What is learned from it (patterns, the
//! bookmark hint, the last user) comes from your own runs only.

use anyhow::Result;
use rusqlite::{OptionalExtension as _, params};
use tangul_core::usage::HistoryPattern;

use super::{HistoryEntry, Local, NewHistory, Source};

impl Local {
    pub fn add_history(&self, h: &NewHistory) -> Result<()> {
        self.conn.execute(
            "INSERT INTO history
               (database_id, user_id, sql, fingerprint, started_at, duration_ms, rows, error,
                source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                h.database_id,
                h.user_id,
                h.sql,
                h.fingerprint,
                h.entry.started_at,
                h.entry.duration_ms,
                h.entry.rows,
                h.entry.error,
                h.entry.source.as_str(),
            ],
        )?;
        Ok(())
    }

    /// Most recent runs for a database, optionally filtered by a substring and by
    /// who ran them (`sources`; empty: everyone).
    pub fn history(
        &self,
        db_id: &str,
        search: &str,
        sources: &[Source],
        limit: usize,
    ) -> Vec<HistoryEntry> {
        let sources: Vec<&str> = match sources {
            [] => Source::ALL.iter().map(|s| s.as_str()).collect(),
            some => some.iter().map(|s| s.as_str()).collect(),
        };
        let sources = serde_json::to_string(&sources).unwrap_or_default();
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
                "SELECT sql, started_at, duration_ms, rows, error, source FROM history
                 WHERE database_id = ?1 AND sql LIKE ?2 ESCAPE '\\'
                   AND source IN (SELECT value FROM json_each(?4))
                 ORDER BY started_at DESC LIMIT ?3",
            )
            .and_then(|mut stmt| {
                stmt.query_map(params![db_id, pattern, limit as i64, sources], |r| {
                    Ok(HistoryEntry {
                        source: Source::parse(&r.get::<_, String>(5)?),
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
                "SELECT user_id FROM history WHERE database_id = ?1 AND source = 'app'
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
                 WHERE database_id = ?1 AND fingerprint = ?2 AND error IS NULL
                   AND source = 'app'",
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
                           AND h2.source = 'app'
                         ORDER BY h2.started_at DESC LIMIT 1),
                        COUNT(*)
                 FROM history h
                 WHERE h.database_id = ?1 AND h.error IS NULL AND h.source = 'app'
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

    /// `fingerprint` stands in for the real one (the same for queries that differ
    /// only in values).
    fn run(local: &Local, sql: &str, fingerprint: &str, at: i64, error: Option<&str>) {
        run_by(local, Source::App, sql, fingerprint, at, error);
    }

    fn run_by(
        local: &Local,
        source: Source,
        sql: &str,
        fingerprint: &str,
        at: i64,
        error: Option<&str>,
    ) {
        local
            .add_history(&NewHistory {
                database_id: "db",
                user_id: "u",
                sql,
                fingerprint,
                entry: HistoryEntry {
                    sql: sql.into(),
                    source,
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
        run(&l, "select * from t where id = 1", "t by id", 1, None);
        run(&l, "select * from t where id = 2", "t by id", 2, None);
        run(&l, "select 100%", "percent", 3, None);
        run(&l, "select broken", "broken", 4, Some("boom"));
        let p = l.patterns("db", 10);
        assert_eq!(
            (p[0].sql.as_str(), p[0].runs),
            ("select * from t where id = 2", 2)
        );
        assert_eq!(p.len(), 2);
        assert_eq!(l.runs("db", "t by id"), 2);
        assert_eq!(
            l.history("db", "", &[], 10)[0].error.as_deref(),
            Some("boom")
        );
        assert_eq!(l.last_user("db").as_deref(), Some("u"));
        assert_eq!(l.last_user("other"), None);
        assert_eq!(l.history("db", "%", &[], 10).len(), 1);
    }

    #[test]
    fn everyone_s_runs_filtered_by_who_ran_them() {
        let l = Local::in_memory();
        run(&l, "select 1", "one", 1, None);
        run_by(&l, Source::Mcp, "select 2", "two", 2, None);
        run_by(&l, Source::Mcp, "select 2", "two", 3, None);
        run_by(&l, Source::Ai, "select 3", "three", 4, Some("boom"));
        let seen = |sources: &[Source]| -> Vec<(Source, String)> {
            l.history("db", "", sources, 10)
                .into_iter()
                .map(|e| (e.source, e.sql))
                .collect()
        };
        // None chosen: everything, newest first.
        assert_eq!(
            seen(&[]),
            [
                (Source::Ai, "select 3".to_string()),
                (Source::Mcp, "select 2".into()),
                (Source::Mcp, "select 2".into()),
                (Source::App, "select 1".into()),
            ]
        );
        assert_eq!(seen(&[Source::App]), [(Source::App, "select 1".into())]);
        assert_eq!(seen(&[Source::App, Source::Ai]).len(), 2);
        assert_eq!(seen(&[Source::Mcp]).len(), 2);
        // Only your runs are learned from.
        let p = l.patterns("db", 10);
        assert_eq!((p.len(), p[0].sql.as_str()), (1, "select 1"));
        assert_eq!(l.runs("db", "two"), 0);
    }
}
