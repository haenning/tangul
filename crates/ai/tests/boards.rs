//! The board tools, called through the registry as a model would, over boards kept
//! in memory and changed through `service::boards` the way the app does.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tangul_ai::{BoardCall, BoardReply, BoardRequest, Ctx, Exposed, Registry, ToolError};
use tangul_assist::lexer::Lexing;
use tangul_core::board::AiCaller;
use tangul_core::{AiAccess, ConnId, DriverKind, ValueAccess};
use tangul_data::boards::Boards;
use tangul_service::boards;
use tangul_service::knowledge::Knowledge;

fn exposed(access: AiAccess) -> Exposed {
    Exposed {
        database: String::new(),
        columns: Default::default(),
        name: "shop".into(),
        user: "u1".into(),
        conn: Some(ConnId(1)),
        access,
        driver: DriverKind::Postgres,
        values: ValueAccess::None,
        bookmarks: Vec::new(),
    }
}

/// What the app does with a board call: database `db1` is user `u1`'s; queries
/// over `big` have a heavy plan.
fn answer(boards: &mut Boards, request: BoardRequest) -> Result<BoardReply, String> {
    let db = |user: &str| match user {
        "u1" => Ok("db1"),
        _ => Err("no such database".to_string()),
    };
    let queries = match &request {
        BoardRequest::Create { tiles, .. } => tiles.iter().map(|t| t.sql.clone()).collect(),
        BoardRequest::Edit { edits, .. } => boards::ai_queries(edits),
        BoardRequest::List { .. } => Vec::new(),
    };
    let held: HashMap<String, String> = queries
        .into_iter()
        .filter(|q| q.contains("big"))
        .map(|q| (q, "its plan estimates a cost of 2000000".to_string()))
        .collect();
    let ai = boards::ByAi {
        by: AiCaller::Mcp,
        lexing: Lexing::default(),
        held: &held,
    };
    match request {
        BoardRequest::List { user } => Ok(BoardReply::Boards(
            boards
                .for_database(db(&user)?)
                .into_iter()
                .filter(|b| !b.archived)
                .cloned()
                .collect(),
        )),
        BoardRequest::Create {
            user,
            name,
            description,
            tiles,
        } => boards::create_by_ai(boards, db(&user)?, &name, &description, tiles, &ai)
            .map(BoardReply::Board),
        BoardRequest::Edit {
            user,
            board,
            revision,
            edits,
        } => {
            let db = db(&user)?;
            if boards.get(&board).is_none_or(|b| b.database_id != db) {
                return Err("no such board".into());
            }
            boards::apply_by_ai(boards, &board, &revision, edits, &ai).map(BoardReply::Board)
        }
    }
}

fn ctx(access: AiAccess, store: Arc<Mutex<Boards>>) -> Ctx {
    Ctx::new(Knowledge::default(), None, vec![exposed(access)]).with_boards(Arc::new(
        move |call: BoardCall| {
            let reply = answer(&mut store.lock().unwrap(), call.request);
            let _ = call.reply.send(reply);
        },
    ))
}

async fn call(ctx: &Ctx, tool: &str, input: Value) -> Result<String, ToolError> {
    Registry::standard()
        .call(ctx, tool, input)
        .await
        .map(|out| out.text)
}

fn only(store: &Arc<Mutex<Boards>>) -> tangul_core::board::Board {
    store.lock().unwrap().for_database("db1")[0].clone()
}

