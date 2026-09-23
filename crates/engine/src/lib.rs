//! Backend: owns a tokio runtime on its own thread and all database sessions.
//! The UI talks to it only through `Command`s and receives `Event`s, so the UI
//! thread never blocks on the network.

mod pool;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub use pool::PoolStats;
use pool::{Lease, Pool};
use rusdb_core::{ConnId, ConnectConfig, DriverKind, QueryId, TabId, TableInfo};
pub use rusdb_drivers::ExecEvent;
use rusdb_drivers::{CancelHandle, driver_for};
use tokio::sync::mpsc;

/// Max rows kept per result set. TODO(fetch-more): make configurable / page on demand.
pub const ROW_LIMIT: usize = 10_000;
/// Max server connections per profile (tabs + schema browsing share them).
pub const MAX_CONNECTIONS: usize = 4;

#[derive(Debug)]
pub enum Command {
    /// Opens the pool and verifies credentials by loading the schema.
    Connect {
        conn: ConnId,
        driver: DriverKind,
        cfg: ConnectConfig,
    },
    Disconnect {
        conn: ConnId,
    },
    RefreshSchema {
        conn: ConnId,
    },
    Execute {
        query: QueryId,
        tab: TabId,
        conn: ConnId,
        sql: String,
    },
    Cancel {
        query: QueryId,
    },
    /// Tab closed or switched connection: drop its pinned session (rolls back).
    ReleaseTab {
        tab: TabId,
        conn: ConnId,
    },
}

#[derive(Debug)]
pub enum Event {
    Connected {
        conn: ConnId,
    },
    ConnectFailed {
        conn: ConnId,
        error: String,
    },
    Schema {
        conn: ConnId,
        result: Result<Vec<TableInfo>, String>,
    },
    Query {
        query: QueryId,
        event: QueryEvent,
    },
    PoolStats {
        conn: ConnId,
        stats: PoolStats,
    },
}

#[derive(Debug)]
pub enum QueryEvent {
    Exec(ExecEvent),
    Finished {
        elapsed: Duration,
        error: Option<String>,
        /// The tab's session stays pinned while this is true.
        in_transaction: bool,
    },
}

/// Cheap to clone; sending never blocks.
#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::UnboundedSender<Command>,
}

impl EngineHandle {
    pub fn send(&self, cmd: Command) {
        // Only fails if the engine thread died; nothing sensible to do then.
        let _ = self.tx.send(cmd);
    }
}

/// Starts the engine thread. Events arrive on the returned receiver, which can be
/// awaited from any executor (the UI uses GPUI's).
pub fn start() -> (EngineHandle, async_channel::Receiver<Event>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (events_tx, events_rx) = async_channel::unbounded();
    std::thread::Builder::new()
        .name("rusdb-engine".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(Engine::new(events_tx).run(rx));
        })
        .expect("engine thread");
    (EngineHandle { tx }, events_rx)
}

type Events = async_channel::Sender<Event>;

struct Engine {
    events: Events,
    pools: HashMap<ConnId, Arc<Pool>>,
    running: Arc<Mutex<HashMap<QueryId, Arc<dyn CancelHandle>>>>,
}

