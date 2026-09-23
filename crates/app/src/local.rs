//! The app's own SQLite database (`local.db` next to `profiles.toml`): bookmarks,
//! query history, learned usage and a per-user schema cache.
//! Falls back to an in-memory database if the file cannot be opened.

use std::path::Path;

use anyhow::Result;
use rusdb_assist::suggest::HistoryPattern;
use rusdb_assist::usage::{Usage, UsageKind, UsageStat};
use rusdb_core::SchemaInfo;
use rusqlite::{Connection, OptionalExtension as _, params};
use serde::Deserialize;

/// A named, saved query.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Bookmark {
    pub id: String,
    pub name: String,
    pub sql: String,
    /// Database the bookmark belongs to; `None` shows it for every database.
    #[serde(default)]
    pub database_id: Option<String>,
}

/// One executed script.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub sql: String,
    /// Unix milliseconds.
    pub started_at: i64,
    pub duration_ms: i64,
    pub rows: Option<i64>,
    pub error: Option<String>,
}

pub struct NewHistory<'a> {
    pub database_id: &'a str,
    pub user_id: &'a str,
    pub sql: &'a str,
    pub fingerprint: &'a str,
    pub entry: HistoryEntry,
}

const MIGRATIONS: &[&str] = &["CREATE TABLE bookmarks (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        sql TEXT NOT NULL,
        database_id TEXT,
        fingerprint TEXT NOT NULL
    );
    CREATE TABLE history (
        id INTEGER PRIMARY KEY,
        database_id TEXT NOT NULL,
        user_id TEXT NOT NULL,
        sql TEXT NOT NULL,
        fingerprint TEXT NOT NULL,
        started_at INTEGER NOT NULL,
        duration_ms INTEGER NOT NULL,
        rows INTEGER,
        error TEXT
    );
    CREATE INDEX history_db_time ON history (database_id, started_at DESC);
    CREATE INDEX history_db_fingerprint ON history (database_id, fingerprint);
    CREATE TABLE usage (
        database_id TEXT NOT NULL,
        kind TEXT NOT NULL,
        key TEXT NOT NULL,
        count INTEGER NOT NULL,
        last_used INTEGER NOT NULL,
        PRIMARY KEY (database_id, kind, key)
    );
    CREATE TABLE schema_cache (
        user_id TEXT PRIMARY KEY,
        payload TEXT NOT NULL
    );"];

pub struct Local {
    conn: Connection,
}

impl Local {
    /// Opens (and migrates) `dir/local.db`. Imports a legacy `bookmarks.toml` once.
    pub fn open(dir: Option<&Path>) -> Self {
        let opened = dir.and_then(|dir| {
            std::fs::create_dir_all(dir).ok()?;
            Connection::open(dir.join("local.db"))
                .map_err(|e| eprintln!("could not open local.db: {e}"))
                .ok()
        });
        let conn =
            opened.unwrap_or_else(|| Connection::open_in_memory().expect("in-memory sqlite"));
        let local = Self { conn };
        if let Err(e) = local.migrate() {
            eprintln!("local.db migration failed: {e:#}");
        }
        if let Some(dir) = dir {
            local.import_legacy_bookmarks(dir);
        }
        local
    }

    #[cfg(test)]
    fn in_memory() -> Self {
        Self::open(None)
    }

