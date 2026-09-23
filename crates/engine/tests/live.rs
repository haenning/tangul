//! Integration tests against real servers. Skipped unless the env vars are set, e.g.:
//!   RUSDB_TEST_PG=postgres:@127.0.0.1:55432/postgres
//!   RUSDB_TEST_MYSQL=root:@127.0.0.1:53306/app
//!   RUSDB_TEST_SSH=rusdb:rusdb@127.0.0.1:52222  (bastion that reaches postgres:5432)
//! (`make test-live` starts the databases from docker-compose.yml and sets all three.)

use std::time::Duration;

use rusdb_core::{
    ConnId, ConnectConfig, DriverKind, QueryId, ResultSet, SshAuth, SshConfig, SshTarget, TabId,
    TlsMode,
};
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
        password: (!password.is_empty()).then(|| password.into()),
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
    cancelled: bool,
}

impl Harness {
    async fn connect(driver: DriverKind, cfg: ConnectConfig) -> Self {
        let (engine, events) = rusdb_engine::start();
        let conn = ConnId(1);
        engine.send(Command::Connect {
            conn,
            driver,
            cfg,
            ssh: None,
        });
        let mut h = Self {
            engine,
            events,
            conn,
            next_query: 0,
        };
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
        self.engine.send(Command::Execute {
            query,
            tab,
            conn: self.conn,
            sql: sql.into(),
        });
        query
    }

