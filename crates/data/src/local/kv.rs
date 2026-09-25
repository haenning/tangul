//! Small app state kept between runs (last user, sidebar width).

use rusqlite::{OptionalExtension as _, params};

use super::Local;

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
}
