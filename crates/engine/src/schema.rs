//! Loading a connection's catalog: tables, columns, foreign keys, and indexes,
//! constraints, enums, view definitions and routines.

use tangul_core::SchemaInfo;

use crate::pool::Pool;

/// Tables are required; everything else is best effort (it feeds SQL assistance and
/// the assistant, and a restricted user may not be allowed to read all of it).
pub(crate) async fn load_schema(pool: &Pool) -> anyhow::Result<SchemaInfo> {
    let mut lease = pool.acquire(None).await?;
    let session = &mut lease.session;
    let result = match session.list_tables().await {
        Ok(tables) => {
            let columns = session.list_columns().await.unwrap_or_else(|e| {
                eprintln!("could not load columns: {e:#}");
                Vec::new()
            });
            let foreign_keys = session.list_foreign_keys().await.unwrap_or_else(|e| {
                eprintln!("could not load foreign keys: {e:#}");
                Vec::new()
            });
            let extras = session.catalog_extras().await.unwrap_or_else(|e| {
                eprintln!("could not load indexes, constraints and views: {e:#}");
                Default::default()
            });
            Ok(SchemaInfo {
                tables,
                columns,
                foreign_keys,
                ..Default::default()
            }
            .with_extras(extras))
        }
        Err(e) => Err(e),
    };
    pool.release(None, lease);
    result
}
