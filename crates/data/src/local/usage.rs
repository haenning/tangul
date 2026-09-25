//! Learned usage of tables, columns and joins (for completion and ranking).

use anyhow::Result;
use rusqlite::params;
use tangul_assist::usage::{Usage, UsageKind, UsageStat};

use super::Local;

impl Local {
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
}
