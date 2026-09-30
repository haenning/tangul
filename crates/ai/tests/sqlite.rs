//! Tools against a real database: a SQLite file with known contents, the real
//! engine and its gate, the schema loaded as the app loads it. Deterministic, no
//! server needed.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tangul_ai::chat::{ChatEvent, ChatSettings, answer};
use tangul_ai::llm::{self, Delta, Error, Message, Part, Provider, Reply, Request};
use tangul_ai::{
    ChartRequest, Ctx, Exposed, Registry, SharedBookmark, ShownResult, SqlProposal, ToolCall,
    ToolError,
};
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
            json!({"sql": format!("  {sql}\n"), "note": "  drop the\ntest order ",
                   "title": " Drop\n1 "})
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
            title: Some("Drop 1".into()),
        }]
    );
    // Only changes: reads are shown with run_readonly.
    assert_eq!(
        call(&ctx, "propose_sql", json!({"sql": "SELECT 1"})).await,
        Err(ToolError::BadInput(
            "this only reads; use run_readonly (with show to open it for the user)".into()
        ))
    );
    // What it showed before is gone.
    for gone in [
        json!({"sql": sql, "view": {"chart": "line"}}),
        json!({"bookmark": "Clean up"}),
        json!({"result_id": "r1"}),
    ] {
        assert!(
            matches!(
                call(&ctx, "propose_sql", gone.clone()).await,
                Err(ToolError::BadInput(_))
            ),
            "{gone}"
        );
    }
    assert_eq!(
        call(&ctx, "propose_sql", json!({"sql": " "})).await,
        Err(ToolError::BadInput("sql is empty".into()))
    );
    assert_eq!(proposed.lock().unwrap().len(), 1);
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

