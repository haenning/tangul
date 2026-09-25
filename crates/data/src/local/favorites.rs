//! Favorite tables and views per database.

use anyhow::Result;
use rusqlite::params;

use super::Local;

impl Local {
    /// Favorite tables and views of a database as (schema, name), ordered.
    pub fn favorites(&self, db_id: &str) -> Vec<(String, String)> {
        let result = self
            .conn
            .prepare_cached(
                "SELECT schema, name FROM favorites WHERE database_id = ?1
                 ORDER BY schema, name",
            )
            .and_then(|mut stmt| {
                stmt.query_map(params![db_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect()
            });
        result.unwrap_or_else(|e| {
            eprintln!("favorites query failed: {e}");
            Vec::new()
        })
    }

    pub fn set_favorite(&self, db_id: &str, schema: &str, name: &str, on: bool) -> Result<()> {
        let sql = if on {
            "INSERT OR IGNORE INTO favorites (database_id, schema, name) VALUES (?1, ?2, ?3)"
        } else {
            "DELETE FROM favorites WHERE database_id = ?1 AND schema = ?2 AND name = ?3"
        };
        self.conn.execute(sql, params![db_id, schema, name])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn favorites_per_database() {
        let l = Local::in_memory();
        l.set_favorite("pg", "public", "users", true).unwrap();
        l.set_favorite("pg", "app", "orders", true).unwrap();
        // Adding twice is harmless.
        l.set_favorite("pg", "app", "orders", true).unwrap();
        l.set_favorite("mysql", "app", "users", true).unwrap();
        assert_eq!(
            l.favorites("pg"),
            vec![
                ("app".to_string(), "orders".to_string()),
                ("public".to_string(), "users".to_string()),
            ]
        );
        l.set_favorite("pg", "public", "users", false).unwrap();
        assert_eq!(l.favorites("pg").len(), 1);
        assert_eq!(l.favorites("mysql").len(), 1);
    }
}