    async fn wait(&mut self, query: QueryId) -> Outcome {
        let mut out = Outcome {
            result: None,
            error: None,
            in_transaction: false,
            truncated: false,
            cancelled: false,
        };
        loop {
            if let Event::Query { query: q, event } = self.next().await {
                assert_eq!(q, query);
                match event {
                    QueryEvent::Exec(ExecEvent::ResultStart(cols)) => {
                        out.result = Some(ResultSet::new(cols))
                    }
                    QueryEvent::Exec(ExecEvent::Rows(b)) => out.result.as_mut().unwrap().append(b),
                    QueryEvent::Exec(ExecEvent::Truncated) => out.truncated = true,
                    QueryEvent::Exec(ExecEvent::StatementDone { .. }) => {}
                    QueryEvent::Finished {
                        error,
                        in_transaction,
                        cancelled,
                        ..
                    } => {
                        out.error = error;
                        out.in_transaction = in_transaction;
                        out.cancelled = cancelled;
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
        while let Ok(Ok(ev)) =
            tokio::time::timeout(Duration::from_millis(200), self.events.recv()).await
        {
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
    let out = h
        .run(
            tab_a,
            "SELECT 1 AS a, NULL AS b, 'héllo' AS c; SELECT 2 AS x",
        )
        .await;
    assert_eq!(out.error, None);
    let rs = out.result.unwrap();
    assert_eq!(rs.columns()[0].name, "x");
    assert_eq!(rs.cell(0, 0), Some("2"));

    let out = h.run(tab_a, "SELECT 1 AS a, NULL AS b, 'héllo' AS c").await;
    let rs = out.result.unwrap();
    assert_eq!(
        (rs.cell(0, 0), rs.cell(0, 1), rs.cell(0, 2)),
        (Some("1"), None, Some("héllo"))
    );

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

    // Closing a tab with an open transaction closes its pinned session.
    h.run(tab_a, "BEGIN").await;
    h.engine.send(Command::ReleaseTab {
        tab: tab_a,
        conn: h.conn,
    });
    let s = h.stats().await;
    assert_eq!((s.open, s.pinned), (1, 0));

    // Cancel a long-running statement.
    let q = h.start(tab_a, sleep_sql);
    tokio::time::sleep(Duration::from_millis(500)).await;
    h.engine.send(Command::Cancel { query: q });
    let started = std::time::Instant::now();
    let out = h.wait(q).await;
    assert!(out.cancelled, "cancel should be reported");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "cancelled query should stop early"
    );
    assert_eq!(h.run(tab_a, "SELECT 1").await.error, None);
}

/// The seed data has `orders.user_id` referencing `users.id` in schema `schema`.
async fn schema_checks(driver: DriverKind, cfg: ConnectConfig, schema: &str) {
    let (engine, events) = rusdb_engine::start();
    engine.send(Command::Connect {
        conn: ConnId(1),
        driver,
        cfg,
        ssh: None,
    });
    let info = loop {
        let ev = tokio::time::timeout(Duration::from_secs(20), events.recv())
            .await
            .expect("timeout waiting for schema")
            .expect("engine gone");
        match ev {
            Event::Schema { result, .. } => break result.expect("schema failed"),
            Event::ConnectFailed { error, .. } => panic!("connect failed: {error}"),
            _ => {}
        }
    };
    let col = info
        .columns
        .iter()
        .find(|c| c.schema == schema && c.table == "orders" && c.name == "user_id")
        .expect("orders.user_id column");
    assert!(!col.nullable);
    assert!(
        info.columns
            .iter()
            .any(|c| c.table == "users" && c.name == "id" && c.primary_key)
    );
    let fk = info
        .foreign_keys
        .iter()
        .find(|f| f.schema == schema && f.table == "orders")
        .expect("orders foreign key");
    assert_eq!(fk.columns, ["user_id"]);
    assert_eq!(
        (fk.ref_table.as_str(), &fk.ref_columns[..]),
        ("users", &["id".to_string()][..])
    );
}

#[tokio::test]
async fn postgres() {
    let Some(cfg) = config("RUSDB_TEST_PG") else {
        eprintln!("RUSDB_TEST_PG not set, skipping");
        return;
    };
    common_checks(DriverKind::Postgres, cfg.clone(), "SELECT pg_sleep(30)").await;
    schema_checks(DriverKind::Postgres, cfg.clone(), "app").await;

    let mut h = Harness::connect(DriverKind::Postgres, cfg).await;
    let out = h
        .run(TabId(1), "SELECT g FROM generate_series(1, 25000) g")
        .await;
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
    schema_checks(DriverKind::Mysql, cfg.clone(), "app").await;

    let mut h = Harness::connect(DriverKind::Mysql, cfg).await;
    let out = h
        .run(
            TabId(1),
            "CREATE TEMPORARY TABLE t (id int); INSERT INTO t VALUES (1),(2); SELECT * FROM t",
        )
        .await;
    assert_eq!(out.error, None);
    assert_eq!(out.result.unwrap().row_count(), 2);

    // Binary data is shown as hex instead of broken UTF-8.
    let out = h.run(TabId(1), "SELECT UNHEX('DEADBEEF') AS b").await;
    assert_eq!(out.result.unwrap().cell(0, 0), Some("0xdeadbeef"));
}

/// Through the SSH bastion to Postgres: an unknown host key is reported (not
/// accepted), and after trusting it the connection works.
#[tokio::test]
async fn ssh_tunnel() {
    let Some(url) = std::env::var("RUSDB_TEST_SSH").ok() else {
        eprintln!("RUSDB_TEST_SSH not set, skipping");
        return;
    };
    let (creds, hostport) = url.split_once('@').expect("user:pw@host:port");
    let (user, password) = creds.split_once(':').expect("user:pw");
    let (host, port) = hostport.split_once(':').expect("host:port");
    let known_hosts =
        std::env::temp_dir().join(format!("rusdb-known-hosts-{}", std::process::id()));
    let _ = std::fs::remove_file(&known_hosts);
    // SAFETY: only this test reads the variable (in the tunnel code it starts).
    unsafe { std::env::set_var("RUSDB_KNOWN_HOSTS", &known_hosts) };

    let ssh = SshTarget {
        config: SshConfig {
            host: host.into(),
            port: port.parse().unwrap(),
            user: user.into(),
            auth: SshAuth::Password,
            key_path: String::new(),
        },
        secret: Some(password.into()),
    };
    let cfg = ConnectConfig {
        host: "postgres".into(),
        port: 5432,
        user: "postgres".into(),
        password: None,
        database: "postgres".into(),
        tls: TlsMode::Disable,
    };
    let (engine, events) = rusdb_engine::start();
    let connect = |ssh: &SshTarget| Command::Connect {
        conn: ConnId(1),
        driver: DriverKind::Postgres,
        cfg: cfg.clone(),
        ssh: Some(ssh.clone()),
    };
    let next = || async {
        tokio::time::timeout(Duration::from_secs(30), events.recv())
            .await
            .expect("timeout")
            .expect("engine gone")
    };

    engine.send(connect(&ssh));
    let (host, port, key) = loop {
        match next().await {
            Event::HostKeyUnknown {
                host,
                port,
                key,
                fingerprint,
                ..
            } => {
                assert!(fingerprint.starts_with("SHA256:"));
                break (host, port, key);
            }
            Event::Connected { .. } => panic!("unknown host key was accepted"),
            Event::ConnectFailed { error, .. } => panic!("connect failed: {error}"),
            _ => {}
        }
    };
    engine.send(Command::TrustHostKey { host, port, key });
    engine.send(connect(&ssh));
    loop {
        match next().await {
            Event::Connected { .. } => break,
            Event::ConnectFailed { error, .. } => panic!("connect failed: {error}"),
            Event::HostKeyUnknown { .. } => panic!("trusted key not found"),
            _ => {}
        }
    }
    engine.send(Command::Execute {
        query: QueryId(1),
        tab: TabId(1),
        conn: ConnId(1),
        sql: "SELECT 42".into(),
    });
    loop {
        if let Event::Query {
            event: QueryEvent::Finished { error, .. },
            ..
        } = next().await
        {
            assert_eq!(error, None);
            break;
        }
    }

    // Wrong password: a clear auth error, no hang.
    let bad = SshTarget {
        secret: Some("wrong".into()),
        ..ssh
    };
    engine.send(Command::Connect {
        conn: ConnId(2),
        driver: DriverKind::Postgres,
        cfg: cfg.clone(),
        ssh: Some(bad),
    });
    loop {
        match next().await {
            Event::ConnectFailed {
                conn: ConnId(2),
                error,
            } => {
                assert!(error.contains("authentication"), "{error}");
                break;
            }
            Event::Connected { conn: ConnId(2) } => panic!("wrong password accepted"),
            _ => {}
        }
    }
    let _ = std::fs::remove_file(&known_hosts);
}
