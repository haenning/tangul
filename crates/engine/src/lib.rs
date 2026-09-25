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
use tangul_core::{
    ConnId, ConnectConfig, DriverKind, QueryId, ResultSet, SchemaInfo, SshTarget, TabId,
};
pub use tangul_drivers::ExecEvent;
use tangul_drivers::{CancelHandle, SqlError, driver_for};
use tangul_tunnel::{HostKeyError, Tunnel};
use tokio::sync::{mpsc, oneshot};

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
        /// The server refuses writes for this statement (read-only mode).
        read_only: bool,
        /// Stop the statement after this long (the server, and the engine too).
        timeout: Option<Duration>,
    },
    /// Runs `sql` on a spare session (never a tab's) and answers on `reply` with the
    /// whole result instead of streaming events. See [`EngineHandle::query`].
    Query {
        query: QueryId,
        conn: ConnId,
        sql: String,
        options: QueryOptions,
        reply: oneshot::Sender<Result<QueryOutput, QueryError>>,
    },
    Cancel {
        query: QueryId,
    },
    /// Checks statements without running them (prepare only), on a spare session so
    /// an open transaction of a tab is never touched. `statements` are
    /// (byte offset in the editor, text).
    Validate {
        request: u64,
        conn: ConnId,
        statements: Vec<(usize, String)>,
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
    /// Result of `Validate`: one entry per failing statement.
    Validated {
        request: u64,
        problems: Vec<Problem>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Index into the request's statements.
    pub statement: usize,
    /// Byte offset within that statement, when the server said where.
    pub position: Option<usize>,
    pub message: String,
}

#[derive(Debug)]
pub enum QueryEvent {
    Exec(ExecEvent),
    Finished {
        elapsed: Duration,
        error: Option<String>,
        /// Byte offset of the error in the executed SQL, when the server reported one.
        error_position: Option<usize>,
        /// The tab's session stays pinned while this is true.
        in_transaction: bool,
        /// The user cancelled the query. Not every cancel produces an error (MySQL's
        /// `SLEEP()` just returns 1), so this is reported separately.
        cancelled: bool,
    },
}

/// How a [`Command::Query`] runs.
#[derive(Debug, Clone)]
pub struct QueryOptions {
    /// The server refuses writes.
    pub read_only: bool,
    /// Stop the statement after this long (the server, and the engine too).
    pub timeout: Option<Duration>,
    /// Rows kept per result set; the rest are dropped and `truncated` is set.
    pub row_limit: usize,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            read_only: true,
            timeout: None,
            row_limit: ROW_LIMIT,
        }
    }
}

