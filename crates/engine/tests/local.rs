//! The engine against a local SQLite file (no server needed): read-only, and the
//! statement time limit.

use std::time::Duration;

use tangul_core::{ConnId, ConnectConfig, DriverKind, QueryId, TabId, TlsMode};
use tangul_engine::{Command, Event, QueryEvent, QueryOptions};

async fn next(events: &async_channel::Receiver<Event>) -> Event {
    tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .expect("engine event in time")
        .expect("engine running")
}

/// Runs `sql` and returns its error, if any.
async fn run(
    engine: &tangul_engine::EngineHandle,
    events: &async_channel::Receiver<Event>,
    query: u64,
    sql: &str,
) -> Option<String> {
    engine.send(Command::Execute {
        query: QueryId(query),
        tab: TabId(1),
        conn: ConnId(1),
        sql: sql.into(),
        read_only: true,
        timeout: Some(Duration::from_millis(300)),
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

#[tokio::test(flavor = "multi_thread")]
async fn local_file_is_read_only_and_time_limited() {
    let dir = std::env::temp_dir().join(format!("tangul-engine-local-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("metrics.db");
    let _ = std::fs::remove_file(&path);
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("CREATE TABLE t (x INTEGER); INSERT INTO t VALUES (1);")
        .unwrap();

    let (engine, events) = tangul_engine::start();
    engine.send(Command::Connect {
        conn: ConnId(1),
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
    loop {
        match next(&events).await {
            Event::Connected { .. } => break,
            Event::ConnectFailed { error, .. } => panic!("{error}"),
            _ => {}
        }
    }

    assert_eq!(run(&engine, &events, 1, "SELECT x FROM t").await, None);
    assert!(run(&engine, &events, 2, "DELETE FROM t").await.is_some());

    // Never ends on its own.
    let forever = "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c)
                   SELECT count(*) FROM c";
    let error = run(&engine, &events, 3, forever).await.unwrap();
    assert!(error.contains("time limit"), "{error}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn query_returns_the_whole_result() {
    let dir = std::env::temp_dir().join(format!("tangul-engine-query-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("data.db");
    let _ = std::fs::remove_file(&path);
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("CREATE TABLE t (x INTEGER); INSERT INTO t VALUES (1), (2), (3);")
        .unwrap();

    let (engine, events) = tangul_engine::start();
    engine.send(Command::Connect {
        conn: ConnId(1),
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
    loop {
        if let Event::Connected { .. } = next(&events).await {
            break;
        }
    }
    let options = |row_limit| QueryOptions {
        read_only: true,
        timeout: Some(Duration::from_millis(300)),
        row_limit,
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

    let error = engine
        .query(QueryId(3), ConnId(1), "DELETE FROM t", options(10))
        .await
        .unwrap_err();
    assert!(error.message.contains("readonly"), "{error}");

    let forever = "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c)
                   SELECT count(*) FROM c";
    let error = engine
        .query(QueryId(4), ConnId(1), forever, options(10))
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
