//! Tools against a real database: a SQLite file with known contents, the real
//! engine and its gate, the schema loaded as the app loads it. Deterministic, no
//! server needed.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tangul_ai::{Ctx, Exposed, ProposedView, Registry, SharedBookmark, SqlProposal, ToolError};
use tangul_assist::dialect::dialect_for;
use tangul_assist::schema::SchemaIndex;
use tangul_core::chart::ChartStyle;
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
        bookmarks: vec![
            SharedBookmark {
                name: "Paid orders".into(),
                sql: "SELECT id FROM orders WHERE status = 'paid'".into(),
            },
            SharedBookmark {
                name: "Clean up".into(),
                sql: "DELETE FROM orders WHERE status = 'cancelled'".into(),
            },
        ],
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
                "this statement changes something and was not run; use propose_sql to open it \
                 for the user to review and run"
                    .into()
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
async fn a_cut_cell_is_read_whole_from_tangul_s_copy() {
    let (dir, ctx) = shop("cells").await;
    // A JSON document of 256 characters: more than the text shows.
    let sql = "SELECT 1 AS id, json_object('lots', json_array(json_object('name', 'A'), \
               json_object('name', substr(hex(zeroblob(120)), 1, 220)))) AS doc";
    let doc = format!(
        r#"{{"lots":[{{"name":"A"}},{{"name":"{}"}}]}}"#,
        "0".repeat(220)
    );
    let out = call(&ctx, "run_readonly", json!({"sql": sql}))
        .await
        .unwrap();
    assert_eq!(
        out,
        format!(
            "id\tdoc\n1\t{}…[{} chars; get_cell r1 row 1 col doc]\n(1 row)",
            doc.chars().take(200).collect::<String>(),
            doc.len()
        )
    );
    // The whole value, formatted; no query runs (the connection could be gone).
    let whole = call(
        &ctx,
        "get_cell",
        json!({"result": "r1", "row": 1, "column": "doc"}),
    )
    .await
    .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&doc).unwrap();
    assert_eq!(whole, serde_json::to_string_pretty(&parsed).unwrap());
    // A part of it; the column by number.
    assert_eq!(
        call(
            &ctx,
            "get_cell",
            json!({"result": "r1", "row": 1, "column": "2", "path": "lots.0.name"})
        )
        .await
        .unwrap(),
        "A"
    );
    // Short results aren't kept; wrong cells say what there is.
    call(&ctx, "run_readonly", json!({"sql": "SELECT 1 AS n"}))
        .await
        .unwrap();
    assert_eq!(
        call(
            &ctx,
            "get_cell",
            json!({"result": "r2", "row": 1, "column": "n"})
        )
        .await,
        Err(ToolError::NotFound(
            "result r2 (kept about 10 minutes); run the query again".into()
        ))
    );
    assert_eq!(
        call(
            &ctx,
            "get_cell",
            json!({"result": "r1", "row": 2, "column": "doc"})
        )
        .await,
        Err(ToolError::NotFound(
            "row 2, column doc in r1; it has rows 1–1 and columns id, doc".into()
        ))
    );
    assert_eq!(
        call(
            &ctx,
            "get_cell",
            json!({"result": "r1", "row": 1, "column": "id", "path": "a"})
        )
        .await,
        Err(ToolError::BadInput(
            "the cell isn't JSON; leave out path".into()
        ))
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn proposals_open_for_the_user_and_never_run() {
    let (dir, ctx) = shop("propose").await;
    let sql = "DELETE FROM orders WHERE id = 1";
    // Without an app to open them in (e.g. tests of the tools alone).
    assert_eq!(
        call(&ctx, "propose_sql", json!({"sql": sql})).await,
        Err(ToolError::NotAllowed(
            "there is no app to open proposals in here".into()
        ))
    );
    let proposed: Arc<Mutex<Vec<SqlProposal>>> = Arc::default();
    let ctx = ctx.with_proposals({
        let proposed = proposed.clone();
        Arc::new(move |p| proposed.lock().unwrap().push(p))
    });
    assert_eq!(
        call(
            &ctx,
            "propose_sql",
            json!({"sql": format!("  {sql}\n"), "note": "  drop the\ntest order "})
        )
        .await
        .unwrap(),
        "Opened in tangul in a new tab on shop for the user to review and run. It was not run."
    );
    assert_eq!(
        *proposed.lock().unwrap(),
        [SqlProposal {
            connection: "shop".into(),
            user: "u1".into(),
            sql: format!("-- Proposed by the assistant: drop the test order\n{sql}"),
            title: None,
            view: None,
        }]
    );
    // A query to look at as a chart: all columns, or one by name. Whether the
    // result has it is only known once the user runs it.
    let by_status = "SELECT status, count(*) AS n FROM orders GROUP BY status";
    assert_eq!(
        call(
            &ctx,
            "propose_sql",
            json!({"sql": by_status, "view": {"chart": "bars"}, "title": " Orders\nby status "})
        )
        .await
        .unwrap(),
        "Opened in tangul in a new tab on shop, set to show bars side by side once the user \
         runs it (tangul's default view if the result doesn't fit). It was not run."
    );
    assert_eq!(
        call(
            &ctx,
            "propose_sql",
            json!({"sql": by_status, "view": {"chart": "line", "column": "missing"}})
        )
        .await
        .unwrap(),
        "Opened in tangul in a new tab on shop, set to show lines of missing once the user \
         runs it (tangul's default view if the result doesn't fit). It was not run."
    );
    {
        let p = proposed.lock().unwrap();
        assert_eq!(p[1].title.as_deref(), Some("Orders by status"));
        assert_eq!(
            p[1].view,
            Some(ProposedView {
                style: ChartStyle::Bars,
                column: None
            })
        );
        assert_eq!(
            p[2].view,
            Some(ProposedView {
                style: ChartStyle::Line,
                column: Some("missing".into())
            })
        );
    }
    // Every style can be asked for.
    for style in ChartStyle::ALL {
        let view = json!({"chart": style.key(), "column": "n"});
        assert!(
            call(&ctx, "propose_sql", json!({"sql": by_status, "view": view}))
                .await
                .is_ok(),
            "{}",
            style.key()
        );
    }
    // A write with a view: refused, like a view of a bookmark that writes.
    assert_eq!(
        call(
            &ctx,
            "propose_sql",
            json!({"sql": sql, "view": {"chart": "line"}})
        )
        .await,
        Err(ToolError::BadInput(
            "a view shows a query that reads; leave out view for changes".into()
        ))
    );
    assert!(matches!(
        call(
            &ctx,
            "propose_sql",
            json!({"sql": "SELECT 1", "view": {"chart": "pie"}})
        )
        .await,
        Err(ToolError::BadInput(_))
    ));
    // The old `chart` field is gone.
    assert!(matches!(
        call(
            &ctx,
            "propose_sql",
            json!({"sql": "SELECT 1", "chart": "bars"})
        )
        .await,
        Err(ToolError::BadInput(_))
    ));
    // A bookmark by name (any case), titled after it.
    let before = proposed.lock().unwrap().len();
    call(
        &ctx,
        "propose_sql",
        json!({"bookmark": "paid ORDERS", "view": {"chart": "bars"}}),
    )
    .await
    .unwrap();
    {
        let p = proposed.lock().unwrap();
        let opened = &p[before];
        assert_eq!(opened.title.as_deref(), Some("Paid orders"));
        assert_eq!(
            opened.sql,
            "-- Proposed by the assistant\nSELECT id FROM orders WHERE status = 'paid'"
        );
    }
    assert_eq!(
        call(
            &ctx,
            "propose_sql",
            json!({"bookmark": "Clean up", "view": {"chart": "bars"}})
        )
        .await,
        Err(ToolError::BadInput(
            "a view shows a query that reads; leave out view for changes".into()
        ))
    );
    assert_eq!(
        call(&ctx, "propose_sql", json!({"bookmark": "Nope"})).await,
        Err(ToolError::NotFound(
            "bookmark Nope on shop; list_bookmarks lists them".into()
        ))
    );
    // One source at a time.
    assert_eq!(
        call(
            &ctx,
            "propose_sql",
            json!({"sql": "SELECT 1", "bookmark": "Paid orders"})
        )
        .await,
        Err(ToolError::BadInput(
            "give one of sql, bookmark or result_id".into()
        ))
    );
    assert_eq!(
        call(&ctx, "propose_sql", json!({"result_id": "r7"})).await,
        Err(ToolError::NotFound(
            "result r7 (results are kept for 10 minutes); give its sql instead".into()
        ))
    );
    assert_eq!(
        call(&ctx, "propose_sql", json!({"sql": " "})).await,
        Err(ToolError::BadInput("sql is empty".into()))
    );
    // Nothing ran.
    assert_eq!(
        call(
            &ctx,
            "run_readonly",
            json!({"sql": "SELECT count(*) AS n FROM orders"})
        )
        .await
        .unwrap(),
        "n\n4\n(1 row)"
    );
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

#[tokio::test(flavor = "multi_thread")]
async fn bookmarks_are_listed_with_their_sql() {
    let (dir, ctx) = shop("bookmarks").await;
    assert_eq!(
        call(&ctx, "list_bookmarks", json!({})).await.unwrap(),
        "Bookmarks on shop:\n\nPaid orders:\nSELECT id FROM orders WHERE status = 'paid'\n\n\
         Clean up:\nDELETE FROM orders WHERE status = 'cancelled'"
    );
    assert_eq!(
        call(&ctx, "list_bookmarks", json!({"connection": "other"})).await,
        Err(ToolError::NotFound(
            "connection other; available: shop".into()
        ))
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kept_result_opens_as_its_query() {
    let (dir, ctx) = shop("kept").await;
    let proposed: Arc<Mutex<Vec<SqlProposal>>> = Arc::default();
    let ctx = ctx.with_proposals({
        let proposed = proposed.clone();
        Arc::new(move |p| proposed.lock().unwrap().push(p))
    });
    // Kept because a cell was cut.
    let sql = "SELECT 1 AS id, hex(zeroblob(200)) AS doc";
    let out = call(&ctx, "run_readonly", json!({"sql": sql}))
        .await
        .unwrap();
    assert!(out.contains("get_cell r1"), "{out}");
    call(&ctx, "propose_sql", json!({"result_id": "r1"}))
        .await
        .unwrap();
    assert_eq!(
        proposed.lock().unwrap()[0].sql,
        format!("-- Proposed by the assistant\n{sql}")
    );
    assert_eq!(
        call(
            &ctx,
            "propose_sql",
            json!({"result_id": "r1", "connection": "elsewhere"})
        )
        .await,
        Err(ToolError::BadInput(
            "result r1 is from shop; leave out connection".into()
        ))
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
