//! Everything tangul keeps on disk, without any UI: saved databases and their
//! secrets, `local.db` (bookmarks, history, learned usage, favorites, schema cache),
//! `metrics.db` (bookmark runs) and `settings.json`. The app uses it, and so can
//! other front ends (an MCP server) in their own process.

pub mod config;
pub mod local;
pub mod metrics;
pub mod profiles;
pub mod settings;

use anyhow::Result;

use crate::local::{Bookmark, Local, bare_sql};
use crate::metrics::Metrics;
use crate::profiles::Profiles;

/// All stored data, opened from the config directory.
pub struct Data {
    pub profiles: Profiles,
    pub local: Local,
    pub metrics: Metrics,
}

impl Data {
    pub fn open() -> Self {
        let profiles = Profiles::load();
        let local = Local::open(profiles.dir());
        let metrics = Metrics::open(profiles.dir());
        Self {
            profiles,
            local,
            metrics,
        }
    }

    /// Deletes a bookmark and the runs recorded for it.
    pub fn delete_bookmark(&self, id: &str) -> Result<()> {
        self.metrics.delete_bookmark(id);
        self.local.delete_bookmark(id)
    }

    /// The bookmark `sql` ran as: `bookmark_id`'s, if all of its SQL ran (not a
    /// selection of it).
    pub fn bookmark_run(&self, bookmark_id: &str, sql: &str) -> Option<Bookmark> {
        self.local
            .bookmark(bookmark_id)
            .filter(|b| bare_sql(&b.sql) == bare_sql(sql))
    }
}
