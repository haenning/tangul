//! An open connection as an entity of its own: the tabs using it and the workspace
//! point to the same one, so its status and pool stats are never copies. What is
//! known about its database (schema, usage) is in `tangul_service::knowledge`.

use gpui_kit::SharedString;
use tangul_assist::dialect::{Dialect, dialect_for};
use tangul_core::{ConnId, DriverKind};
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
            stats: PoolStats::default(),
        }
    }

    /// How its database spells SQL.
    pub fn dialect(&self) -> &'static dyn Dialect {
        dialect_for(self.driver)
    }

    pub fn is_connected(&self) -> bool {
        self.status == ConnStatus::Connected
    }
}
