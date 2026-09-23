//! Lazy connection pool per profile.
//!
//! Rules (see PLAN.md, D3):
//! - Nothing is opened before first use.
//! - A tab leases a session only while a statement runs and returns it afterwards,
//!   so idle tabs cost zero connections.
//! - If the statement left a transaction open, the session is pinned to the tab until
//!   the transaction ends (COMMIT/ROLLBACK) or the tab is released.
//! - Idle sessions are closed after `IDLE_TIMEOUT`.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::Result;
use rusdb_core::{ConnectConfig, TabId};
use rusdb_drivers::{Driver, Session};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// A session together with its slot in the pool. Dropping it frees the slot.
pub struct Lease {
    pub session: Box<dyn Session>,
    _permit: OwnedSemaphorePermit,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolStats {
    /// Open server connections (idle + pinned + in use).
    pub open: usize,
    pub idle: usize,
    pub pinned: usize,
    pub max: usize,
}

pub struct Pool {
    driver: Arc<dyn Driver>,
    cfg: ConnectConfig,
    max: usize,
    permits: Arc<Semaphore>,
    idle: Mutex<Vec<(Lease, Instant)>>,
    pinned: Mutex<HashMap<TabId, Lease>>,
}

impl Pool {
    pub fn new(driver: Arc<dyn Driver>, cfg: ConnectConfig, max: usize) -> Self {
        Self {
            driver,
            cfg,
            max,
            permits: Arc::new(Semaphore::new(max)),
            idle: Mutex::default(),
            pinned: Mutex::default(),
        }
    }

    /// Returns the tab's pinned session, an idle one, or opens a new one. Waits if
    /// all slots are in use.
    pub async fn acquire(&self, tab: Option<TabId>) -> Result<Lease> {
        if let Some(tab) = tab
            && let Some(lease) = self.pinned.lock().unwrap().remove(&tab)
        {
            return Ok(lease);
        }
        let permit = self.permits.clone().acquire_owned().await?;
        loop {
            // Take the most recently used idle session (warmest).
            let candidate = self.idle.lock().unwrap().pop();
            match candidate {
                Some((lease, _)) if lease.session.is_alive() => return Ok(lease),
                // Dead session: drop it and its permit, try the next one.
                Some(_) => continue,
                None => break,
            }
        }
        let session = self.driver.connect(&self.cfg).await?;
        Ok(Lease {
            session,
            _permit: permit,
        })
    }

    /// Gives a session back after a statement finished.
    pub fn release(&self, tab: Option<TabId>, lease: Lease) {
        if !lease.session.is_alive() {
            return;
        }
        // TODO(reserve-connection): a tab-level "reserve connection" button would set a
        // flag on the tab and pin here as well, independent of the transaction state.
        if let Some(tab) = tab
            && lease.session.in_transaction()
        {
            self.pinned.lock().unwrap().insert(tab, lease);
            return;
        }
        self.idle.lock().unwrap().push((lease, Instant::now()));
    }

    /// Drops the tab's pinned session. Closing the connection rolls back an open
    /// transaction on the server.
    pub fn release_tab(&self, tab: TabId) -> Option<Lease> {
        self.pinned.lock().unwrap().remove(&tab)
    }

    /// Removes sessions idle for longer than `IDLE_TIMEOUT` and returns them for closing.
    pub fn take_expired(&self) -> Vec<Lease> {
        let mut idle = self.idle.lock().unwrap();
        let now = Instant::now();
        let (expired, keep): (Vec<_>, Vec<_>) = idle
            .drain(..)
            .partition(|(_, since)| now.duration_since(*since) >= IDLE_TIMEOUT);
        *idle = keep;
        expired.into_iter().map(|(l, _)| l).collect()
    }

    /// Removes all idle and pinned sessions (disconnect).
    pub fn take_all(&self) -> Vec<Lease> {
        let mut out: Vec<Lease> = self.idle.lock().unwrap().drain(..).map(|(l, _)| l).collect();
        out.extend(self.pinned.lock().unwrap().drain().map(|(_, l)| l));
        out
    }

    pub fn stats(&self) -> PoolStats {
        PoolStats {
            open: self.max - self.permits.available_permits(),
            idle: self.idle.lock().unwrap().len(),
            pinned: self.pinned.lock().unwrap().len(),
            max: self.max,
        }
    }
}
