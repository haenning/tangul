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
use rusdb_core::{ConnId, ConnectConfig, DriverKind, QueryId, SchemaInfo, SshTarget, TabId};
pub use rusdb_drivers::ExecEvent;
use rusdb_drivers::{CancelHandle, driver_for};
use rusdb_tunnel::{HostKeyError, Tunnel};
use tokio::sync::mpsc;

/// Max rows kept per result set. TODO(fetch-more): make configurable / page on demand.
pub const ROW_LIMIT: usize = 10_000;
/// Max server connections per profile (tabs + schema browsing share them).
pub const MAX_CONNECTIONS: usize = 4;
/// Upper bound for a graceful disconnect; after that the socket is simply dropped.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum Command {
    /// Opens the pool and verifies credentials by loading the schema. With `ssh`,
    /// sessions go through a tunnel and `cfg.host`/`port` are as the SSH server sees them.
    Connect {
        conn: ConnId,
        driver: DriverKind,
        cfg: ConnectConfig,
        ssh: Option<SshTarget>,
    },
    /// The user confirmed an unknown SSH host key: add it to `~/.ssh/known_hosts`.
    TrustHostKey {
        host: String,
        port: u16,
        key: String,
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
    /// The SSH server's key is not in known_hosts. Connecting stopped; after the user
    /// confirms, send `TrustHostKey` and connect again.
    HostKeyUnknown {
        conn: ConnId,
        host: String,
        port: u16,
        fingerprint: String,
        key: String,
    },
    Schema {
        conn: ConnId,
        result: Result<SchemaInfo, String>,
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
        /// The user cancelled the query. Not every cancel produces an error (MySQL's
        /// `SLEEP()` just returns 1), so this is reported separately.
        cancelled: bool,
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

struct RunningQuery {
    cancel: Arc<dyn CancelHandle>,
    cancel_requested: bool,
}

type Running = Arc<Mutex<HashMap<QueryId, RunningQuery>>>;

struct Engine {
    events: Events,
    pools: HashMap<ConnId, Arc<Pool>>,
    running: Running,
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
            Command::Connect {
                conn,
                driver,
                cfg,
                ssh,
            } => {
                let tunnel = ssh.map(|ssh| Tunnel::new(ssh, cfg.host.clone(), cfg.port));
                let pool = Arc::new(Pool::new(driver_for(driver), cfg, tunnel, MAX_CONNECTIONS));
                if let Some(old) = self.pools.insert(conn, pool.clone()) {
                    close_all(old.take_all(), None);
                }
                let events = self.events.clone();
                tokio::spawn(async move {
                    match load_schema(&pool).await {
                        Ok(schema) => {
                            send(&events, Event::Connected { conn });
                            send(
                                &events,
                                Event::Schema {
                                    conn,
                                    result: Ok(schema),
                                },
                            );
                        }
                        Err(e) => match e.downcast_ref::<HostKeyError>() {
                            Some(HostKeyError::Unknown {
                                host,
                                port,
                                fingerprint,
                                key,
                            }) => send(
                                &events,
                                Event::HostKeyUnknown {
                                    conn,
                                    host: host.clone(),
                                    port: *port,
                                    fingerprint: fingerprint.clone(),
                                    key: key.clone(),
                                },
                            ),
                            _ => send(
                                &events,
                                Event::ConnectFailed {
                                    conn,
                                    error: format!("{e:#}"),
                                },
                            ),
                        },
                    }
                    send_stats(&events, conn, &pool);
                });
            }
            Command::TrustHostKey { host, port, key } => {
                // A small file append; done inline so a following Connect sees it.
                if let Err(e) = rusdb_tunnel::trust_host_key(&host, port, &key) {
                    eprintln!("could not update known_hosts: {e:#}");
                }
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
                    let result = load_schema(&pool).await.map_err(|e| format!("{e:#}"));
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
                        false,
                    );
                    return;
                };
                let events = self.events.clone();
                let running = self.running.clone();
                tokio::spawn(execute(pool, events, running, query, tab, conn, sql));
            }
            Command::Cancel { query } => {
                // A query still waiting for a free connection can't be cancelled yet.
                let handle = self.running.lock().unwrap().get_mut(&query).map(|r| {
                    r.cancel_requested = true;
                    r.cancel.clone()
                });
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
    running: Running,
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
                false,
            );
            return;
        }
    };
    send_stats(&events, conn, &pool);
    running.lock().unwrap().insert(
        query,
        RunningQuery {
            cancel: lease.session.cancel_handle(),
            cancel_requested: false,
        },
    );

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

    let cancelled = running
        .lock()
        .unwrap()
        .remove(&query)
        .is_some_and(|r| r.cancel_requested);
    let in_tx = lease.session.in_transaction();
    finish(
        &events,
        query,
        started.elapsed(),
        result.err().map(|e| format!("{e:#}")),
        in_tx,
        cancelled,
    );
    pool.release(Some(tab), lease);
    send_stats(&events, conn, &pool);
}

/// Tables are required; columns and foreign keys are best effort (they only feed
/// SQL assistance, and a restricted user may not be allowed to read them).
async fn load_schema(pool: &Pool) -> anyhow::Result<SchemaInfo> {
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

/// Frees the pool slots immediately (reported via stats if `report` is set) and closes
/// the sessions in the background. A slow or hanging server disconnect must neither block
/// the pool nor delay the stats.
fn close_all(leases: Vec<Lease>, report: Option<(Events, ConnId, Arc<Pool>)>) {
    if leases.is_empty() {
        return;
    }
    let sessions: Vec<_> = leases.into_iter().map(Lease::into_session).collect();
    if let Some((events, conn, pool)) = report {
        send_stats(&events, conn, &pool);
    }
    tokio::spawn(async move {
        for session in sessions {
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, session.close()).await;
        }
    });
}

fn finish(
    events: &Events,
    query: QueryId,
    elapsed: Duration,
    error: Option<String>,
    in_transaction: bool,
    cancelled: bool,
) {
    send(
        events,
        Event::Query {
            query,
            event: QueryEvent::Finished {
                elapsed,
                error,
                in_transaction,
                cancelled,
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
