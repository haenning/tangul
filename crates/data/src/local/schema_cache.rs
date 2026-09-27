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

#[cfg(test)]
mod tests {
    use rusqlite::params;
    use tangul_core::{TableInfo, TableKind};

    use super::super::Local;

    #[test]
    fn a_schema_cached_by_an_older_version_still_loads() {
        let local = Local::open(None);
        // Before the rich catalog: no comments, estimates, indexes, … in the payload.
        let old = r#"{"tables":[{"schema":"app","name":"orders","kind":"Table"}],
            "columns":[{"schema":"app","table":"orders","name":"id","data_type":"bigint",
                        "nullable":false,"primary_key":true}],
            "foreign_keys":[]}"#;
        local
            .conn
            .execute(
                "INSERT INTO schema_cache (user_id, payload) VALUES ('u1', ?1)",
                params![old],
            )
            .unwrap();
        let info = local.cached_schema("u1").expect("old cache loads");
        assert_eq!(
            info.tables,
            [TableInfo {
                schema: "app".into(),
                name: "orders".into(),
                kind: TableKind::Table,
                ..Default::default()
            }]
        );
        assert!(info.columns[0].primary_key && info.columns[0].comment.is_none());
        assert!(info.indexes.is_empty() && info.enums.is_empty());
    }

    #[test]
    fn rich_schemas_round_trip() {
        let local = Local::open(None);
        let mut info = tangul_core::SchemaInfo::default();
        info.tables.push(TableInfo {
            schema: "app".into(),
            name: "daily".into(),
            kind: TableKind::MaterializedView,
            comment: Some("Revenue per day".into()),
            row_estimate: Some(700),
            size_bytes: Some(8192),
        });
        info.enums.push(tangul_core::EnumInfo {
            schema: "app".into(),
            name: "order_status".into(),
            values: vec!["open".into(), "paid".into()],
            comment: None,
        });
        local.save_schema("u1", &info).unwrap();
        assert_eq!(local.cached_schema("u1"), Some(info));
    }
}