impl Engine {
    fn new(events: Events) -> Self {
        Self {
            events,
            pools: HashMap::new(),
            running: Arc::default(),
        }
    }

    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Command>) {
        let mut reap = tokio::time::interval(Duration::from_secs(10));
        loop {
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(cmd) => self.handle(cmd),
                    None => break, // UI gone
                },
                _ = reap.tick() => self.reap_idle(),
            }
        }
    }

    fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Connect { conn, driver, cfg } => {
                let pool = Arc::new(Pool::new(driver_for(driver), cfg, MAX_CONNECTIONS));
                if let Some(old) = self.pools.insert(conn, pool.clone()) {
                    close_all(old.take_all(), None);
                }
                let events = self.events.clone();
                tokio::spawn(async move {
                    match load_schema(&pool).await {
                        Ok(tables) => {
                            send(&events, Event::Connected { conn });
                            send(
                                &events,
                                Event::Schema {
                                    conn,
                                    result: Ok(tables),
                                },
                            );
                        }
                        Err(e) => send(&events, Event::ConnectFailed { conn, error: e }),
                    }
                    send_stats(&events, conn, &pool);
                });
            }
            Command::Disconnect { conn } => {
                if let Some(pool) = self.pools.remove(&conn) {
                    close_all(pool.take_all(), None);
                    send(
                        &self.events,
                        Event::PoolStats {
                            conn,
                            stats: PoolStats::default(),
                        },
                    );
                }
            }
            Command::RefreshSchema { conn } => {
                let Some(pool) = self.pools.get(&conn).cloned() else {
                    return;
                };
                let events = self.events.clone();
                tokio::spawn(async move {
                    let result = load_schema(&pool).await;
                    send(&events, Event::Schema { conn, result });
                    send_stats(&events, conn, &pool);
                });
            }
            Command::Execute {
                query,
                tab,
                conn,
                sql,
            } => {
                let Some(pool) = self.pools.get(&conn).cloned() else {
                    finish(
                        &self.events,
                        query,
                        Duration::ZERO,
                        Some("not connected".into()),
                        false,
                    );
                    return;
                };
                let events = self.events.clone();
                let running = self.running.clone();
                tokio::spawn(execute(pool, events, running, query, tab, conn, sql));
            }
            Command::Cancel { query } => {
                let handle = self.running.lock().unwrap().get(&query).cloned();
                if let Some(handle) = handle {
                    tokio::spawn(async move {
                        if let Err(e) = handle.cancel().await {
                            eprintln!("cancel failed: {e:#}");
                        }
                    });
                }
            }
            Command::ReleaseTab { tab, conn } => {
                if let Some(pool) = self.pools.get(&conn)
                    && let Some(lease) = pool.release_tab(tab)
                {
                    close_all(vec![lease], Some((self.events.clone(), conn, pool.clone())));
                }
            }
        }
    }

    fn reap_idle(&self) {
        for (conn, pool) in &self.pools {
            close_all(
                pool.take_expired(),
                Some((self.events.clone(), *conn, pool.clone())),
            );
        }
    }
}

async fn execute(
    pool: Arc<Pool>,
    events: Events,
    running: Arc<Mutex<HashMap<QueryId, Arc<dyn CancelHandle>>>>,
    query: QueryId,
    tab: TabId,
    conn: ConnId,
    sql: String,
) {
    let started = Instant::now();
    let mut lease = match pool.acquire(Some(tab)).await {
        Ok(l) => l,
        Err(e) => {
            finish(
                &events,
                query,
                started.elapsed(),
                Some(format!("{e:#}")),
                false,
            );
            return;
        }
    };
    send_stats(&events, conn, &pool);
    running
        .lock()
        .unwrap()
        .insert(query, lease.session.cancel_handle());

    let mut sink = |ev: ExecEvent| {
        send(
            &events,
            Event::Query {
                query,
                event: QueryEvent::Exec(ev),
            },
        )
    };
    let result = lease.session.execute(&sql, ROW_LIMIT, &mut sink).await;

    running.lock().unwrap().remove(&query);
    let in_tx = lease.session.in_transaction();
    finish(
        &events,
        query,
        started.elapsed(),
        result.err().map(|e| format!("{e:#}")),
        in_tx,
    );
    pool.release(Some(tab), lease);
    send_stats(&events, conn, &pool);
}

async fn load_schema(pool: &Pool) -> Result<Vec<TableInfo>, String> {
    let mut lease = pool.acquire(None).await.map_err(|e| format!("{e:#}"))?;
    let result = lease
        .session
        .list_tables()
        .await
        .map_err(|e| format!("{e:#}"));
    pool.release(None, lease);
    result
}

/// Closes sessions in the background. With `report`, pool stats are sent once the
/// sessions (and their pool slots) are gone.
fn close_all(leases: Vec<Lease>, report: Option<(Events, ConnId, Arc<Pool>)>) {
    if leases.is_empty() {
        return;
    }
    tokio::spawn(async move {
        for lease in leases {
            lease.session.close().await;
        }
        if let Some((events, conn, pool)) = report {
            send_stats(&events, conn, &pool);
        }
    });
}

fn finish(events: &Events, query: QueryId, elapsed: Duration, error: Option<String>, in_tx: bool) {
    send(
        events,
        Event::Query {
            query,
            event: QueryEvent::Finished {
                elapsed,
                error,
                in_transaction: in_tx,
            },
        },
    );
}

fn send_stats(events: &Events, conn: ConnId, pool: &Pool) {
    send(
        events,
        Event::PoolStats {
            conn,
            stats: pool.stats(),
        },
    );
}

fn send(events: &Events, event: Event) {
    // Unbounded: only fails when the UI is gone.
    let _ = events.try_send(event);
}
