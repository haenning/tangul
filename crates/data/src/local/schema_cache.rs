//! The last schema seen per database user, shown until the server answers.

use anyhow::Result;
use rusqlite::{OptionalExtension as _, params};
use tangul_core::SchemaInfo;

use super::Local;

impl Local {
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