/// `ctx` with a sink that keeps what run_readonly shows.
fn showing(ctx: Ctx) -> (Ctx, Arc<Mutex<Vec<ShownResult>>>) {
    let shown: Arc<Mutex<Vec<ShownResult>>> = Arc::default();
    let ctx = ctx.with_shows({
        let shown = shown.clone();
        Arc::new(move |s| shown.lock().unwrap().push(s))
    });
    (ctx, shown)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_is_shown_to_the_user_whole() {
    let (dir, ctx) = shop("show").await;
    let by_status = "SELECT status, count(*) AS n FROM orders GROUP BY status ORDER BY status";
    // Without an app to show it in: refused before anything runs.
    assert_eq!(
        call(&ctx, "run_readonly", json!({"sql": by_status, "show": {}})).await,
        Err(ToolError::NotAllowed(
            "there is no app to show results in here".into()
        ))
    );
    let (ctx, shown) = showing(ctx);
    // The model reads one row; the user gets all three, as a chart.
    assert_eq!(
        call(
            &ctx,
            "run_readonly",
            json!({"sql": format!(" {by_status} "), "limit": 1,
                   "show": {"chart": "bars", "title": " Orders\nby status ", "note": "per status"}})
        )
        .await
        .unwrap(),
        "status\tn\ncancelled\t1\n(1 row, more not shown)\n\
         Opened in tangul in a new tab on shop, as a bars chart (the grid if the result \
         doesn't fit the chart)."
    );
    {
        let s = shown.lock().unwrap();
        let s = &s[0];
        assert_eq!((s.connection.as_str(), s.user.as_str()), ("shop", "u1"));
        assert_eq!(s.sql, by_status);
        assert_eq!(s.title.as_deref(), Some("Orders by status"));
        assert_eq!(s.note.as_deref(), Some("per status"));
        assert_eq!(
            s.chart,
            Some(ChartRequest {
                style: ChartStyle::Bars,
                column: None
            })
        );
        assert_eq!(s.result.row_count(), 3);
        assert_eq!(s.result.cell(2, 0), Some("paid"));
        assert!(!s.truncated);
    }
    // The grid, and one column of a chart.
    assert!(
        call(&ctx, "run_readonly", json!({"sql": by_status, "show": {}}))
            .await
            .unwrap()
            .ends_with(
                "Opened in tangul in a new tab on shop, in the grid (the grid if the \
                        result doesn't fit the chart)."
            )
    );
    assert!(
        call(
            &ctx,
            "run_readonly",
            json!({"sql": by_status, "show": {"chart": "line", "column": " n "}})
        )
        .await
        .unwrap()
        .contains("as a line chart of n")
    );
    {
        let s = shown.lock().unwrap();
        assert_eq!((s[1].chart.clone(), s[1].title.clone()), (None, None));
        assert_eq!(s[2].chart.as_ref().unwrap().column.as_deref(), Some("n"));
    }
    // Every style can be asked for; a column needs a chart.
    for style in ChartStyle::ALL {
        let show = json!({"chart": style.key()});
        assert!(
            call(
                &ctx,
                "run_readonly",
                json!({"sql": by_status, "show": show})
            )
            .await
            .is_ok(),
            "{}",
            style.key()
        );
    }
    let before = shown.lock().unwrap().len();
    assert_eq!(
        call(
            &ctx,
            "run_readonly",
            json!({"sql": by_status, "show": {"column": "n"}})
        )
        .await,
        Err(ToolError::BadInput("column needs a chart".into()))
    );
    assert!(matches!(
        call(
            &ctx,
            "run_readonly",
            json!({"sql": by_status, "show": {"chart": "pie"}})
        )
        .await,
        Err(ToolError::BadInput(_))
    ));
    // A write is never run, so nothing is shown.
    assert!(matches!(
        call(
            &ctx,
            "run_readonly",
            json!({"sql": "DELETE FROM orders", "show": {}})
        )
        .await,
        Err(ToolError::NotAllowed(_))
    ));
    assert_eq!(shown.lock().unwrap().len(), before);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bookmark_runs_by_name() {
    let (dir, ctx) = shop("bookmark-run").await;
    let calls: Arc<Mutex<Vec<ToolCall>>> = Arc::default();
    let ctx = ctx.with_audit("mcp", {
        let calls = calls.clone();
        Arc::new(move |c| calls.lock().unwrap().push(c))
    });
    let (ctx, shown) = showing(ctx);
    // Any case; shown under its name.
    assert_eq!(
        call(&ctx, "run_readonly", json!({"bookmark": " paid ORDERS "}))
            .await
            .unwrap(),
        "id\n2\n3\n(2 rows)"
    );
    call(
        &ctx,
        "run_readonly",
        json!({"bookmark": "Paid orders", "show": {}}),
    )
    .await
    .unwrap();
    {
        let s = shown.lock().unwrap();
        assert_eq!(s[0].title.as_deref(), Some("Paid orders"));
        assert_eq!(s[0].sql, "SELECT id FROM orders WHERE status = 'paid'");
    }
    // A bookmark that writes is never run.
    assert!(matches!(
        call(&ctx, "run_readonly", json!({"bookmark": "Clean up"})).await,
        Err(ToolError::NotAllowed(_))
    ));
    assert_eq!(
        call(&ctx, "run_readonly", json!({"bookmark": "Nope"})).await,
        Err(ToolError::NotFound(
            "bookmark Nope on shop; list_bookmarks lists them".into()
        ))
    );
    // One source at a time.
    for input in [
        json!({"sql": "SELECT 1", "bookmark": "Paid orders"}),
        json!({}),
    ] {
        assert_eq!(
            call(&ctx, "run_readonly", input).await,
            Err(ToolError::BadInput("give one of sql or bookmark".into()))
        );
    }
    // The history gets the bookmark's statement, also when it was refused.
    let ran: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|c| (c.sql.clone(), c.outcome))
        .collect();
    let paid = Some("SELECT id FROM orders WHERE status = 'paid'".to_string());
    assert_eq!(
        ran,
        [
            (paid.clone(), "ran"),
            (paid, "ran"),
            (
                Some("DELETE FROM orders WHERE status = 'cancelled'".into()),
                "refused"
            ),
            (None, "failed"),
            (None, "failed"),
            (None, "failed"),
        ]
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

// The assistant's conversation: a scripted model calls the tools as a provider's
// model would, through the same context and gate.

/// Answers with the given replies in turn and keeps the requests it got.
struct Script {
    replies: Mutex<Vec<Message>>,
    seen: Mutex<Vec<Request>>,
}

#[async_trait]
impl Provider for Script {
    async fn chat(&self, req: &Request, _: &mut (dyn FnMut(Delta) + Send)) -> Result<Reply, Error> {
        self.seen.lock().unwrap().push(req.clone());
        Ok(Reply {
            message: self.replies.lock().unwrap().remove(0),
            ..Reply::default()
        })
    }

    async fn models(&self) -> Result<Vec<String>, Error> {
        Ok(Vec::new())
    }
}

fn calls(calls: &[(&str, serde_json::Value)]) -> Message {
    Message::assistant(
        calls
            .iter()
            .enumerate()
            .map(|(i, (name, input))| {
                Part::ToolCall(llm::ToolCall {
                    id: format!("c{i}"),
                    name: name.to_string(),
                    input: input.clone(),
                    signature: None,
                })
            })
            .collect(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_asked_for_by_the_model_is_refused_and_nothing_changes() {
    let (dir, ctx) = shop("chat-write").await;
    let script = Script {
        replies: Mutex::new(vec![
            calls(&[
                (
                    "run_readonly",
                    json!({"sql": "SELECT count(*) AS n FROM orders"}),
                ),
                (
                    "run_readonly",
                    json!({"sql": "SELECT 1; DELETE FROM orders"}),
                ),
            ]),
            calls(&[(
                "run_readonly",
                json!({"sql": "SELECT count(*) AS n FROM orders"}),
            )]),
            Message::assistant(vec![Part::Text {
                text: "There are 4 orders.".into(),
            }]),
        ]),
        seen: Mutex::new(Vec::new()),
    };
    let settings = ChatSettings {
        model: "m".into(),
        max_rounds: 5,
        focus: Some("shop".into()),
    };
    let mut ran = Vec::new();
    answer(
        &script,
        &Registry::standard(),
        &ctx,
        &settings,
        &[Message::user("How many orders?")],
        &mut |e| {
            if let ChatEvent::ToolRan { ok, summary, .. } = e {
                ran.push((ok, summary));
            }
        },
    )
    .await
    .unwrap();
    assert_eq!(
        ran,
        [
            (true, "n".to_string()),
            (
                false,
                "not allowed: this statement changes something and was not run; use \
                 propose_sql to open it for the user to review and run"
                    .to_string()
            ),
            (true, "n".to_string()),
        ]
    );
    // The model saw the count, and the same count after the refused write.
    let seen = script.seen.lock().unwrap();
    let results = |i: usize| serde_json::to_string(&seen[i].messages).unwrap();
    assert!(
        results(1).contains(r#""content":"n\n4\n(1 row)""#),
        "{}",
        results(1)
    );
    assert!(results(2).ends_with(r#""content":"n\n4\n(1 row)","is_error":false}]}]"#));
    assert!(seen[0].system.contains("working on shop"));
    std::fs::remove_dir_all(&dir).unwrap();
}
