//! The engine against a local SQLite file (no server needed): the gate (who may run
//! what), read-only files, the statement time limit, whole results and the audit log.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tangul_core::{ConnId, ConnectConfig, DriverKind, QueryId, TabId, TlsMode};
use tangul_data::local::Local;
use tangul_data::settings::Settings;
use tangul_engine::{
    Approval, Caller, Command, EngineConfig, EngineHandle, Event, Guard, QueryEvent, QueryOptions,
    SettingsSource,
};

async fn next(events: &async_channel::Receiver<Event>) -> Event {
    tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .expect("engine event in time")
        .expect("engine running")
}

/// A fresh directory with `data.db` (table `t` with 1, 2, 3); the engine connected
/// to it as `ConnId(1)`, a 300 ms time limit, and its audit log in the directory.
async fn setup(name: &str) -> (PathBuf, EngineHandle, async_channel::Receiver<Event>) {
    let dir = std::env::temp_dir().join(format!("tangul-engine-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("data.db");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("CREATE TABLE t (x INTEGER); INSERT INTO t VALUES (1), (2), (3);")
        .unwrap();
    let mut settings = Settings::default();
    settings.safety.max_query_time = Some(Duration::from_millis(300));
    let (engine, events) = tangul_engine::start_with(EngineConfig {
        settings: SettingsSource::Fixed(Box::new(settings)),
        audit_dir: Some(dir.clone()),
    });
    connect(&engine, &path, ConnId(1));
    loop {
        match next(&events).await {
            Event::Connected { .. } => break,
            Event::ConnectFailed { error, .. } => panic!("{error}"),
            _ => {}
        }
    }
    (dir, engine, events)
}

fn connect(engine: &EngineHandle, path: &Path, conn: ConnId) {
    engine.send(Command::Connect {
        conn,
        driver: DriverKind::Sqlite,
        cfg: ConnectConfig {
            host: String::new(),
            port: 0,
            user: String::new(),
            password: None,
            database: path.to_string_lossy().into_owned(),
            tls: TlsMode::Disable,
        },
        ssh: None,
    });
}

/// Runs `sql` in a tab as the user and returns its error, if any.
async fn run(
    engine: &EngineHandle,
    events: &async_channel::Receiver<Event>,
    query: u64,
    sql: &str,
    approval: Approval,
) -> Option<String> {
    engine.send(Command::Execute {
        query: QueryId(query),
        tab: TabId(1),
        conn: ConnId(1),
        sql: sql.into(),
        approval,
    });
    loop {
        if let Event::Query {
            query: q,
            event: QueryEvent::Finished { error, .. },
        } = next(events).await
            && q == QueryId(query)
        {
            return error;
        }
    }
}

const FOREVER: &str = "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c)
                       SELECT count(*) FROM c";

#[tokio::test(flavor = "multi_thread")]
async fn tabs_pass_the_gate() {
    let (dir, engine, events) = setup("tabs").await;
    assert_eq!(
        run(&engine, &events, 1, "SELECT x FROM t", Approval::None).await,
        None
    );
    // Without WHERE: refused until the dialog was confirmed.
    let refused = run(&engine, &events, 2, "DELETE FROM t", Approval::None).await;
    assert!(refused.unwrap().contains("WHERE"));
    // Confirmed, it reaches the file, which is read-only.
    let error = run(&engine, &events, 3, "DELETE FROM t", Approval::Confirmed).await;
    assert!(error.unwrap().contains("readonly"));
    let error = run(&engine, &events, 4, FOREVER, Approval::None)
        .await
        .unwrap();
    assert!(error.contains("time limit"), "{error}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn query_returns_the_whole_result() {
    let (dir, engine, _events) = setup("query").await;
    let options = |row_limit| QueryOptions {
        row_limit,
        ..QueryOptions::new(Caller::Schedule)
    };
    let out = engine
        .query(
            QueryId(1),
            ConnId(1),
            "SELECT x FROM t ORDER BY x; SELECT 42",
            options(10),
        )
        .await
        .unwrap();
    assert_eq!(out.results.len(), 2);
    assert_eq!(out.results[0].row_count(), 3);
    assert_eq!(out.results[1].cell(0, 0), Some("42"));
    assert!(!out.truncated);

    let out = engine
        .query(QueryId(2), ConnId(1), "SELECT x FROM t", options(2))
        .await
        .unwrap();
    assert!(out.truncated);
    assert_eq!(out.results[0].row_count(), 2);

    // Schedules only read.
    let error = engine
        .query(
            QueryId(3),
            ConnId(1),
            "DELETE FROM t WHERE x = 1",
            options(10),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("schedule"), "{error}");
    assert_eq!(error.held, None);

    let error = engine
        .query(QueryId(4), ConnId(1), FOREVER, options(10))
        .await
        .unwrap_err();
    assert!(error.message.contains("time limit"), "{error}");

    let error = engine
        .query(QueryId(5), ConnId(9), "SELECT 1", options(10))
        .await
        .unwrap_err();
    assert_eq!(error.message, "not connected");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn assistant_writes_wait_for_approval_and_are_audited() {
    let (dir, engine, _events) = setup("assistant").await;
    let assistant = QueryOptions::new(Caller::Assistant);
    let write = "DELETE FROM t WHERE x = 1";

    let out = engine
        .query(
            QueryId(1),
            ConnId(1),
            "SELECT count(*) FROM t",
            assistant.clone(),
        )
        .await
        .unwrap();
    assert_eq!(out.results[0].cell(0, 0), Some("3"));

    // Held, with the statements to show and a pass for exactly this one.
    let held = engine
        .query(QueryId(2), ConnId(1), write, assistant.clone())
        .await
        .unwrap_err()
        .held
        .expect("held for approval");
    assert!(matches!(&held.guard, Guard::Assistant { statements } if statements[0].0 == "DELETE"));
    let approved = QueryOptions {
        approval: Approval::Pass(held.pass),
        ..assistant.clone()
    };
    // Approved, it runs (and the read-only file refuses it); the pass is used up.
    let error = engine
        .query(QueryId(3), ConnId(1), write, approved.clone())
        .await
        .unwrap_err();
    assert!(error.message.contains("readonly"), "{error}");
    let again = engine
        .query(QueryId(4), ConnId(1), write, approved)
        .await
        .unwrap_err();
    assert!(again.held.is_some(), "a pass works once");

    let audit = Local::open(Some(&dir)).audit(10);
    let outcomes: Vec<(&str, &str)> = audit
        .iter()
        .rev()
        .map(|e| (e.caller.as_str(), e.outcome.as_str()))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("assistant", "ran"),
            ("assistant", "held"),
            ("assistant", "ran"),
            ("assistant", "held"),
        ]
    );
    assert_eq!(audit.last().unwrap().rows, Some(1));
    assert!(audit[1].error.as_deref().unwrap().contains("readonly"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_detached_handle_drops_commands_and_sends_no_events() {
    let (engine, events) = tangul_engine::detached();
    engine.send(Command::Cancel { query: QueryId(1) });
    assert!(events.is_closed());
    assert!(events.try_recv().is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_schema_comes_first_then_the_rest() {
    let dir = std::env::temp_dir().join(format!("tangul-engine-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("data.db");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("CREATE TABLE t (x INTEGER); CREATE INDEX t_x ON t (x);")
        .unwrap();
    let (engine, events) = tangul_engine::start_with(EngineConfig::isolated());
    connect(&engine, &path, ConnId(1));
    let mut schemas = Vec::new();
    while schemas.len() < 2 {
        if let Event::Schema {
            result, complete, ..
        } = next(&events).await
        {
            schemas.push((complete, result.unwrap()));
        }
    }
    // What the tree needs, without waiting for indexes and the like …
    assert!(!schemas[0].0);
    assert_eq!(schemas[0].1.tables.len(), 1);
    assert!(schemas[0].1.indexes.is_empty());
    // … then all of it.
    assert!(schemas[1].0);
    assert_eq!(schemas[1].1.indexes[0].name, "t_x");
    std::fs::remove_dir_all(&dir).unwrap();
}
