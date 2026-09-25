//! An open connection as an entity of its own: the tabs using it and the workspace
//! point to the same one, so its status, schema and pool stats are never copies.

use std::sync::Arc;

use gpui_kit::SharedString;
use tangul_assist::schema::SchemaIndex;
use tangul_core::{ConnId, DriverKind, SchemaInfo};
use tangul_engine::PoolStats;

#[derive(Clone, PartialEq)]
pub enum ConnStatus {
    Connecting,
    Connected,
    Failed(SharedString),
}

pub struct Connection {
    pub id: ConnId,
    /// `user@database`, shown in the tab.
    pub name: SharedString,
    pub driver: DriverKind,
    pub status: ConnStatus,
    /// Tables, columns and foreign keys (from the cache until the server answered).
    pub schema: Arc<SchemaIndex>,
    pub stats: PoolStats,
}

impl Connection {
    /// A read-only file of tangul's own (metrics), open as soon as the engine has it.
    pub fn local_file(id: ConnId, name: impl Into<SharedString>) -> Self {
        Self {
            id,
            name: name.into(),
            driver: DriverKind::Sqlite,
            status: ConnStatus::Connected,
            schema: Arc::new(SchemaIndex::new(
                SchemaInfo::default(),
                DriverKind::Sqlite,
                None,
            )),
            stats: PoolStats::default(),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.status == ConnStatus::Connected
    }
}
