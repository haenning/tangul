//! `metrics.db` next to `local.db`: duration and row count of every bookmark run,
//! by hand or scheduled. A file of its own, so its size limit and the read-only
//! query tab (`DriverKind::Sqlite`) only ever touch metrics.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use rusqlite::{Connection, params};

use crate::settings::MonitoringSettings;

const FILE: &str = "metrics.db";

/// Rows deleted per round while the file is over its size limit.
const PRUNE_BATCH: i64 = 500;

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS bookmark_runs (
        id INTEGER PRIMARY KEY,
        bookmark_id TEXT NOT NULL,
        -- Name when it ran, so queries over several bookmarks read well.
        bookmark_name TEXT NOT NULL,
        -- Unix milliseconds.
        started_at INTEGER NOT NULL,
        duration_ms INTEGER NOT NULL,
        rows INTEGER,
        error TEXT,
        scheduled INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS bookmark_runs_time ON bookmark_runs (bookmark_id, started_at);
";

/// One run of a bookmark.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub bookmark_id: String,
    pub bookmark_name: String,
    pub started_at: i64,
    pub duration_ms: i64,
    pub rows: Option<i64>,
    pub error: Option<String>,
    pub scheduled: bool,
}

pub struct Metrics {
    conn: Connection,
    /// `None` when the file could not be opened (then metrics live in memory and
    /// can't be queried from a tab).
    path: Option<PathBuf>,
}

impl Metrics {
    pub fn open(dir: Option<&Path>) -> Self {
        let opened = dir.and_then(|dir| {
            std::fs::create_dir_all(dir).ok()?;
            let path = dir.join(FILE);
            Connection::open(&path)
                .map_err(|e| eprintln!("could not open {FILE}: {e}"))
                .ok()
                .map(|conn| (conn, path))
        });
        let (conn, path) = match opened {
            Some((conn, path)) => (conn, Some(path)),
            None => (
                Connection::open_in_memory().expect("in-memory sqlite"),
                None,
            ),
        };
        // Before any table exists, so deleted runs give their space back.
        let setup = conn
            .pragma_update(None, "auto_vacuum", "INCREMENTAL")
            .and_then(|()| conn.execute_batch(SCHEMA));
        if let Err(e) = setup {
            eprintln!("{FILE} setup failed: {e}");
        }
        Self { conn, path }
    }

    #[cfg(test)]
    fn in_memory() -> Self {
        Self::open(None)
    }

    /// The file, for opening it read-only in a query tab.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Stores a run, then drops what the limits no longer allow.
    pub fn record(&self, run: &Run, limits: &MonitoringSettings, now_ms: i64) {
        let result = self.conn.execute(
            "INSERT INTO bookmark_runs
               (bookmark_id, bookmark_name, started_at, duration_ms, rows, error, scheduled)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                run.bookmark_id,
                run.bookmark_name,
                run.started_at,
                run.duration_ms,
                run.rows,
                run.error,
                run.scheduled,
            ],
        );
        if let Err(e) = result {
            eprintln!("could not record bookmark run: {e}");
            return;
        }
        self.prune(limits, now_ms);
    }

    /// Start of the bookmark's latest run, by hand or scheduled.
    pub fn last_run(&self, bookmark_id: &str) -> Option<i64> {
        self.conn
            .query_row(
                "SELECT max(started_at) FROM bookmark_runs WHERE bookmark_id = ?1",
                params![bookmark_id],
                |r| r.get(0),
            )
            .ok()
            .flatten()
    }

    pub fn delete_bookmark(&self, bookmark_id: &str) {
        let result = self.conn.execute(
            "DELETE FROM bookmark_runs WHERE bookmark_id = ?1",
            params![bookmark_id],
        );
        if let Err(e) = result {
            eprintln!("could not delete bookmark runs: {e}");
        }
    }

    /// Drops runs past the age limit, beyond the per-bookmark limit (oldest first)
    /// and, while the data is over the size limit, the oldest of all.
    pub fn prune(&self, limits: &MonitoringSettings, now_ms: i64) {
        if let Err(e) = self.try_prune(limits, now_ms) {
            eprintln!("could not prune bookmark runs: {e}");
        }
    }

    fn try_prune(&self, limits: &MonitoringSettings, now_ms: i64) -> Result<()> {
        let mut deleted = 0;
        if let Some(age) = limits.max_age {
            let cutoff = now_ms.saturating_sub(millis(age));
            deleted += self.conn.execute(
                "DELETE FROM bookmark_runs WHERE started_at < ?1",
                params![cutoff],
            )?;
        }
        if let Some(points) = limits.max_points {
            deleted += self.conn.execute(
                "DELETE FROM bookmark_runs WHERE id IN (
                   SELECT id FROM (
                     SELECT id, row_number() OVER (
                       PARTITION BY bookmark_id ORDER BY started_at DESC, id DESC
                     ) AS n
                     FROM bookmark_runs
                   ) WHERE n > ?1
                 )",
                params![points as i64],
            )?;
        }
        if let Some(mb) = limits.max_mb {
            let max = (mb.max(0.) * 1024. * 1024.) as i64;
            while self.used_bytes()? > max {
                let n = self.conn.execute(
                    "DELETE FROM bookmark_runs WHERE id IN (
                       SELECT id FROM bookmark_runs ORDER BY started_at, id LIMIT ?1
                     )",
                    params![PRUNE_BATCH],
                )?;
                if n == 0 {
                    break;
                }
                deleted += n;
            }
        }
        if deleted > 0 {
            self.conn.execute_batch("PRAGMA incremental_vacuum")?;
        }
        Ok(())
    }

    /// Space the data takes: pages in use (freed pages don't count).
    fn used_bytes(&self) -> Result<i64> {
        let pragma = |name: &str| -> Result<i64> {
            Ok(self
                .conn
                .query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))?)
        };
        Ok((pragma("page_count")? - pragma("freelist_count")?) * pragma("page_size")?)
    }

    #[cfg(test)]
    fn count(&self, bookmark_id: &str) -> i64 {
        self.conn
            .query_row(
                "SELECT count(*) FROM bookmark_runs WHERE bookmark_id = ?1",
                params![bookmark_id],
                |r| r.get(0),
            )
            .unwrap()
    }
}

