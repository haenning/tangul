//! Integration tests against real servers. Skipped unless the env vars are set, e.g.:
//!   RUSDB_TEST_PG=postgres:postgres@127.0.0.1:5432/postgres
//!   RUSDB_TEST_MYSQL=rusdb:rusdb@127.0.0.1:3306/app
//! (`docker compose up -d` provides both, see docker-compose.yml.)

use std::time::Duration;

use rusdb_core::{ConnId, ConnectConfig, DriverKind, QueryId, ResultSet, TabId, TlsMode};
use rusdb_engine::{Command, EngineHandle, Event, ExecEvent, QueryEvent};

fn config(var: &str) -> Option<ConnectConfig> {
    let url = std::env::var(var).ok()?;
    let (creds, rest) = url.split_once('@')?;
    let (user, password) = creds.split_once(':')?;
    let (hostport, database) = rest.split_once('/')?;
    let (host, port) = hostport.split_once(':')?;
    Some(ConnectConfig {
        host: host.into(),
        port: port.parse().ok()?,
        user: user.into(),
        password: Some(password.into()),
        database: database.into(),
        tls: TlsMode::Prefer,
    })
}

struct Harness {
    engine: EngineHandle,
    events: async_channel::Receiver<Event>,
    conn: ConnId,
    next_query: u64,
}

struct Outcome {
    result: Option<ResultSet>,
    error: Option<String>,
    in_transaction: bool,
    truncated: bool,
}

impl Harness {
    async fn connect(driver: DriverKind, cfg: ConnectConfig) -> Self {
        let (engine, events) = rusdb_engine::start();
        let conn = ConnId(1);
        engine.send(Command::Connect { conn, driver, cfg });
        let mut h = Self { engine, events, conn, next_query: 0 };
        loop {
            match h.next().await {
                Event::Connected { .. } => break,
                Event::ConnectFailed { error, .. } => panic!("connect failed: {error}"),
                _ => {}
            }
        }
        h
    }

    async fn next(&mut self) -> Event {
        tokio::time::timeout(Duration::from_secs(20), self.events.recv())
            .await
            .expect("timeout waiting for engine event")
            .expect("engine gone")
    }

    fn start(&mut self, tab: TabId, sql: &str) -> QueryId {
        self.next_query += 1;
        let query = QueryId(self.next_query);
        self.engine.send(Command::Execute { query, tab, conn: self.conn, sql: sql.into() });
        query
    }

    async fn wait(&mut self, query: QueryId) -> Outcome {
        let mut out = Outcome { result: None, error: None, in_transaction: false, truncated: false };
        loop {
            if let Event::Query { query: q, event } = self.next().await {
                assert_eq!(q, query);
                match event {
                    QueryEvent::Exec(ExecEvent::ResultStart(cols)) => out.result = Some(ResultSet::new(cols)),
                    QueryEvent::Exec(ExecEvent::Rows(b)) => out.result.as_mut().unwrap().append(b),
                    QueryEvent::Exec(ExecEvent::Truncated) => out.truncated = true,
                    QueryEvent::Exec(ExecEvent::StatementDone { .. }) => {}
                    QueryEvent::Finished { error, in_transaction, .. } => {
                        out.error = error;
                        out.in_transaction = in_transaction;
                        return out;
                    }
                }
            }
        }
    }

    async fn run(&mut self, tab: TabId, sql: &str) -> Outcome {
        let q = self.start(tab, sql);
        self.wait(q).await
    }

    async fn stats(&mut self) -> rusdb_engine::PoolStats {
        // Stats are sent after every release; drain until the latest one.
        let mut last = None;
        while let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_millis(200), self.events.recv()).await {
            if let Event::PoolStats { stats, .. } = ev {
                last = Some(stats);
            }
        }
        last.expect("no pool stats")
    }
}

async fn common_checks(driver: DriverKind, cfg: ConnectConfig, sleep_sql: &str) {
    let mut h = Harness::connect(driver, cfg).await;
    let tab_a = TabId(1);
    let tab_b = TabId(2);

    // Basic select with NULL, unicode and multiple result sets (last one wins in UI).
    let out = h.run(tab_a, "SELECT 1 AS a, NULL AS b, 'héllo' AS c; SELECT 2 AS x").await;
    assert_eq!(out.error, None);
    let rs = out.result.unwrap();
    assert_eq!(rs.columns()[0].name, "x");
    assert_eq!(rs.cell(0, 0), Some("2"));

    let out = h.run(tab_a, "SELECT 1 AS a, NULL AS b, 'héllo' AS c").await;
    let rs = out.result.unwrap();
    assert_eq!((rs.cell(0, 0), rs.cell(0, 1), rs.cell(0, 2)), (Some("1"), None, Some("héllo")));

    // Errors are reported, the session stays usable.
    let out = h.run(tab_a, "SELECT * FROM does_not_exist_xyz").await;
    assert!(out.error.is_some());
    assert_eq!(h.run(tab_a, "SELECT 1").await.error, None);

    // Idle tabs hold no connection: sequential queries reuse one session.
    assert_eq!(h.stats().await.open, 1);

    // BEGIN pins the session to tab A; tab B needs a second connection.
    let out = h.run(tab_a, "BEGIN").await;
    assert!(out.in_transaction, "BEGIN should pin");
    assert!(!h.run(tab_b, "SELECT 1").await.in_transaction);
    let s = h.stats().await;
    assert_eq!((s.open, s.pinned), (2, 1));
    let out = h.run(tab_a, "COMMIT").await;
    assert!(!out.in_transaction, "COMMIT should unpin");
    assert_eq!(h.stats().await.pinned, 0);

    // Cancel a long-running statement.
    let q = h.start(tab_a, sleep_sql);
    tokio::time::sleep(Duration::from_millis(500)).await;
    h.engine.send(Command::Cancel { query: q });
    let out = h.wait(q).await;
    assert!(out.error.is_some(), "cancelled query should report an error");
    assert_eq!(h.run(tab_a, "SELECT 1").await.error, None);
}

#[tokio::test]
async fn postgres() {
    let Some(cfg) = config("RUSDB_TEST_PG") else {
        eprintln!("RUSDB_TEST_PG not set, skipping");
        return;
    };
    common_checks(DriverKind::Postgres, cfg.clone(), "SELECT pg_sleep(30)").await;

    let mut h = Harness::connect(DriverKind::Postgres, cfg).await;
    let out = h.run(TabId(1), "SELECT g FROM generate_series(1, 25000) g").await;
    assert!(out.truncated);
    assert_eq!(out.result.unwrap().row_count(), rusdb_engine::ROW_LIMIT);

    // A failed statement inside a transaction keeps the tab pinned (aborted txn).
    h.run(TabId(1), "BEGIN").await;
    assert!(h.run(TabId(1), "SELECT 1/0").await.in_transaction);
    assert!(!h.run(TabId(1), "ROLLBACK").await.in_transaction);
}

#[tokio::test]
async fn mysql() {
    let Some(cfg) = config("RUSDB_TEST_MYSQL") else {
        eprintln!("RUSDB_TEST_MYSQL not set, skipping");
        return;
    };
    common_checks(DriverKind::Mysql, cfg.clone(), "SELECT SLEEP(30)").await;

    let mut h = Harness::connect(DriverKind::Mysql, cfg).await;
    let out = h
        .run(TabId(1), "CREATE TEMPORARY TABLE t (id int); INSERT INTO t VALUES (1),(2); SELECT * FROM t")
        .await;
    assert_eq!(out.error, None);
    assert_eq!(out.result.unwrap().row_count(), 2);
}