/// The whole result of a [`Command::Query`].
#[derive(Debug, Default)]
pub struct QueryOutput {
    /// One per statement that returned rows.
    pub results: Vec<ResultSet>,
    /// Rows the last statement returned or changed, as the server reported them.
    pub rows: Option<u64>,
    /// A result set had more rows than `row_limit`.
    pub truncated: bool,
    pub elapsed: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryError {
    pub message: String,
    /// Byte offset of the error in the SQL, when the server said where.
    pub position: Option<usize>,
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for QueryError {}

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

    /// Runs `sql` on `conn` and returns the whole result, for callers that want an
    /// answer rather than a stream (scheduled runs, the assistant). Works from any
    /// executor. `query` identifies it for [`Command::Cancel`].
    pub async fn query(
        &self,
        query: QueryId,
        conn: ConnId,
        sql: impl Into<String>,
        options: QueryOptions,
    ) -> Result<QueryOutput, QueryError> {
        let (reply, answer) = oneshot::channel();
        self.send(Command::Query {
            query,
            conn,
            sql: sql.into(),
            options,
            reply,
        });
        answer.await.unwrap_or_else(|_| {
            Err(QueryError {
                message: "the engine stopped".into(),
                position: None,
            })
        })
    }
}

/// Starts the engine thread. Events arrive on the returned receiver, which can be
/// awaited from any executor (the UI uses GPUI's).
pub fn start() -> (EngineHandle, async_channel::Receiver<Event>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (events_tx, events_rx) = async_channel::unbounded();
    std::thread::Builder::new()
        .name("tangul-engine".into())
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
                if let Err(e) = tangul_tunnel::trust_host_key(&host, port, &key) {
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
                read_only,
                timeout,
            } => {
                let Some(pool) = self.pools.get(&conn).cloned() else {
                    finish(
                        &self.events,
                        query,
                        Duration::ZERO,
                        Some(("not connected".into(), None)),
                        false,
                        false,
                    );
                    return;
                };
                let events = self.events.clone();
                let running = self.running.clone();
                let job = Job {
                    query,
                    tab,
                    conn,
                    sql,
                    read_only,
                    timeout,
                };
                tokio::spawn(execute(pool, events, running, job));
            }
            Command::Query {
                query,
                conn,
                sql,
                options,
                reply,
            } => {
                let Some(pool) = self.pools.get(&conn).cloned() else {
                    let _ = reply.send(Err(QueryError {
                        message: "not connected".into(),
                        position: None,
                    }));
                    return;
                };
                let (events, running) = (self.events.clone(), self.running.clone());
                tokio::spawn(async move {
                    let output = run_query(&pool, &running, query, &sql, &options).await;
                    let _ = reply.send(output);
                    send_stats(&events, conn, &pool);
                });
            }
            Command::Validate {
                request,
                conn,
                statements,
            } => {
                let Some(pool) = self.pools.get(&conn).cloned() else {
                    return;
                };
                let events = self.events.clone();
                tokio::spawn(async move {
                    if let Some(problems) = validate(&pool, &statements).await {
                        send(&events, Event::Validated { request, problems });
                    }
                });
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

/// A statement to run on a tab's session.
struct Job {
    query: QueryId,
    tab: TabId,
    conn: ConnId,
    sql: String,
    read_only: bool,
    timeout: Option<Duration>,
}

async fn execute(pool: Arc<Pool>, events: Events, running: Running, job: Job) {
    let Job {
        query,
        tab,
        conn,
        sql,
        read_only,
        timeout,
    } = job;
    let started = Instant::now();
    let mut lease = match pool.acquire(Some(tab)).await {
        Ok(l) => l,
        Err(e) => {
            finish(
                &events,
                query,
                started.elapsed(),
                Some(describe(&e)),
                false,
                false,
            );
            return;
        }
    };
    let synced = match sync_read_only(&mut lease, read_only).await {
        Ok(()) => sync_timeout(&mut lease, timeout).await,
        Err(e) => Err(e),
    };
    if let Err(e) = synced {
        let in_tx = lease.session.in_transaction();
        finish(
            &events,
            query,
            started.elapsed(),
            Some(describe(&e)),
            in_tx,
            false,
        );
        pool.release(Some(tab), lease);
        send_stats(&events, conn, &pool);
        return;
    }
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
    let result = execute_limited(&mut lease, &sql, ROW_LIMIT, timeout, &mut sink).await;

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
        result.err().map(|e| describe(&e)),
        in_tx,
        cancelled,
    );
    pool.release(Some(tab), lease);
    send_stats(&events, conn, &pool);
}

/// Runs `sql` on the session, stopping it at `limit`. The server stops a statement
/// at the time limit, too; this also covers one it doesn't (SQLite, MySQL for
/// anything but SELECT).
async fn execute_limited(
    lease: &mut Lease,
    sql: &str,
    row_limit: usize,
    limit: Option<Duration>,
    sink: tangul_drivers::Sink<'_>,
) -> anyhow::Result<()> {
    let cancel = lease.session.cancel_handle();
    let mut timed_out = false;
    let result = {
        let exec = lease.session.execute(sql, row_limit, sink);
        tokio::pin!(exec);
        match limit {
            None => exec.await,
            Some(limit) => tokio::select! {
                result = &mut exec => result,
                _ = tokio::time::sleep(limit) => {
                    timed_out = true;
                    if let Err(e) = cancel.cancel().await {
                        eprintln!("cancel after time limit failed: {e:#}");
                    }
                    exec.await
                }
            },
        }
    };
    match (timed_out, limit) {
        (true, Some(limit)) => Err(anyhow::anyhow!(
            "stopped after the time limit of {}s",
            limit.as_secs_f64()
        )),
        _ => result,
    }
}

/// A [`Command::Query`]: a spare session, the whole result collected. A session left
/// inside a transaction is closed rather than reused (the server rolls it back).
async fn run_query(
    pool: &Arc<Pool>,
    running: &Running,
    query: QueryId,
    sql: &str,
    options: &QueryOptions,
) -> Result<QueryOutput, QueryError> {
    let error = |e: &anyhow::Error| {
        let (message, position) = describe(e);
        QueryError { message, position }
    };
    let started = Instant::now();
    let mut lease = pool.acquire(None).await.map_err(|e| error(&e))?;
    let synced = match sync_read_only(&mut lease, options.read_only).await {
        Ok(()) => sync_timeout(&mut lease, options.timeout).await,
        Err(e) => Err(e),
    };
    if let Err(e) = synced {
        pool.release(None, lease);
        return Err(error(&e));
    }
    running.lock().unwrap().insert(
        query,
        RunningQuery {
            cancel: lease.session.cancel_handle(),
            cancel_requested: false,
        },
    );
    let mut output = QueryOutput::default();
    let mut sink = |ev: ExecEvent| match ev {
        ExecEvent::ResultStart(columns) => output.results.push(ResultSet::new(columns)),
        ExecEvent::Rows(batch) => {
            if let Some(result) = output.results.last_mut() {
                result.append(batch);
            }
        }
        ExecEvent::StatementDone { rows } => output.rows = Some(rows),
        ExecEvent::Truncated => output.truncated = true,
    };
    let result = execute_limited(
        &mut lease,
        sql,
        options.row_limit,
        options.timeout,
        &mut sink,
    )
    .await;
    running.lock().unwrap().remove(&query);
    if lease.session.in_transaction() {
        close_all(vec![lease], None);
    } else {
        pool.release(None, lease);
    }
    result.map_err(|e| error(&e))?;
    output.elapsed = started.elapsed();
    Ok(output)
}

/// Makes the server side match `want` before a statement runs. In read-only mode it is
/// applied before every statement, since a script could have switched it off; that
/// costs one round trip. An open transaction can't be changed, so read-only mode
/// refuses to run inside one that may write.
async fn sync_read_only(lease: &mut Lease, want: bool) -> anyhow::Result<()> {
    if lease.session.in_transaction() {
        if want && !lease.read_only {
            anyhow::bail!(
                "Read-only mode is on, but this tab has an open transaction that can write. \
                 Commit or roll it back first."
            );
        }
        return Ok(());
    }
    if want || lease.read_only {
        lease.session.set_read_only(want).await?;
        lease.read_only = want;
    }
    Ok(())
}

/// Tells the server the statement time limit when it changed for this session.
async fn sync_timeout(lease: &mut Lease, want: Option<Duration>) -> anyhow::Result<()> {
    if lease.timeout != want {
        lease.session.set_statement_timeout(want).await?;
        lease.timeout = want;
    }
    Ok(())
}

/// Longest wait for a spare session; validation is skipped when all are busy.
const VALIDATE_WAIT: Duration = Duration::from_secs(2);

/// `None` when no session was free in time (the caller keeps its last result).
async fn validate(pool: &Pool, statements: &[(usize, String)]) -> Option<Vec<Problem>> {
    let mut lease = tokio::time::timeout(VALIDATE_WAIT, pool.acquire(None))
        .await
        .ok()?
        .ok()?;
    let mut problems = Vec::new();
    for (i, (_, sql)) in statements.iter().enumerate() {
        if let Err(e) = lease.session.validate(sql).await {
            let (message, position) = describe(&e);
            problems.push(Problem {
                statement: i,
                position,
                message,
            });
        }
    }
    pool.release(None, lease);
    Some(problems)
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

/// Message and (for statement errors) position of an execution error.
fn describe(e: &anyhow::Error) -> (String, Option<usize>) {
    let position = e.downcast_ref::<SqlError>().and_then(|e| e.position);
    (format!("{e:#}"), position)
}

fn finish(
    events: &Events,
    query: QueryId,
    elapsed: Duration,
    error: Option<(String, Option<usize>)>,
    in_transaction: bool,
    cancelled: bool,
) {
    send(
        events,
        Event::Query {
            query,
            event: QueryEvent::Finished {
                elapsed,
                error_position: error.as_ref().and_then(|e| e.1),
                error: error.map(|e| e.0),
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
