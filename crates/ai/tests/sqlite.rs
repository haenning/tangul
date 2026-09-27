//! Tools against a real database: a SQLite file with known contents, the real
//! engine and its gate, the schema loaded as the app loads it. Deterministic, no
//! server needed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tangul_ai::{Ctx, Exposed, Registry, ToolError};
use tangul_assist::dialect::dialect_for;
use tangul_assist::schema::SchemaIndex;
use tangul_core::{ConnId, ConnectConfig, DriverKind, TlsMode, ValueAccess};
use tangul_engine::{Command, EngineConfig, Event};
use tangul_service::knowledge::Knowledge;

const SHOP: &str = "
    CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT NOT NULL UNIQUE,
                        active INTEGER NOT NULL DEFAULT 1);
    CREATE TABLE orders (
        id INTEGER PRIMARY KEY,
        user_id INTEGER NOT NULL REFERENCES users (id),
        status TEXT NOT NULL DEFAULT 'open',
        total REAL NOT NULL CHECK (total >= 0)
    );
    CREATE INDEX orders_open ON orders (user_id) WHERE status = 'open';
    CREATE VIEW paid AS SELECT * FROM orders WHERE status = 'paid';
    CREATE TABLE accounts (id INTEGER PRIMARY KEY, password_hash TEXT);
    INSERT INTO users (email) VALUES ('ann@example.com'), ('bob@example.com');
    INSERT INTO orders (user_id, status, total) VALUES
        (1, 'open', 10), (1, 'paid', 25.5), (2, 'paid', 7), (2, 'cancelled', 0);
";

async fn next(events: &async_channel::Receiver<Event>) -> Event {
    tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .expect("engine event in time")
        .expect("engine running")
}

/// The shop in a fresh file, connected, its schema in the pool, exposed as `shop`.
async fn shop(name: &str) -> (PathBuf, Ctx) {
    shop_with(name, ValueAccess::None).await
}

