//! Integration tests against real servers. Skipped unless the env vars are set, e.g.:
//!   TANGUL_TEST_PG=postgres:@127.0.0.1:55432/postgres
//!   TANGUL_TEST_MYSQL=root:@127.0.0.1:53306/app
//!   TANGUL_TEST_SSH=tangul:tangul@127.0.0.1:52222  (bastion that reaches postgres:5432)
//! (`make test-live` starts the databases from docker-compose.yml and sets all three.)

use std::time::Duration;

use tangul_core::{
    ConnId, ConnectConfig, DriverKind, QueryId, ResultSet, SshAuth, SshConfig, SshTarget, TabId,
    TlsMode,
};
use tangul_engine::{
    Approval, Caller, Command, EngineConfig, EngineHandle, Event, ExecEvent, QueryEvent,
    QueryOptions, SettingsSource,
};

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
    /// The engine's settings.json; read-only mode and the time limit are set there,
    /// as in the app.
    settings: std::path::PathBuf,
    /// The database user the harness logs in as.
    user: String,
}

struct Outcome {
    result: Option<ResultSet>,
    error: Option<String>,
    error_position: Option<usize>,
    in_transaction: bool,
    truncated: bool,
    cancelled: bool,
}

impl Harness {
    async fn connect(driver: DriverKind, cfg: ConnectConfig) -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let settings = std::env::temp_dir().join(format!(
            "tangul-live-settings-{}-{n}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&settings);
        let (engine, events) = tangul_engine::start_with(EngineConfig {
            settings: SettingsSource::File(Some(settings.clone())),
            audit_dir: None,
        });
        let user = cfg.user.clone();
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
            settings,
            user,
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

    /// Starts `sql` as the user in a tab, dialogs confirmed.
    fn start(&mut self, tab: TabId, sql: &str) -> QueryId {
        self.next_query += 1;
        let query = QueryId(self.next_query);
        self.engine.send(Command::Execute {
            query,
            tab,
            conn: self.conn,
            sql: sql.into(),
            approval: Approval::Confirmed,
        });
        query
    }

    /// Writes the engine's settings: read-only mode for the harness's user, and the
    /// time limit in seconds (none without).
    fn set(&self, read_only: bool, limit_secs: Option<u64>) {
        let users = if read_only {
            vec![self.user.clone()]
        } else {
            vec![]
        };
        let limit = limit_secs.map(|s| format!("{s}s"));
        let json = serde_json::json!({
            "safety": { "read_only_users": users, "max_query_time": limit }
        });
        std::fs::write(&self.settings, json.to_string()).unwrap();
        self.engine.send(Command::ReloadSettings);
    }

    /// Runs `sql` with the user in read-only mode (the server refuses writes).
    async fn run_read_only(&mut self, tab: TabId, sql: &str) -> Outcome {
        self.set(true, None);
        let query = self.start(tab, sql);
        let out = self.wait(query).await;
        self.set(false, None);
        out
    }

    /// Runs `sql` with a statement time limit in seconds.
    async fn run_limited(&mut self, tab: TabId, sql: &str, limit_secs: Option<u64>) -> Outcome {
        self.set(false, limit_secs);
        let query = self.start(tab, sql);
        self.wait(query).await
    }

    async fn wait(&mut self, query: QueryId) -> Outcome {
        let mut out = Outcome {
            result: None,
            error: None,
            error_position: None,
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
                        error_position,
                        in_transaction,
                        cancelled,
                        ..
                    } => {
                        out.error = error;
                        out.error_position = error_position;
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

    async fn stats(&mut self) -> tangul_engine::PoolStats {
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

    // Validation prepares without executing: syntax and unknown objects are
    // reported with positions, and nothing runs (the CREATE must not happen).
    h.engine.send(Command::Validate {
        request: 1,
        conn: h.conn,
        statements: vec![
            (0, "SELECT 1".into()),
            (0, "SELECT * FORM x".into()),
            (0, "SELECT * FROM tangul_no_such_table".into()),
            (0, "CREATE TABLE tangul_validate_probe (id INT)".into()),
        ],
    });
    let problems = loop {
        if let Event::Validated { problems, .. } = h.next().await {
            break problems;
        }
    };
    let at = |i| problems.iter().find(|p| p.statement == i);
    assert!(at(0).is_none() && at(3).is_none(), "{problems:?}");
    assert_eq!(at(1).and_then(|p| p.position), Some(9), "{problems:?}");
    assert_eq!(at(2).and_then(|p| p.position), Some(14), "{problems:?}");
    let out = h.run(tab_a, "SELECT * FROM tangul_validate_probe").await;
    assert!(
        out.error.is_some(),
        "validation must not execute statements"
    );

    // Syntax errors carry the byte position of the offending token.
    let out = h.run(tab_a, "SELECT * FORM x").await;
    assert!(out.error.is_some());
    assert_eq!(out.error_position, Some(9), "{:?}", out.error);

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

/// A statement over the time limit is stopped; without a limit the same session
/// runs long statements again.
async fn time_limit_checks(h: &mut Harness, sleep_sql: &str, short_sleep_sql: &str) {
    let tab = TabId(7);
    let started = std::time::Instant::now();
    let out = h.run_limited(tab, sleep_sql, Some(1)).await;
    assert!(out.error.is_some(), "a statement over the limit must fail");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    let out = h.run_limited(tab, short_sleep_sql, None).await;
    assert_eq!(out.error, None, "the limit is lifted again");
}

/// Creates `tangul_fk_test.parent` / `.child` (child.parent_id → parent.id) and checks
/// the columns and foreign key the schema loader reports. Self-contained: the CI
/// service databases have no seed data.
async fn schema_checks(driver: DriverKind, cfg: ConnectConfig) {
    // MySQL's schemas are databases; neither DROP SCHEMA variant runs on both.
    let drop = match driver {
        DriverKind::Postgres => "DROP SCHEMA IF EXISTS tangul_fk_test CASCADE",
        DriverKind::Mysql => "DROP SCHEMA IF EXISTS tangul_fk_test",
        DriverKind::Sqlite => unreachable!("no schemas to create in a read-only file"),
    };
    let mut h = Harness::connect(driver, cfg).await;
    let tab = TabId(1);
    let setup = format!(
        "{drop}; CREATE SCHEMA tangul_fk_test; \
         CREATE TABLE tangul_fk_test.parent (id INT PRIMARY KEY); \
         CREATE TABLE tangul_fk_test.child (id INT PRIMARY KEY, parent_id INT NOT NULL, \
           FOREIGN KEY (parent_id) REFERENCES tangul_fk_test.parent (id))"
    );
    assert_eq!(h.run(tab, &setup).await.error, None);

    h.engine.send(Command::RefreshSchema { conn: h.conn });
    let info = loop {
        if let Event::Schema { result, .. } = h.next().await {
            let info = result.expect("schema failed");
            // Skip a schema load that started before the tables existed.
            if info.tables.iter().any(|t| t.schema == "tangul_fk_test") {
                break info;
            }
        }
    };
    let col = |table: &str, name: &str| {
        info.columns
            .iter()
            .find(|c| c.schema == "tangul_fk_test" && c.table == table && c.name == name)
            .unwrap_or_else(|| panic!("column {table}.{name}"))
    };
    assert!(!col("child", "parent_id").nullable);
    assert!(!col("child", "parent_id").primary_key);
    assert!(col("parent", "id").primary_key);
    let fk = info
        .foreign_keys
        .iter()
        .find(|f| f.schema == "tangul_fk_test" && f.table == "child")
        .expect("child foreign key");
    assert_eq!(fk.columns, ["parent_id"]);
    assert_eq!(
        (
            fk.ref_schema.as_str(),
            fk.ref_table.as_str(),
            &fk.ref_columns[..]
        ),
        ("tangul_fk_test", "parent", &["id".to_string()][..])
    );

    assert_eq!(h.run(tab, drop).await.error, None);
}

/// Read-only mode is enforced by the server: writes fail, reads work, and a script
/// that switches the session back to read-write does not get around it.
async fn read_only_checks(h: &mut Harness, switch_to_read_write: &str) {
    let tab = TabId(9);
    h.run(tab, "DROP TABLE IF EXISTS tangul_ro_test").await;
    assert_eq!(
        h.run(tab, "CREATE TABLE tangul_ro_test (id INT)")
            .await
            .error,
        None
    );
    assert_eq!(
        h.run(tab, "INSERT INTO tangul_ro_test VALUES (1)")
            .await
            .error,
        None
    );

    let out = h.run_read_only(tab, "SELECT * FROM tangul_ro_test").await;
    assert_eq!(out.error, None);
    assert_eq!(out.result.unwrap().row_count(), 1);
    for write in [
        "INSERT INTO tangul_ro_test VALUES (2)",
        "UPDATE tangul_ro_test SET id = 5",
        "DELETE FROM tangul_ro_test",
        "DROP TABLE tangul_ro_test",
    ] {
        let out = h.run_read_only(tab, write).await;
        assert!(out.error.is_some(), "{write} must fail in read-only mode");
    }
    // Switching the session to read-write doesn't stick: the next statement is
    // read-only again.
    h.run_read_only(tab, switch_to_read_write).await;
    let out = h
        .run_read_only(tab, "INSERT INTO tangul_ro_test VALUES (3)")
        .await;
    assert!(out.error.is_some(), "read-only mode must be re-applied");

    // Nothing changed, and turning read-only off allows writes again.
    let out = h.run(tab, "SELECT * FROM tangul_ro_test").await;
    assert_eq!(out.result.unwrap().row_count(), 1);
    assert_eq!(
        h.run(tab, "INSERT INTO tangul_ro_test VALUES (4)")
            .await
            .error,
        None
    );

    // An open transaction that can write can't be made read-only.
    h.run(tab, "BEGIN").await;
    let out = h.run_read_only(tab, "SELECT 1").await;
    assert!(out.error.is_some_and(|e| e.contains("Read-only")));
    h.run(tab, "ROLLBACK").await;
    h.run(tab, "DROP TABLE tangul_ro_test").await;
}

/// The assistant through [`EngineHandle::query`]: reads run in a read-only
/// transaction that is rolled back, so a write hidden in a read (`sneak`, a
/// statement on the allow-list that writes) doesn't stay; an approved write runs on
/// a session of its own, which is closed afterwards.
async fn assistant_checks(h: &mut Harness, sneak: Option<(&str, &str)>) {
    let tab = TabId(11);
    h.set(false, None);
    h.run(tab, "DROP TABLE IF EXISTS tangul_ai_test").await;
    h.run(tab, "CREATE TABLE tangul_ai_test (id INT)").await;
    h.run(tab, "INSERT INTO tangul_ai_test VALUES (1)").await;
    let mut query = 1000;
    let mut ask = async |h: &mut Harness, sql: &str, approval: Approval| {
        query += 1;
        let options = QueryOptions {
            approval,
            ..QueryOptions::new(Caller::Assistant)
        };
        h.engine.query(QueryId(query), h.conn, sql, options).await
    };
    let count = "SELECT count(*) FROM tangul_ai_test";
    let out = ask(h, count, Approval::None).await.unwrap();
    assert_eq!(out.results[0].cell(0, 0), Some("1"));

    if let Some((create, call)) = sneak {
        assert_eq!(h.run(tab, create).await.error, None);
        let error = ask(h, call, Approval::None).await.unwrap_err();
        assert!(error.held.is_none(), "{call} passes the allow-list");
        let message = error.message.to_lowercase().replace('-', " ");
        assert!(message.contains("read only"), "{error}");
    }
    let before = h.stats().await.open;

    let write = "INSERT INTO tangul_ai_test VALUES (2)";
    let held = ask(h, write, Approval::None).await.unwrap_err();
    let pass = held.held.expect("held for approval").pass;
    let out = ask(h, write, Approval::Pass(pass)).await.unwrap();
    assert_eq!(out.rows, Some(1));
    // The write stayed; the session it ran on is gone again.
    assert_eq!(h.stats().await.open, before);
    let out = ask(h, count, Approval::None).await.unwrap();
    assert_eq!(out.results[0].cell(0, 0), Some("2"));
    h.run(tab, "DROP TABLE tangul_ai_test").await;
}

#[tokio::test]
async fn postgres() {
    let Some(cfg) = config("TANGUL_TEST_PG") else {
        eprintln!("TANGUL_TEST_PG not set, skipping");
        return;
    };
    common_checks(DriverKind::Postgres, cfg.clone(), "SELECT pg_sleep(30)").await;
    schema_checks(DriverKind::Postgres, cfg.clone()).await;

    let mut h = Harness::connect(DriverKind::Postgres, cfg).await;
    let out = h
        .run(TabId(1), "SELECT g FROM generate_series(1, 25000) g")
        .await;
    assert!(out.truncated);
    assert_eq!(out.result.unwrap().row_count(), tangul_engine::ROW_LIMIT);

    // A failed statement inside a transaction keeps the tab pinned (aborted txn).
    h.run(TabId(1), "BEGIN").await;
    assert!(h.run(TabId(1), "SELECT 1/0").await.in_transaction);
    assert!(!h.run(TabId(1), "ROLLBACK").await.in_transaction);

    read_only_checks(
        &mut h,
        "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE",
    )
    .await;
    time_limit_checks(&mut h, "SELECT pg_sleep(30)", "SELECT pg_sleep(2)").await;
    assistant_checks(
        &mut h,
        Some((
            "CREATE OR REPLACE FUNCTION tangul_ai_sneak() RETURNS int LANGUAGE sql \
             AS 'INSERT INTO tangul_ai_test VALUES (99) RETURNING id'",
            "SELECT tangul_ai_sneak()",
        )),
    )
    .await;
    h.run(TabId(1), "DROP FUNCTION tangul_ai_sneak()").await;
}

#[tokio::test]
async fn mysql() {
    let Some(cfg) = config("TANGUL_TEST_MYSQL") else {
        eprintln!("TANGUL_TEST_MYSQL not set, skipping");
        return;
    };
    common_checks(DriverKind::Mysql, cfg.clone(), "SELECT SLEEP(30)").await;
    schema_checks(DriverKind::Mysql, cfg.clone()).await;

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

    read_only_checks(&mut h, "SET SESSION TRANSACTION READ WRITE").await;
    time_limit_checks(&mut h, "SELECT SLEEP(30)", "SELECT SLEEP(2)").await;
    h.run(TabId(1), "DROP FUNCTION IF EXISTS tangul_ai_sneak")
        .await;
    assistant_checks(
        &mut h,
        Some((
            "CREATE FUNCTION tangul_ai_sneak() RETURNS INT DETERMINISTIC MODIFIES SQL DATA \
             BEGIN INSERT INTO tangul_ai_test VALUES (99); RETURN 1; END",
            "SELECT tangul_ai_sneak()",
        )),
    )
    .await;
    h.run(TabId(1), "DROP FUNCTION tangul_ai_sneak").await;
}

/// Through the SSH bastion to Postgres: an unknown host key is reported (not
/// accepted), and after trusting it the connection works.
#[tokio::test]
async fn ssh_tunnel() {
    let Some(url) = std::env::var("TANGUL_TEST_SSH").ok() else {
        eprintln!("TANGUL_TEST_SSH not set, skipping");
        return;
    };
    let (creds, hostport) = url.split_once('@').expect("user:pw@host:port");
    let (user, password) = creds.split_once(':').expect("user:pw");
    let (host, port) = hostport.split_once(':').expect("host:port");
    let known_hosts =
        std::env::temp_dir().join(format!("tangul-known-hosts-{}", std::process::id()));
    let _ = std::fs::remove_file(&known_hosts);
    // SAFETY: only this test reads the variable (in the tunnel code it starts).
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("TANGUL_KNOWN_HOSTS", &known_hosts)
    };

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
    let (engine, events) = tangul_engine::start_with(EngineConfig::isolated());
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
        approval: Approval::None,
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
