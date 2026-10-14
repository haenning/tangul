//! Everything tangul keeps on disk, without any UI: saved databases and their
//! secrets, `local.db` (bookmarks, history, learned usage, favorites, schema cache),
//! `metrics.db` (bookmark runs), `settings.json`, color themes and boards. The app uses it, and so can
//! other front ends (an MCP server) in their own process.

pub mod boards;
pub mod config;
pub mod database_settings;
pub mod local;
pub mod metrics;
pub mod migrations;
pub mod profiles;
pub mod settings;
pub mod themes;
pub mod upgrade;

use anyhow::Result;

use crate::boards::Boards;
use crate::local::{Bookmark, Local, bare_sql};
use crate::metrics::Metrics;
use crate::profiles::Profiles;

/// All stored data, opened from the config directory.
pub struct Data {
    pub profiles: Profiles,
    pub local: Local,
    pub metrics: Metrics,
    pub boards: Boards,
}

impl Data {
    pub fn open() -> Self {
        let profiles = Profiles::load();
        let local = Local::open(profiles.dir());
        let metrics = Metrics::open(profiles.dir());
        let boards = Boards::open(profiles.dir());
        Self {
            profiles,
            local,
            metrics,
            boards,
        }
    }

    /// No saved databases and in-memory stores: nothing is read from or written to
    /// disk (tests).
    pub fn in_memory() -> Self {
        Self {
            profiles: Profiles::in_memory(Vec::new()),
            local: Local::open(None),
            metrics: Metrics::open(None),
            boards: Boards::open(None),
        }
    }

    /// Deletes a bookmark and the runs recorded for it.
    pub fn delete_bookmark(&self, id: &str) -> Result<()> {
        self.metrics.delete_bookmark(id);
        self.local.delete_bookmark(id)
    }