#[tokio::test]
async fn an_ai_creates_reads_and_changes_a_board() {
    let store = Arc::new(Mutex::new(Boards::open(None)));
    let ctx = ctx(AiAccess::Data, store.clone());
    assert_eq!(
        call(&ctx, "list_boards", json!({})).await.unwrap(),
        "No boards on shop."
    );
    let created = call(
        &ctx,
        "create_board",
        json!({
            "name": "Revenue",
            "description": "Money in.",
            "tiles": [
                {"title": "Per month", "sql": "select month, sum(total) from orders group by 1",
                 "view": "bars", "width": "half"},
                {"title": "All", "description": "Every order ever.",
                 "sql": "select * from big"}
            ]
        }),
    )
    .await
    .unwrap();
    let board = only(&store);
    let (id, revision) = (board.id.clone(), board.revision.clone());
    assert_eq!(
        created,
        format!(
            "Created. Board \"Revenue\" on shop (id {id}, revision {revision}):\nMoney in.\n\n\
             Tile t1 \"Per month\" (bars, half width), by mcp\n\
             -- Set up by MCP\nselect month, sum(total) from orders group by 1\n\n\
             Tile t2 \"All\" (table, full width), by mcp, waits for the user to run it (its \
             plan estimates a cost of 2000000): Every order ever.\n\
             -- Set up by MCP\nselect * from big"
        )
    );
    assert_eq!(
        call(&ctx, "list_boards", json!({})).await.unwrap(),
        format!("Boards on shop:\n- Revenue (id {id}, revision {revision}, 2 tiles): Money in.")
    );
    let read = call(&ctx, "get_board", json!({"board": id})).await.unwrap();
    assert_eq!(read, created.trim_start_matches("Created. "));

    // An edit at the revision read; then that revision is stale.
    let edit = |revision: &str| {
        json!({
            "board": id,
            "revision": revision,
            "edits": [
                {"op": "set_query", "tile": "t2", "sql": "select count(*) from orders", "view": "line"},
                {"op": "move_tile", "tile": "t2", "to": 0}
            ]
        })
    };
    let changed = call(&ctx, "edit_board", edit(&revision)).await.unwrap();
    let now = only(&store);
    assert!(changed.starts_with(&format!(
        "Changed. Board \"Revenue\" on shop (id {id}, revision {})",
        now.revision
    )));
    assert_eq!(now.tiles[0].id, "t2");
    assert_eq!(
        now.tiles[0].sql,
        "-- Changed by MCP\nselect count(*) from orders"
    );
    assert_eq!(now.tiles[0].held(), None, "a new query, a new plan");
    let stale = call(&ctx, "edit_board", edit(&revision)).await.unwrap_err();
    assert_eq!(
        stale.to_string(),
        format!(
            "failed: the board changed since revision {revision} (now {}): nothing was \
             changed; read it again and redo the changes on what it is now",
            now.revision
        )
    );
}

#[tokio::test]
async fn an_ai_only_writes_tiles_that_read_and_leaves_the_rest_to_the_user() {
    let store = Arc::new(Mutex::new(Boards::open(None)));
    let ctx = ctx(AiAccess::Data, store.clone());
    let tiles = json!([{"title": "Gone", "sql": "delete from orders"}]);
    let error = call(&ctx, "create_board", json!({"name": "X", "tiles": tiles}))
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "failed: a tile only reads; this query is DELETE"
    );
    assert!(store.lock().unwrap().for_database("db1").is_empty());

    let tiles = json!([{"title": "Pie", "sql": "select 1", "view": "pie"}]);
    let error = call(&ctx, "create_board", json!({"name": "X", "tiles": tiles}))
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid input: view pie: table or one of line, area, stacked_area, bars, \
         stacked_bars (with :n for one column)"
    );

    call(&ctx, "create_board", json!({"name": "X", "tiles": []}))
        .await
        .unwrap();
    let board = only(&store);
    for op in [
        json!({"op": "remove_tile", "tile": "t1"}),
        json!({"op": "set_refresh", "every": 5}),
        json!({"op": "add_link", "url": "https://x.io"}),
    ] {
        let input = json!({"board": board.id, "revision": board.revision, "edits": [op]});
        let error = call(&ctx, "edit_board", input).await.unwrap_err();
        assert!(matches!(error, ToolError::BadInput(_)), "{error}");
    }
    let input = json!({"board": "elsewhere", "revision": board.revision, "edits": [{"op": "rename", "name": "Y"}]});
    assert_eq!(
        call(&ctx, "edit_board", input)
            .await
            .unwrap_err()
            .to_string(),
        "failed: no such board"
    );
}

#[tokio::test]
async fn schema_only_shows_tile_queries_as_fingerprints() {
    let store = Arc::new(Mutex::new(Boards::open(None)));
    let tiles = json!([{"title": "Ann", "sql": "select * from users where name = 'Ann' -- vip"}]);
    let ctx = ctx(AiAccess::Schema, store.clone());
    let created = call(
        &ctx,
        "create_board",
        json!({"name": "People", "tiles": tiles}),
    )
    .await
    .unwrap();
    assert!(
        created.ends_with("\nselect * from users where name = ?"),
        "{created}"
    );
    // Stored as written: the user's board runs it.
    assert_eq!(
        only(&store).tiles[0].sql,
        "-- Set up by MCP\nselect * from users where name = 'Ann' -- vip"
    );
}

#[tokio::test]
async fn without_boards_the_tools_say_so() {
    let ctx = Ctx::new(Knowledge::default(), None, vec![exposed(AiAccess::Data)]);
    let error = call(&ctx, "list_boards", json!({})).await.unwrap_err();
    assert_eq!(error.to_string(), "failed: boards aren't available here");
}
