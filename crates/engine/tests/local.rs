//! The engine against a local SQLite file (no server needed): read-only, and the
//! statement time limit.

use std::time::Duration;

use rusdb_core::{ConnId, ConnectConfig, DriverKind, QueryId, TabId, TlsMode};
use rusdb_engine::{Command, Event, QueryEvent};

async fn next(events: &async_channel::Receiver<Event>) -> Event {
    tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .expect("engine event in time")
        .expect("engine running")
}

/// Runs `sql` and returns its error, if any.
async fn run(
    engine: &rusdb_engine::EngineHandle,
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
    let dir = std::env::temp_dir().join(format!("rusdb-engine-local-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("metrics.db");
    let _ = std::fs::remove_file(&path);
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("CREATE TABLE t (x INTEGER); INSERT INTO t VALUES (1);")
        .unwrap();

    let (engine, events) = rusdb_engine::start();
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
