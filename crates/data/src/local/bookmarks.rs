//! Saved queries: name, SQL, database, how the result is shown, whether it runs on
//! a schedule.

use anyhow::Result;
use rusqlite::{OptionalExtension as _, params};
use tangul_core::chart::ChartView;

use super::{Bookmark, Local};

impl Local {
    /// Bookmarks shown for a database: its own plus the global ones, by name.
    pub fn bookmarks_for(&self, db_id: Option<&str>) -> Vec<Bookmark> {
        self.query_bookmarks(
            "SELECT id, name, sql, database_id, view, scheduled FROM bookmarks
             WHERE database_id IS NULL OR database_id = ?1
             ORDER BY lower(name)",
            params![db_id],
        )
    }

    pub fn bookmark(&self, id: &str) -> Option<Bookmark> {
        self.query_bookmarks(
            "SELECT id, name, sql, database_id, view, scheduled FROM bookmarks WHERE id = ?1",
            params![id],
        )
        .pop()
    }

    /// Bookmarks that run on a schedule (only ones for a single database).
    pub fn scheduled_bookmarks(&self) -> Vec<Bookmark> {
        self.query_bookmarks(
            "SELECT id, name, sql, database_id, view, scheduled FROM bookmarks
             WHERE scheduled AND database_id IS NOT NULL
             ORDER BY lower(name)",
            [],
        )
    }

    fn query_bookmarks(&self, sql: &str, params: impl rusqlite::Params) -> Vec<Bookmark> {
        let result = self.conn.prepare_cached(sql).and_then(|mut stmt| {
            stmt.query_map(params, |r| {
                let view: Option<String> = r.get(4)?;
                Ok(Bookmark {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    sql: r.get(2)?,
                    database_id: r.get(3)?,
                    view: view.as_deref().and_then(ChartView::from_key),
                    scheduled: r.get(5)?,
                })
            })?
            .collect()
        });
        result.unwrap_or_else(|e| {
            eprintln!("bookmark query failed: {e}");
            Vec::new()
        })
    }

    /// Inserts or replaces a bookmark (matched by id). `fingerprint` is its SQL's
    /// (`tangul_assist::lexer::fingerprint`), for [`Local::is_bookmarked`].
    pub fn upsert_bookmark(&self, b: &Bookmark, fingerprint: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO bookmarks (id, name, sql, database_id, fingerprint, view, scheduled)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (id) DO UPDATE SET name = ?2, sql = ?3, database_id = ?4,
               fingerprint = ?5, view = ?6, scheduled = ?7",
            params![
                b.id,
                b.name,
                b.sql,
                b.database_id,
                fingerprint,
                b.view.map(ChartView::to_key),
                b.scheduled,
            ],
        )?;
        Ok(())
    }

    pub fn delete_bookmark(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM bookmarks WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// The bookmark shown for the database whose SQL is `sql`, ignoring surrounding
    /// whitespace and trailing semicolons. Values count (unlike the fingerprint): a
    /// query for another id is not that bookmark. The database's own bookmarks win
    /// over global ones.
    pub fn bookmark_for_sql(&self, db_id: &str, sql: &str) -> Option<Bookmark> {
        let wanted = bare_sql(sql);
        let mut matches: Vec<Bookmark> = self
            .bookmarks_for(Some(db_id))
            .into_iter()
            .filter(|b| bare_sql(&b.sql) == wanted)
            .collect();
        matches.sort_by_key(|b| b.database_id.is_none());
        matches.into_iter().next()
    }

    /// Computes the fingerprints bookmarks were stored without (imported ones);
    /// `fingerprint` is `tangul_assist::lexer::fingerprint`.
    pub fn fill_missing_fingerprints(&self, fingerprint: impl Fn(&str) -> String) -> Result<()> {
        let missing: Vec<(String, String)> = self
            .conn
            .prepare("SELECT id, sql FROM bookmarks WHERE fingerprint = ''")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, sql) in missing {
            self.conn.execute(
                "UPDATE bookmarks SET fingerprint = ?2 WHERE id = ?1",
                params![id, fingerprint(&sql)],
            )?;
        }
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
}

