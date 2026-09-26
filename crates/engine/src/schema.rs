//! Loading a connection's tables, columns and foreign keys.

use tangul_core::SchemaInfo;

use crate::pool::Pool;

/// Tables are required; columns and foreign keys are best effort (they only feed
/// SQL assistance, and a restricted user may not be allowed to read them).
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
            Ok(SchemaInfo {
                tables,
                columns,
                foreign_keys,
            })
        }
        Err(e) => Err(e),
    };
    pool.release(None, lease);
    result
}
