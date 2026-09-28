//! The engine itself: owns the connections and handles one command at a time.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tangul_assist::dialect::dialect_for;
use tangul_core::{ConnId, ConnectConfig, DriverKind, QueryId, SchemaInfo, SshTarget, TabId};
use tangul_data::local::Local;
use tangul_drivers::driver_for;
use tangul_tunnel::{HostKeyError, Tunnel};
use tokio::sync::{mpsc, oneshot};

use crate::audit::{Audit, Trace};
use crate::gate::{Approval, Caller, Enforced, Gate};
use crate::pool::{Pool, PoolStats};
use crate::run::{
    Events, Job, Running, close_all, execute, finish, run_query, send, send_stats, validate,
};
use crate::schema::{load_extras, load_schema};
use crate::{Command, EngineConfig, Event, MAX_CONNECTIONS, QueryError, QueryOptions, QueryOutput};

pub(crate) struct Engine {
    events: Events,
    pools: HashMap<ConnId, Arc<Pool>>,
    running: Running,
    gate: Gate,
    audit: Audit,
}

/// How often the settings file is checked for changes.
/// Why the assistant's statement that isn't a read did not run.
const PROPOSED: &str = "not run: the assistant only reads; statements that change something \
                        are proposals the user opens and runs themselves";
const SETTINGS_POLL: Duration = Duration::from_secs(1);

impl Engine {
    pub(crate) fn new(events: Events, config: EngineConfig) -> Self {
        let audit = config
            .audit_dir
            .map(|dir| Arc::new(Mutex::new(Local::open(Some(&dir)))));
        Self {
            events,
            pools: HashMap::new(),
            running: Arc::default(),
            gate: Gate::new(config.settings),
            audit: Audit(audit),
        }
    }

