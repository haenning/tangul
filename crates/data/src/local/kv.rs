//! Small app state kept between runs (last user, sidebar width, each database's
//! pinned tabs).

use rusqlite::{OptionalExtension as _, params};

use tangul_core::pinned::PinnedTab;

use super::Local;

/// The user the app opens with: the one last chosen.
pub const LAST_USER: &str = "last_user";

pub(super) fn pinned_key(database_id: &str) -> String {
    format!("pinned_tabs:{database_id}")
}

impl Local {
    pub fn setting(&self, key: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .ok()?
    }

    pub fn set_setting(&self, key: &str, value: &str) {
        let result = self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = ?2",
            params![key, value],
        );
        if let Err(e) = result {
            eprintln!("could not save setting {key}: {e}");
        }
    }

    /// The tabs pinned for `database_id` when it was last open, in order. Entries a
    /// newer version wrote that this one doesn't know are skipped.
    pub fn pinned_tabs(&self, database_id: &str) -> Vec<PinnedTab> {
        let Some(json) = self.setting(&pinned_key(database_id)) else {
            return Vec::new();
        };
        let Ok(values) = serde_json::from_str::<Vec<serde_json::Value>>(&json) else {
            return Vec::new();
        };
        values
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect()
    }

    pub fn set_pinned_tabs(&self, database_id: &str, tabs: &[PinnedTab]) {
        match serde_json::to_string(tabs) {
            Ok(json) => self.set_setting(&pinned_key(database_id), &json),
            Err(e) => eprintln!("could not save pinned tabs: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use tangul_core::pinned::PinnedTab;

    use crate::local::Local;

    #[test]
    fn pinned_tabs_per_database() {
        let local = Local::open(None);
        assert!(local.pinned_tabs("db1").is_empty());
        let tabs = vec![
            PinnedTab::Board {
                board_id: "b1".into(),
            },
            PinnedTab::Query {
                title: "q".into(),
                sql: "select 1".into(),
                user_id: None,
                bookmark_id: None,
                view: None,
            },
        ];
        local.set_pinned_tabs("db1", &tabs);
        assert_eq!(local.pinned_tabs("db1"), tabs);
        assert!(local.pinned_tabs("db2").is_empty());
        local.set_pinned_tabs("db1", &[]);
        assert!(local.pinned_tabs("db1").is_empty());
        // Unknown kinds (a newer version) are skipped, the rest kept.
        local.set_setting(
            "pinned_tabs:db3",
            r#"[{"kind":"chat","id":"c"},{"kind":"board","board_id":"b2"}]"#,
        );
        assert_eq!(
            local.pinned_tabs("db3"),
            [PinnedTab::Board {
                board_id: "b2".into()
            }]
        );
    }
}