    fn migrate(&self) -> Result<()> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        for (i, sql) in (0i64..).zip(MIGRATIONS).skip(version as usize) {
            self.conn.execute_batch(sql)?;
            self.conn.pragma_update(None, "user_version", i + 1)?;
        }
        Ok(())
    }

    /// Bookmarks used to live in `bookmarks.toml`; move them over and rename the file.
    fn import_legacy_bookmarks(&self, dir: &Path) {
        #[derive(Deserialize)]
        struct File {
            #[serde(default)]
            bookmarks: Vec<Bookmark>,
        }
        let path = dir.join("bookmarks.toml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        match toml::from_str::<File>(&text) {
            Ok(file) => {
                for b in &file.bookmarks {
                    if let Err(e) = self.upsert_bookmark(b) {
                        eprintln!("bookmark import failed: {e:#}");
                        return;
                    }
                }
                let _ = std::fs::rename(&path, dir.join("bookmarks.toml.imported"));
            }
            Err(e) => eprintln!("ignoring invalid bookmarks file: {e}"),
        }
    }

    // ---- bookmarks -----------------------------------------------------------------

    /// Bookmarks shown for a database: its own plus the global ones, by name.
    pub fn bookmarks_for(&self, db_id: Option<&str>) -> Vec<Bookmark> {
        self.query_bookmarks(
            "SELECT id, name, sql, database_id FROM bookmarks
             WHERE database_id IS NULL OR database_id = ?1
             ORDER BY lower(name)",
            params![db_id],
        )
    }

    pub fn bookmark(&self, id: &str) -> Option<Bookmark> {
        self.query_bookmarks(
            "SELECT id, name, sql, database_id FROM bookmarks WHERE id = ?1",
            params![id],
        )
        .pop()
    }

    fn query_bookmarks(&self, sql: &str, params: impl rusqlite::Params) -> Vec<Bookmark> {
        let result = self.conn.prepare_cached(sql).and_then(|mut stmt| {
            stmt.query_map(params, |r| {
                Ok(Bookmark {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    sql: r.get(2)?,
                    database_id: r.get(3)?,
                })
            })?
            .collect()
        });
        result.unwrap_or_else(|e| {
            eprintln!("bookmark query failed: {e}");
            Vec::new()
        })
    }

    /// Inserts or replaces a bookmark (matched by id).
    pub fn upsert_bookmark(&self, b: &Bookmark) -> Result<()> {
        self.conn.execute(
            "INSERT INTO bookmarks (id, name, sql, database_id, fingerprint)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET name = ?2, sql = ?3, database_id = ?4, fingerprint = ?5",
            params![b.id, b.name, b.sql, b.database_id, rusdb_assist::lexer::fingerprint(&b.sql)],
        )?;
        Ok(())
    }

    pub fn delete_bookmark(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM bookmarks WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Whether a query with this fingerprint is bookmarked for the database.
    pub fn is_bookmarked(&self, db_id: &str, fingerprint: &str) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM bookmarks
                 WHERE fingerprint = ?2 AND (database_id IS NULL OR database_id = ?1)",
                params![db_id, fingerprint],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    // ---- history -------------------------------------------------------------------

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

    // ---- usage ---------------------------------------------------------------------

    /// Counts one use of each key at `now` (unix seconds).
    pub fn record_usage(&self, db_id: &str, items: &[(UsageKind, String)], now: i64) -> Result<()> {
        let mut stmt = self.conn.prepare_cached(
            "INSERT INTO usage (database_id, kind, key, count, last_used) VALUES (?1, ?2, ?3, 1, ?4)
             ON CONFLICT (database_id, kind, key) DO UPDATE SET count = count + 1, last_used = ?4",
        )?;
        for (kind, key) in items {
            stmt.execute(params![db_id, kind.as_str(), key, now])?;
        }
        Ok(())
    }

    pub fn usage(&self, db_id: &str, now: i64) -> Usage {
        let mut usage = Usage::new(now);
        let result = self
            .conn
            .prepare_cached("SELECT kind, key, count, last_used FROM usage WHERE database_id = ?1")
            .and_then(|mut stmt| {
                let rows = stmt.query_map(params![db_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?;
                for row in rows {
                    let (kind, key, count, last_used) = row?;
                    if let Some(kind) = UsageKind::parse(&kind) {
                        usage.insert(kind, key, UsageStat { count, last_used });
                    }
                }
                Ok(())
            });
        if let Err(e) = result {
            eprintln!("usage query failed: {e}");
        }
        usage
    }

    // ---- schema cache --------------------------------------------------------------

    pub fn save_schema(&self, user_id: &str, schema: &SchemaInfo) -> Result<()> {
        self.conn.execute(
            "INSERT INTO schema_cache (user_id, payload) VALUES (?1, ?2)
             ON CONFLICT (user_id) DO UPDATE SET payload = ?2",
            params![user_id, serde_json::to_string(schema)?],
        )?;
        Ok(())
    }

    pub fn cached_schema(&self, user_id: &str) -> Option<SchemaInfo> {
        let payload: String = self
            .conn
            .query_row(
                "SELECT payload FROM schema_cache WHERE user_id = ?1",
                params![user_id],
                |r| r.get(0),
            )
            .optional()
            .ok()??;
        serde_json::from_str(&payload).ok()
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
                fingerprint: &rusdb_assist::lexer::fingerprint(sql),
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
    fn bookmarks_scoped_and_fingerprinted() {
        let l = Local::in_memory();
        let b = |id: &str, name: &str, db: Option<&str>| Bookmark {
            id: id.into(),
            name: name.into(),
            sql: "select * from t where id = 1".into(),
            database_id: db.map(Into::into),
        };
        l.upsert_bookmark(&b("1", "zeta", Some("pg"))).unwrap();
        l.upsert_bookmark(&b("2", "Alpha", None)).unwrap();
        l.upsert_bookmark(&b("3", "other", Some("mysql"))).unwrap();
        let ids: Vec<_> = l
            .bookmarks_for(Some("pg"))
            .into_iter()
            .map(|b| b.id)
            .collect();
        assert_eq!(ids, ["2", "1"]);
        assert!(l.is_bookmarked(
            "pg",
            &rusdb_assist::lexer::fingerprint("SELECT * FROM t WHERE id = 7")
        ));
        l.delete_bookmark("2").unwrap();
        assert!(l.bookmark("2").is_none());
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
                &rusdb_assist::lexer::fingerprint("select * from t where id = 9")
            ),
            2
        );
        assert_eq!(l.history("db", "", 10)[0].error.as_deref(), Some("boom"));
        assert_eq!(l.history("db", "%", 10).len(), 1);
    }

    #[test]
    fn usage_and_schema_cache_roundtrip() {
        let l = Local::in_memory();
        let items = [(UsageKind::Table, "app\u{1f}users".to_string())];
        l.record_usage("db", &items, 10).unwrap();
        l.record_usage("db", &items, 20).unwrap();
        assert_eq!(
            l.usage("db", 20).score(UsageKind::Table, "app\u{1f}users"),
            2.0
        );
        let schema = SchemaInfo::default();
        l.save_schema("u", &schema).unwrap();
        assert_eq!(l.cached_schema("u"), Some(schema));
    }
}