/// [`shop`], where the assistant may read `values` from the rows.
async fn shop_with(name: &str, values: ValueAccess) -> (PathBuf, Ctx) {
    let dir = std::env::temp_dir().join(format!("tangul-ai-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("shop.db");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(SHOP)
        .unwrap();

    let (engine, events) = tangul_engine::start_with(EngineConfig::isolated());
    let conn = ConnId(1);
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
    // Connecting loads the schema, as for the app.
    let info = loop {
        match next(&events).await {
            Event::Schema {
                result,
                complete: true,
                ..
            } => break result.expect("schema"),
            Event::ConnectFailed { error, .. } => panic!("{error}"),
            _ => {}
        }
    };
    let knowledge = Knowledge::default();
    let dialect = dialect_for(DriverKind::Sqlite);
    let schema = SchemaIndex::new(info, dialect, dialect.default_schema(""));
    // The pool knows the database through the tab's connection; queries must run on
    // the exposed one (the AI user's). A connection that doesn't exist stands in for
    // the tab's, so a query on it would fail.
    knowledge.set_schema("u1", "db1", ConnId(99), Arc::new(schema));
    let exposed = Exposed {
        name: "shop".into(),
        user: "u1".into(),
        conn,
        driver: DriverKind::Sqlite,
        values,
    };
    (dir, Ctx::new(knowledge, Some(engine), vec![exposed]))
}

async fn call(ctx: &Ctx, tool: &str, input: serde_json::Value) -> Result<String, ToolError> {
    Registry::standard()
        .call(ctx, tool, input)
        .await
        .map(|o| o.text)
}

#[tokio::test(flavor = "multi_thread")]
async fn describes_what_the_file_has() {
    let (dir, ctx) = shop("describe").await;
    assert_eq!(
        call(&ctx, "describe_table", json!({"table": "orders"}))
            .await
            .unwrap(),
        "\
orders (table)
columns:
  id integer pk identity
  user_id integer not null → users.id
  status text not null default 'open'
  total real not null
indexes:
  orders_open (user_id) where status = 'open'
checks:
  total >= 0"
    );
    assert_eq!(
        call(&ctx, "describe_table", json!({"table": "users"}))
            .await
            .unwrap(),
        "\
users (table)
columns:
  id integer pk identity
  email text not null
  active integer not null default 1
indexes:
  sqlite_autoindex_users_1 (email) unique
referenced by:
  orders (user_id) → (id)"
    );
    assert_eq!(
        call(
            &ctx,
            "find_join_path",
            json!({"from": "users", "to": "orders"})
        )
        .await
        .unwrap(),
        "users.id = orders.user_id (foreign key orders_fk0)\nFROM users\nJOIN orders ON orders.user_id = users.id"
    );
    assert_eq!(
        call(
            &ctx,
            "schema_slice",
            json!({"question": "open orders of a user"})
        )
        .await
        .unwrap(),
        "\
orders
  id integer pk, user_id integer → users.id, status text, total real
  check total >= 0
  why: name; user_id: name

users
  id integer pk, email text, active integer
  why: name

paid (view)
  id integer, user_id integer, status text, total real
  why: user_id: name"
    );
    assert_eq!(
        call(&ctx, "view_definition", json!({"view": "paid"}))
            .await
            .unwrap(),
        "SELECT * FROM orders WHERE status = 'paid'"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_rows_through_the_gate() {
    let (dir, ctx) = shop("read").await;
    assert_eq!(
        call(
            &ctx,
            "run_readonly",
            json!({"sql": "SELECT u.email, count(*) AS orders, sum(o.total) AS total \
                           FROM orders o JOIN users u ON u.id = o.user_id \
                           GROUP BY u.email ORDER BY u.email"})
        )
        .await
        .unwrap(),
        "email\torders\ttotal\nann@example.com\t2\t35.5\nbob@example.com\t2\t7\n(2 rows)"
    );
    // The row cap, and NULLs spelled out.
    assert_eq!(
        call(
            &ctx,
            "run_readonly",
            json!({"sql": "SELECT id, NULL AS empty FROM orders ORDER BY id", "limit": 2})
        )
        .await
        .unwrap(),
        "id\tempty\n1\tNULL\n2\tNULL\n(2 rows, more not shown)"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_are_never_run() {
    let (dir, ctx) = shop("write").await;
    for sql in [
        "DELETE FROM orders",
        "UPDATE orders SET total = 0 WHERE id = 1",
        "DROP TABLE users",
        "SELECT 1; DELETE FROM orders",
    ] {
        assert_eq!(
            call(&ctx, "run_readonly", json!({"sql": sql})).await,
            Err(ToolError::NotAllowed(
                "this statement writes and was not run; propose it to the user instead".into()
            )),
            "{sql}"
        );
    }
    // Everything is still there.
    assert_eq!(
        call(
            &ctx,
            "run_readonly",
            json!({"sql": "SELECT count(*) AS n, sum(total) AS t FROM orders"})
        )
        .await
        .unwrap(),
        "n\tt\n4\t42.5\n(1 row)"
    );
    // Errors of the database come back as they are.
    let Err(ToolError::Failed(message)) = call(
        &ctx,
        "run_readonly",
        json!({"sql": "SELECT nope FROM orders"}),
    )
    .await
    else {
        panic!("a failure");
    };
    assert!(message.contains("nope"), "{message}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn profiles_a_column_from_a_sample() {
    let (dir, ctx) = shop_with("profile", ValueAccess::Values).await;
    assert_eq!(
        call(
            &ctx,
            "column_profile",
            json!({"table": "orders", "column": "status"})
        )
        .await
        .unwrap(),
        "orders.status: 4 rows · NULL 0 (0%) · 3 distinct · values: paid (2), cancelled (1), open (1)"
    );
    assert_eq!(
        call(
            &ctx,
            "column_profile",
            json!({"table": "orders", "column": "total"})
        )
        .await
        .unwrap(),
        "orders.total: 4 rows · NULL 0 (0%) · 4 distinct · range 0 – 25.5 · values: 0 (1), 7 (1), 10 (1), 25.5 (1)"
    );
    // A sampled value now finds its column, like an enum value.
    assert_eq!(
        call(&ctx, "search_schema", json!({"query": "cancelled"}))
            .await
            .unwrap(),
        "orders.status — value 'cancelled'"
    );
    // Secrets are never read, whatever the consent.
    assert_eq!(
        call(
            &ctx,
            "column_profile",
            json!({"table": "accounts", "column": "password_hash"})
        )
        .await,
        Err(ToolError::NotAllowed(
            "accounts.password_hash looks like a secret; its values are never read".into()
        ))
    );
    assert_eq!(
        call(
            &ctx,
            "column_profile",
            json!({"table": "orders", "column": "colour"})
        )
        .await,
        Err(ToolError::NotFound(
            "column colour; describe_table lists them".into()
        ))
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_profile_is_sampled_once() {
    let (dir, ctx) = shop_with("profile-cache", ValueAccess::Values).await;
    let first = call(
        &ctx,
        "column_profile",
        json!({"table": "users", "column": "email"}),
    )
    .await
    .unwrap();
    // Another user of the file adds a row; within the hour the pool's profile is used.
    rusqlite::Connection::open(dir.join("shop.db"))
        .unwrap()
        .execute("INSERT INTO users (email) VALUES ('cat@example.com')", [])
        .unwrap();
    assert_eq!(
        call(
            &ctx,
            "column_profile",
            json!({"table": "users", "column": "email"})
        )
        .await
        .unwrap(),
        first
    );
    assert!(first.starts_with("users.email: 2 rows"), "{first}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn statistics_only_read_no_values() {
    let (dir, ctx) = shop_with("profile-stats", ValueAccess::Statistics).await;
    // Counts, but no range and no values …
    assert_eq!(
        call(
            &ctx,
            "column_profile",
            json!({"table": "orders", "column": "total"})
        )
        .await
        .unwrap(),
        "orders.total: 4 rows · NULL 0 (0%) · 4 distinct"
    );
    assert_eq!(
        call(
            &ctx,
            "column_profile",
            json!({"table": "orders", "column": "status"})
        )
        .await
        .unwrap(),
        "orders.status: 4 rows · NULL 0 (0%) · 3 distinct"
    );
    // … so no search by values either.
    assert_eq!(
        call(&ctx, "search_schema", json!({"query": "cancelled"}))
            .await
            .unwrap(),
        "Nothing matches \"cancelled\"."
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