    pub(crate) async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Command>) {
        let mut reap = tokio::time::interval(Duration::from_secs(10));
        let mut settings = tokio::time::interval(SETTINGS_POLL);
        loop {
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(cmd) => self.handle(cmd),
                    None => break, // UI gone
                },
                _ = reap.tick() => self.reap_idle(),
                _ = settings.tick() => self.gate.reload(),
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
            } => self.connect(conn, driver, cfg, ssh),
            Command::TrustHostKey { host, port, key } => self.trust_host_key(host, port, key),
            Command::Disconnect { conn } => self.disconnect(conn),
            Command::RefreshSchema { conn } => self.refresh_schema(conn),
            Command::ReloadSettings => self.gate.reload_now(),
            Command::Execute {
                query,
                tab,
                conn,
                sql,
                approval,
            } => self.execute(query, tab, conn, sql, approval),
            Command::Query {
                query,
                conn,
                sql,
                options,
                reply,
            } => self.query(query, conn, sql, options, reply),
            Command::Validate {
                request,
                conn,
                statements,
            } => self.validate(request, conn, statements),
            Command::Cancel { query } => self.cancel(query),
            Command::ReleaseTab { tab, conn } => self.release_tab(tab, conn),
        }
    }

    /// Opens a connection's pool and loads its schema; the first session proves the login.
    fn connect(
        &mut self,
        conn: ConnId,
        driver: DriverKind,
        cfg: ConnectConfig,
        ssh: Option<SshTarget>,
    ) {
        let lexing = dialect_for(driver).lexing();
        let Some(driver) = driver_for(driver) else {
            send(
                &self.events,
                Event::ConnectFailed {
                    conn,
                    error: format!("this build of tangul has no {} support", driver.label()),
                },
            );
            return;
        };
        let tunnel = ssh.map(|ssh| Tunnel::new(ssh, cfg.host.clone(), cfg.port));
        let pool = Arc::new(Pool::new(driver, lexing, cfg, tunnel, MAX_CONNECTIONS));
        if let Some(old) = self.pools.insert(conn, pool.clone()) {
            close_all(old.take_all(), None);
        }
        let events = self.events.clone();
        tokio::spawn(async move {
            match load_schema(&pool).await {
                Ok(schema) => {
                    send(&events, Event::Connected { conn });
                    send_schema(&events, &pool, conn, schema).await;
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

    fn trust_host_key(&self, host: String, port: u16, key: String) {
        // A small file append; done inline so a following Connect sees it.
        if let Err(e) = tangul_tunnel::trust_host_key(&host, port, &key) {
            eprintln!("could not update known_hosts: {e:#}");
        }
    }

    fn disconnect(&mut self, conn: ConnId) {
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

    fn refresh_schema(&self, conn: ConnId) {
        let Some(pool) = self.pools.get(&conn).cloned() else {
            return;
        };
        let events = self.events.clone();
        tokio::spawn(async move {
            match load_schema(&pool).await {
                Ok(schema) => send_schema(&events, &pool, conn, schema).await,
                Err(e) => send(
                    &events,
                    Event::Schema {
                        conn,
                        result: Err(format!("{e:#}")),
                        complete: true,
                    },
                ),
            }
            send_stats(&events, conn, &pool);
        });
    }

    /// A tab's statement (the user's), streamed as events.
    fn execute(
        &mut self,
        query: QueryId,
        tab: TabId,
        conn: ConnId,
        sql: String,
        approval: Approval,
    ) {
        let caller = Caller::User;
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
        let trace = Trace {
            audit: self.audit.clone(),
            caller,
            target: pool.target(),
        };
        let (run, sql) = match self
            .gate
            .enforce(pool.user(), &sql, pool.lexing(), caller, approval)
        {
            Enforced::Run { run, sql } => (run, sql),
            refused => {
                // Only the assistant's statements are proposed, never the user's.
                let reason = match refused {
                    Enforced::Refused(reason) => reason,
                    _ => PROPOSED.to_string(),
                };
                trace.record(&sql, "refused", Some(reason.clone()), None, None);
                finish(
                    &self.events,
                    query,
                    Duration::ZERO,
                    Some((reason, None)),
                    false,
                    false,
                );
                return;
            }
        };
        let events = self.events.clone();
        let running = self.running.clone();
        let job = Job {
            query,
            tab,
            conn,
            sql,
            read_only: run.read_only,
            timeout: run.timeout,
            trace,
        };
        tokio::spawn(execute(pool, events, running, job));
    }

    /// A statement whose whole result is sent back on `reply`.
    fn query(
        &mut self,
        query: QueryId,
        conn: ConnId,
        sql: String,
        options: QueryOptions,
        reply: oneshot::Sender<Result<QueryOutput, QueryError>>,
    ) {
        let Some(pool) = self.pools.get(&conn).cloned() else {
            let _ = reply.send(Err(QueryError::new("not connected")));
            return;
        };
        let trace = Trace {
            audit: self.audit.clone(),
            caller: options.caller,
            target: pool.target(),
        };
        let (run, text) = match self.gate.enforce(
            pool.user(),
            &sql,
            pool.lexing(),
            options.caller,
            options.approval,
        ) {
            // The audit keeps what was asked; the server gets what the gate passed.
            Enforced::Run { run, sql: text } => (run, text),
            Enforced::Proposed(proposal) => {
                trace.record(&sql, "proposed", None, None, None);
                let _ = reply.send(Err(QueryError {
                    proposal: Some(proposal),
                    ..QueryError::new(PROPOSED)
                }));
                return;
            }
            Enforced::Refused(reason) => {
                trace.record(&sql, "refused", Some(reason.clone()), None, None);
                let _ = reply.send(Err(QueryError::new(reason)));
                return;
            }
        };
        let (events, running) = (self.events.clone(), self.running.clone());
        tokio::spawn(async move {
            let output = run_query(&pool, &running, query, &text, run, options.row_limit).await;
            match &output {
                Ok(out) => trace.record(&sql, "ran", None, Some(out.elapsed), out.rows),
                Err(e) => trace.record(&sql, "ran", Some(e.message.clone()), None, None),
            }
            let _ = reply.send(output);
            send_stats(&events, conn, &pool);
        });
    }

    fn validate(&self, request: u64, conn: ConnId, statements: Vec<(usize, String)>) {
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

    fn cancel(&self, query: QueryId) {
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

    fn release_tab(&self, tab: TabId, conn: ConnId) {
        if let Some(pool) = self.pools.get(&conn)
            && let Some(lease) = pool.release_tab(tab)
        {
            close_all(vec![lease], Some((self.events.clone(), conn, pool.clone())));
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

/// Sends `schema` at once (what the UI needs), then again with the extras.
async fn send_schema(events: &Events, pool: &Pool, conn: ConnId, schema: SchemaInfo) {
    send(
        events,
        Event::Schema {
            conn,
            result: Ok(schema.clone()),
            complete: false,
        },
    );
    let extras = load_extras(pool).await;
    send(
        events,
        Event::Schema {
            conn,
            result: Ok(schema.with_extras(extras)),
            complete: true,
        },
    );
}