fn millis(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

/// SQL for a bookmark's runs over time, for a query tab on `metrics.db`: when it
/// ran, how long it took, how many rows it returned. Failed runs are left out.
pub fn runs_sql(bookmark_id: &str) -> String {
    format!(
        "SELECT datetime(started_at / 1000, 'unixepoch', 'localtime') AS run_at,\n  \
         duration_ms,\n  rows,\n  scheduled\n\
         FROM bookmark_runs\n\
         WHERE bookmark_id = '{}' AND error IS NULL\n\
         ORDER BY started_at",
        bookmark_id.replace('\'', "''")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(bookmark: &str, at: i64) -> Run {
        Run {
            bookmark_id: bookmark.into(),
            bookmark_name: format!("name {bookmark}"),
            started_at: at,
            duration_ms: 12,
            rows: Some(3),
            error: None,
            scheduled: true,
        }
    }

    fn no_limits() -> MonitoringSettings {
        MonitoringSettings {
            max_points: None,
            max_age: None,
            max_mb: None,
            ..MonitoringSettings::default()
        }
    }

    #[test]
    fn records_and_keeps_per_bookmark_limit() {
        let m = Metrics::in_memory();
        let limits = MonitoringSettings {
            max_points: Some(3),
            ..no_limits()
        };
        for at in 1..=5 {
            m.record(&run("a", at), &limits, 10);
        }
        m.record(&run("b", 1), &limits, 10);
        assert_eq!((m.count("a"), m.count("b")), (3, 1));
        // The newest are kept.
        assert_eq!(m.last_run("a"), Some(5));
        let oldest: i64 = m
            .conn
            .query_row(
                "SELECT min(started_at) FROM bookmark_runs WHERE bookmark_id = 'a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(oldest, 3);
        assert_eq!(m.last_run("none"), None);
        m.delete_bookmark("a");
        assert_eq!(m.count("a"), 0);
    }

    #[test]
    fn drops_old_runs() {
        let m = Metrics::in_memory();
        let limits = MonitoringSettings {
            max_age: Some(Duration::from_secs(60)),
            ..no_limits()
        };
        let now = 1_000_000;
        m.record(&run("a", now - 61_000), &no_limits(), now);
        m.record(&run("a", now - 59_000), &no_limits(), now);
        m.prune(&limits, now);
        assert_eq!(m.count("a"), 1);
    }

    #[test]
    fn stays_under_the_size_limit() {
        let m = Metrics::in_memory();
        for at in 0..5_000 {
            let mut r = run("a", at);
            r.error = Some("x".repeat(200));
            m.record(&r, &no_limits(), at);
        }
        assert!(m.used_bytes().unwrap() > 1024 * 1024);
        let limits = MonitoringSettings {
            max_mb: Some(0.5),
            ..no_limits()
        };
        m.prune(&limits, 5_000);
        assert!(m.used_bytes().unwrap() <= 512 * 1024);
        // The oldest went first.
        assert_eq!(m.last_run("a"), Some(4_999));
        assert!(m.count("a") > 0);
    }

    #[test]
    fn runs_sql_quotes_the_id() {
        assert!(runs_sql("it's").contains("'it''s'"));
    }
}