/// `sql` without surrounding whitespace and trailing semicolons.
pub fn bare_sql(sql: &str) -> &str {
    sql.trim()
        .trim_end_matches(|c: char| c == ';' || c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmarks_scoped_and_fingerprinted() {
        let l = Local::in_memory();
        let b = |id: &str, name: &str, db: Option<&str>| Bookmark {
            id: id.into(),
            name: name.into(),
            sql: "select * from t where id = 1".into(),
            database_id: db.map(Into::into),
            ..Default::default()
        };
        l.upsert_bookmark(&b("1", "zeta", Some("pg")), "fp")
            .unwrap();
        l.upsert_bookmark(&b("2", "Alpha", None), "fp").unwrap();
        l.upsert_bookmark(&b("3", "other", Some("mysql")), "fp3")
            .unwrap();
        let ids: Vec<_> = l
            .bookmarks_for(Some("pg"))
            .into_iter()
            .map(|b| b.id)
            .collect();
        assert_eq!(ids, ["2", "1"]);
        assert!(l.is_bookmarked("pg", "fp"));
        // Another database's bookmark doesn't count.
        assert!(!l.is_bookmarked("pg", "fp3"));
        l.delete_bookmark("2").unwrap();
        assert!(l.bookmark("2").is_none());
        // Stored without one (imported): filled in later.
        l.upsert_bookmark(&b("4", "imported", Some("pg")), "")
            .unwrap();
        l.fill_missing_fingerprints(|sql| format!("fp of {sql}"))
            .unwrap();
        assert!(l.is_bookmarked("pg", "fp of select * from t where id = 1"));
    }

    #[test]
    fn finds_bookmark_by_sql() {
        let l = Local::in_memory();
        let b = |id: &str, sql: &str, db: Option<&str>| Bookmark {
            id: id.into(),
            name: format!("b{id}"),
            sql: sql.into(),
            database_id: db.map(Into::into),
            ..Default::default()
        };
        l.upsert_bookmark(&b("1", "select * from t where id = 1;", None), "fp")
            .unwrap();
        l.upsert_bookmark(&b("2", "select * from t where id = 1", Some("pg")), "fp")
            .unwrap();
        l.upsert_bookmark(&b("3", "select 2", Some("mysql")), "fp2")
            .unwrap();
        let found = |db: &str, sql: &str| l.bookmark_for_sql(db, sql).map(|b| b.id);
        // Trailing semicolons and whitespace don't matter; the own one wins.
        assert_eq!(
            found("pg", "  select * from t where id = 1 ;\n"),
            Some("2".into())
        );
        assert_eq!(
            found("other", "select * from t where id = 1"),
            Some("1".into())
        );
        // Other values or another database's bookmark: no match.
        assert_eq!(found("pg", "select * from t where id = 2"), None);
        assert_eq!(found("pg", "select 2"), None);
    }

    #[test]
    fn bookmark_view_and_schedule() {
        let l = Local::in_memory();
        let mut b = Bookmark {
            id: "1".into(),
            name: "b".into(),
            sql: "select 1".into(),
            ..Default::default()
        };
        l.upsert_bookmark(&b, "fp").unwrap();
        assert_eq!(l.bookmark("1").unwrap(), b);
        b.view = ChartView::from_key("bars:1");
        b.scheduled = true;
        l.upsert_bookmark(&b, "fp").unwrap();
        assert_eq!(l.bookmark("1").unwrap(), b);
        // Only bookmarks for one database run on a schedule.
        assert!(l.scheduled_bookmarks().is_empty());
        b.database_id = Some("pg".into());
        l.upsert_bookmark(&b, "fp").unwrap();
        assert_eq!(l.scheduled_bookmarks(), vec![b]);
    }
}
