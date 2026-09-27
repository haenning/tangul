//! Loading a connection's catalog in two steps: first what the UI needs at once
//! (tables, columns, foreign keys), then the rest (indexes, constraints, enums,
//! view definitions and routines), which only the assistant and details use.

use tangul_core::{CatalogExtras, SchemaInfo};

use crate::pool::Pool;

/// Tables, columns and foreign keys. Tables are required; the rest is best effort
/// (a restricted user may not be allowed to read all of it).
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
                ..Default::default()
            })
        }
        Err(e) => Err(e),
    };
    pool.release(None, lease);
    result
}

/// Indexes, checks, enums, view definitions and routines; empty where the server
/// can't answer.
pub(crate) async fn load_extras(pool: &Pool) -> CatalogExtras {
    let mut lease = match pool.acquire(None).await {
        Ok(lease) => lease,
        Err(e) => {
            eprintln!("could not load indexes, constraints and views: {e:#}");
            return CatalogExtras::default();
        }
    };
    let extras = lease.session.catalog_extras().await.unwrap_or_else(|e| {
        eprintln!("could not load indexes, constraints and views: {e:#}");
        CatalogExtras::default()
    });
    pool.release(None, lease);
    extras
}
