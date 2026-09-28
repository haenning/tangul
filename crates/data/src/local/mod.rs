//! The app's own SQLite database (`local.db` next to `profiles.toml`): bookmarks,
//! query history, learned usage, favorites, a per-user schema cache and small app
//! state, one file per topic.
//! Falls back to an in-memory database if the file cannot be opened.

mod audit;
mod bookmarks;
mod favorites;
mod history;
mod kv;
mod schema_cache;
mod usage;

use std::path::Path;

use anyhow::Result;
use rusqlite::Connection;
use serde::Deserialize;
use tangul_core::chart::ChartView;

/// A named, saved query.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Bookmark {
    pub id: String,
    pub name: String,
    pub sql: String,
    /// Database the bookmark belongs to; `None` shows it for every database.
    #[serde(default)]
    pub database_id: Option<String>,
    /// How its result is shown: the grid (`None`) or a chart.
    #[serde(skip)]
    pub view: Option<ChartView>,
    /// Run in the background now and then to track its duration and row count.
    #[serde(skip)]
    pub scheduled: bool,
}

/// Who ran a query in the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    /// You, in a tab.
    #[default]
    App,
    /// The in-app assistant.
    Ai,
    /// An MCP client (Claude Code, …) through tangul's MCP server.
    Mcp,
}

impl Source {
    pub const ALL: [Self; 3] = [Self::App, Self::Ai, Self::Mcp];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Ai => "ai",
            Self::Mcp => "mcp",
        }
    }

    fn parse(text: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|s| s.as_str() == text)
            .unwrap_or_default()
    }
}

/// One executed script.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub sql: String,
    pub source: Source,
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

const MIGRATIONS: &[&str] = &[
    "CREATE TABLE bookmarks (
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
    );",
    "CREATE TABLE settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );",
    "CREATE TABLE favorites (
        database_id TEXT NOT NULL,
        schema TEXT NOT NULL,
        name TEXT NOT NULL,
        PRIMARY KEY (database_id, schema, name)
    );",
    "ALTER TABLE bookmarks ADD COLUMN view TEXT;
    ALTER TABLE bookmarks ADD COLUMN scheduled INTEGER NOT NULL DEFAULT 0;",
    "CREATE TABLE audit (
        id INTEGER PRIMARY KEY,
        at INTEGER NOT NULL,
        caller TEXT NOT NULL,
        target TEXT NOT NULL,
        sql TEXT NOT NULL,
        outcome TEXT NOT NULL,
        error TEXT,
        duration_ms INTEGER,
        rows INTEGER
    );
    CREATE INDEX audit_at ON audit (at DESC);",
    "ALTER TABLE history ADD COLUMN source TEXT NOT NULL DEFAULT 'app';",
];

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
    pub(crate) fn in_memory() -> Self {
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
    /// Their fingerprints are filled in by [`Local::fill_missing_fingerprints`].
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
                    if let Err(e) = self.upsert_bookmark(b, "") {
                        eprintln!("bookmark import failed: {e:#}");
                        return;
                    }
                }
                let _ = std::fs::rename(&path, dir.join("bookmarks.toml.imported"));
            }
            Err(e) => eprintln!("ignoring invalid bookmarks file: {e}"),
        }
    }
}

pub use audit::AuditEntry;
pub use bookmarks::bare_sql;

#[cfg(test)]
mod tests {
    use super::*;
    use tangul_core::SchemaInfo;
    use tangul_core::usage::{UsageKind, UsageStat};

    #[test]
    fn usage_and_schema_cache_roundtrip() {
        let l = Local::in_memory();
        let items = [(UsageKind::Table, "app\u{1f}users".to_string())];
        l.record_usage("db", &items, 10).unwrap();
        l.record_usage("db", &items, 20).unwrap();
        assert_eq!(
            l.usage("db"),
            [(
                UsageKind::Table,
                "app\u{1f}users".to_string(),
                UsageStat {
                    count: 2,
                    last_used: 20
                }
            )]
        );
        let schema = SchemaInfo::default();
        l.save_schema("u", &schema).unwrap();
        assert_eq!(l.cached_schema("u"), Some(schema));
        assert_eq!(l.setting("last_user"), None);
        l.set_setting("last_user", "a");
        l.set_setting("last_user", "b");
        assert_eq!(l.setting("last_user").as_deref(), Some("b"));
    }
}