    /// Removes a database and everything kept for it: its entry with its users and
    /// their secrets, what `local.db` holds for it ([`Local::delete_database`]), its
    /// bookmarks' runs, its boards and its own settings file. Nothing else is touched
    /// when the database can't be removed from `profiles.toml`.
    pub fn remove_database(&mut self, id: &str) -> Result<()> {
        let users: Vec<String> = match self.profiles.database(id) {
            Some(db) => db.users.iter().map(|u| u.id.clone()).collect(),
            None => Vec::new(),
        };
        self.profiles.remove_database(id)?;
        let mut failed = Vec::new();
        match self.local.delete_database(id, &users) {
            Ok(bookmarks) => {
                for b in bookmarks {
                    self.metrics.delete_bookmark(&b);
                }
            }
            Err(e) => failed.push(format!("local.db: {e:#}")),
        }
        let boards: Vec<String> = self
            .boards
            .for_database(id)
            .iter()
            .map(|b| b.id.clone())
            .collect();
        for b in boards {
            if let Err(e) = self.boards.delete(&b) {
                failed.push(format!("board {b}: {e}"));
            }
        }
        if let Some(dir) = self.profiles.dir() {
            match std::fs::remove_file(database_settings::path(dir, id)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => failed.push(format!("its settings file: {e}")),
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("removed, but not all of its data: {}", failed.join("; "))
        }
    }

    /// The bookmark `sql` ran as: `bookmark_id`'s, if all of its SQL ran (not a
    /// selection of it).
    pub fn bookmark_run(&self, bookmark_id: &str, sql: &str) -> Option<Bookmark> {
        self.local
            .bookmark(bookmark_id)
            .filter(|b| bare_sql(&b.sql) == bare_sql(sql))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::{HistoryEntry, LAST_USER, NewHistory};
    use crate::metrics::Run;
    use tangul_core::Database;
    use tangul_core::SchemaInfo;
    use tangul_core::board::Board;
    use tangul_core::pinned::PinnedTab;
    use tangul_core::usage::UsageKind;

    fn database(id: &str, user: &str) -> Database {
        toml::from_str(&format!(
            "id = \"{id}\"\nname = \"{id}\"\ndriver = \"postgres\"\nhost = \"h\"\nport = 5432\n\
             [[users]]\nid = \"{user}\"\nuser = \"app\"\n"
        ))
        .unwrap()
    }

    /// Fills in everything tangul keeps for a database.
    fn keep_all(data: &mut Data, db: &str, user: &str) {
        let local = &data.local;
        local
            .add_history(&NewHistory {
                database_id: db,
                user_id: user,
                sql: "select 1",
                fingerprint: "fp",
                entry: HistoryEntry {
                    sql: "select 1".into(),
                    source: Default::default(),
                    started_at: 1,
                    duration_ms: 1,
                    rows: None,
                    error: None,
                },
            })
            .unwrap();
        local
            .record_usage(db, &[(UsageKind::Table, "t".into())], 1)
            .unwrap();
        local.set_favorite(db, "public", "t", true).unwrap();
        let bookmark = Bookmark {
            id: format!("b-{db}"),
            name: "b".into(),
            sql: "select 1".into(),
            database_id: Some(db.into()),
            ..Default::default()
        };
        local.upsert_bookmark(&bookmark, "fp").unwrap();
        let run = Run {
            bookmark_id: bookmark.id,
            bookmark_name: "b".into(),
            started_at: 1,
            duration_ms: 1,
            rows: None,
            error: None,
            scheduled: true,
        };
        data.metrics.record(&run, &Default::default(), 1);
        let chat = local.create_chat(Some(db), "q", 1).unwrap();
        local.add_chat_message(chat, "m", 1).unwrap();
        local.set_pinned_tabs(
            db,
            &[PinnedTab::Board {
                board_id: format!("board-{db}"),
            }],
        );
        local.save_schema(user, &SchemaInfo::default()).unwrap();
        data.boards
            .save(Board {
                id: format!("board-{db}"),
                name: format!("board {db}"),
                database_id: db.into(),
                ..Default::default()
            })
            .unwrap();
    }

    #[test]
    fn removing_a_database_deletes_everything_kept_for_it_and_nothing_else() {
        let dir = std::env::temp_dir().join(format!("tangul-remove-db-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let settings = database_settings::path(&dir, "a");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, "{}").unwrap();
        std::fs::write(database_settings::path(&dir, "b"), "{}").unwrap();
        let mut data = Data {
            profiles: Profiles::in_memory_at(&dir, vec![database("a", "ua"), database("b", "ub")]),
            local: Local::open(None),
            metrics: Metrics::open(None),
            boards: Boards::open(Some(&dir)),
        };
        data.profiles.set_secret("ua", "pw".into());
        data.profiles.set_ssh_secret("a", "ssh".into());
        keep_all(&mut data, "a", "ua");
        keep_all(&mut data, "b", "ub");
        data.local.set_setting(LAST_USER, "ua");
        let global = Bookmark {
            id: "global".into(),
            name: "g".into(),
            sql: "select 2".into(),
            ..Default::default()
        };
        data.local.upsert_bookmark(&global, "fp2").unwrap();

        data.remove_database("a").unwrap();

        let (local, profiles) = (&data.local, &mut data.profiles);
        assert!(profiles.database("a").is_none());
        assert_eq!(profiles.password("ua"), None);
        assert_eq!(profiles.password("ssh:a"), None);
        assert!(local.history("a", "", &[], 10).is_empty());
        assert!(local.usage("a").is_empty());
        assert!(local.favorites("a").is_empty());
        assert!(local.bookmark("b-a").is_none());
        assert_eq!(data.metrics.last_run("b-a"), None);
        assert!(
            local
                .chats(10)
                .iter()
                .all(|c| c.database_id.as_deref() != Some("a"))
        );
        assert!(local.pinned_tabs("a").is_empty());
        assert_eq!(local.setting("pinned_tabs:a"), None);
        assert!(local.cached_schema("ua").is_none());
        assert_eq!(local.setting(LAST_USER), None);
        assert!(data.boards.for_database("a").is_empty());
        assert!(!settings.exists());

        // The other database, and bookmarks for every database, are untouched.
        assert!(profiles.database("b").is_some());
        assert!(!local.history("b", "", &[], 10).is_empty());
        assert!(!local.usage("b").is_empty());
        assert!(!local.favorites("b").is_empty());
        assert!(local.bookmark("b-b").is_some());
        assert!(local.bookmark("global").is_some());
        assert!(data.metrics.last_run("b-b").is_some());
        assert_eq!(local.chats(10).len(), 1);
        assert!(!local.pinned_tabs("b").is_empty());
        assert!(local.cached_schema("ub").is_some());
        assert_eq!(data.boards.for_database("b").len(), 1);
        assert!(database_settings::path(&dir, "b").exists());

        // Gone already: nothing to do.
        data.remove_database("a").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
