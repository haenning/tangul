//! Boards: `list_boards`, `get_board`, `create_board` and `edit_board`. Boards are
//! tangul's own data, not the database's: the tools ask the app (`Ctx::boards`),
//! which changes them through `service::boards` as the AI's changes: all or none,
//! only at the revision the AI read, tile queries that read, and nothing the user
//! keeps for themselves (links, refresh, removing). Tiles the AI changed are marked,
//! and held until the user ran them where their plan looks heavy. Tile queries run
//! as the user's board, never for the AI: no result comes back. On a schema-only
//! database tile queries are shown as fingerprints, like bookmarks.

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tangul_assist::lexer::fingerprint;
use tangul_core::board::{AiCaller, Board, Tile, Width};
use tangul_core::chart::{ChartStyle, ChartView};
use tangul_service::boards::{self, Edit};

use super::one_line;
use crate::ctx::{BoardReply, BoardRequest, Ctx, Exposed};
use crate::tool::{Tool, ToolError, ToolOutput, input, schema_of};

pub(crate) struct ListBoards;
pub(crate) struct GetBoard;
pub(crate) struct CreateBoard;
pub(crate) struct EditBoard;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListInput {
    /// A name from list_connections; can be left out when there is only one.
    connection: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetInput {
    /// The board's id, from list_boards.
    board: String,
    /// A name from list_connections; can be left out when there is only one.
    connection: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateInput {
    name: String,
    /// What the board is for, in a sentence or two.
    description: Option<String>,
    tiles: Vec<TileInput>,
    /// A name from list_connections; can be left out when there is only one.
    connection: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EditInput {
    /// The board's id, from list_boards.
    board: String,
    /// The revision from get_board or list_boards: the board as you know it. If it
    /// changed since, nothing is changed; read it again.
    revision: String,
    /// Applied in order, all or none.
    edits: Vec<EditOp>,
    /// A name from list_connections; can be left out when there is only one.
    connection: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TileInput {
    title: String,
    /// What the tile shows, in a sentence; behind an (i) next to the title.
    description: Option<String>,
    /// One statement that reads (SELECT or WITH … SELECT), no `;`.
    sql: String,
    /// `table` (the default), or a chart style (see tangul_features), optionally
    /// with `:n` for the n-th number column only (from 0), e.g. `bars:1`.
    view: Option<String>,
    width: Option<WidthInput>,
}

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum WidthInput {
    Full,
    /// Half the board: two in a row.
    Half,
}

/// One change, by `op`.
#[derive(Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
enum EditOp {
    Rename {
        name: String,
    },
    Describe {
        text: String,
    },
    AddTile {
        tile: TileInput,
    },
    /// Title and description of a tile.
    LabelTile {
        tile: String,
        title: String,
        description: Option<String>,
    },
    SetQuery {
        tile: String,
        sql: String,
        view: Option<String>,
    },
    SetView {
        tile: String,
        view: Option<String>,
    },
    SetWidth {
        tile: String,
        width: WidthInput,
    },
    /// To position `to` (from 0).
    MoveTile {
        tile: String,
        to: usize,
    },
}

#[derive(Serialize)]
struct Listed {
    connection: String,
    boards: Vec<Summary>,
}

#[derive(Serialize)]
struct Summary {
    id: String,
    name: String,
    revision: String,
    description: String,
    tiles: usize,
}

#[derive(Serialize)]
struct Shown {
    connection: String,
    id: String,
    name: String,
    revision: String,
    description: String,
    tiles: Vec<ShownTile>,
}

#[derive(Serialize)]
struct ShownTile {
    id: String,
    title: String,
    description: String,
    sql: String,
    view: String,
    width: &'static str,
    /// Who of the AIs set it up, if one did.
    by_ai: Option<&'static str>,
    /// Why the board doesn't run it until the user did (its plan).
    held: Option<String>,
}

#[async_trait]
impl Tool for ListBoards {
    fn name(&self) -> &'static str {
        "list_boards"
    }

    fn description(&self) -> &'static str {
        "The user's boards for a database: several queries at a glance, each a tile \
         shown as a table or a chart. Id, name, revision and number of tiles; \
         get_board shows one."
    }

    fn input_schema(&self) -> Value {
        schema_of::<ListInput>()
    }

    async fn call(&self, ctx: &Ctx, value: Value) -> Result<ToolOutput, ToolError> {
        let ListInput { connection } = input(value)?;
        let exposed = ctx.exposed_one(connection.as_deref())?;
        let listed = Listed {
            connection: exposed.name.clone(),
            boards: list(ctx, exposed)
                .await?
                .iter()
                .map(|b| Summary {
                    id: b.id.clone(),
                    name: b.name.clone(),
                    revision: b.revision.clone(),
                    description: b.description.clone(),
                    tiles: b.tiles.len(),
                })
                .collect(),
        };
        let text = if listed.boards.is_empty() {
            format!("No boards on {}.", listed.connection)
        } else {
            let mut text = format!("Boards on {}:", listed.connection);
            for b in &listed.boards {
                text.push_str(&format!(
                    "\n- {} (id {}, revision {}, {} tiles)",
                    b.name, b.id, b.revision, b.tiles
                ));
                if let Some(line) = one_line(Some(b.description.clone())) {
                    text.push_str(&format!(": {line}"));
                }
            }
            text
        };
        Ok(ToolOutput::new(&listed, text))
    }
}

#[async_trait]
impl Tool for GetBoard {
    fn name(&self) -> &'static str {
        "get_board"
    }

    fn description(&self) -> &'static str {
        "A board with its revision and tiles: id, title, description, query, how it \
         is shown, and whether an AI set it up. Results aren't included: the user's \
         board runs the tiles, not you; run_readonly runs a query for you."
    }

    fn input_schema(&self) -> Value {
        schema_of::<GetInput>()
    }

    async fn call(&self, ctx: &Ctx, value: Value) -> Result<ToolOutput, ToolError> {
        let GetInput { board, connection } = input(value)?;
        let exposed = ctx.exposed_one(connection.as_deref())?;
        let found = list(ctx, exposed)
            .await?
            .into_iter()
            .find(|b| b.id == board)
            .ok_or_else(|| {
                ToolError::NotFound(format!(
                    "board {board} on {}; list_boards lists them",
                    exposed.name
                ))
            })?;
        Ok(shown(exposed, &found, None))
    }
}

#[async_trait]
impl Tool for CreateBoard {
    fn name(&self) -> &'static str {
        "create_board"
    }

    fn description(&self) -> &'static str {
        "Create a board for the user: a name, a description and tiles, each one query \
         that reads, shown as a table or a chart. Only when the user asked for a \
         board. The board runs the tiles as the user, read-only; you never see their \
         results. A query whose plan looks heavy waits until the user ran it."
    }

    fn input_schema(&self) -> Value {
        schema_of::<CreateInput>()
    }

    async fn call(&self, ctx: &Ctx, value: Value) -> Result<ToolOutput, ToolError> {
        let CreateInput {
            name,
            description,
            tiles,
            connection,
        } = input(value)?;
        let exposed = ctx.exposed_one(connection.as_deref())?;
        let tiles = tiles.into_iter().map(tile_of).collect::<Result<_, _>>()?;
        let request = BoardRequest::Create {
            user: exposed.user.clone(),
            name,
            description: description.unwrap_or_default(),
            tiles,
        };
        let board = board_of(ctx.boards(request).await?)?;
        Ok(shown(exposed, &board, Some("Created")))
    }
}

#[async_trait]
impl Tool for EditBoard {
    fn name(&self) -> &'static str {
        "edit_board"
    }

    fn description(&self) -> &'static str {
        "Change a board, only when the user asked for it: rename, describe, add tiles, \
         change a tile's title, query, view, width or position. All edits or none, and \
         only at the revision you read (get_board): if the board changed since, \
         nothing changes; read it again. Tile ids are from get_board. Removing tiles, \
         links, refresh and deleting boards stay with the user. A query whose plan \
         looks heavy waits until the user ran it."
    }

    fn input_schema(&self) -> Value {
        schema_of::<EditInput>()
    }

    async fn call(&self, ctx: &Ctx, value: Value) -> Result<ToolOutput, ToolError> {
        let EditInput {
            board,
            revision,
            edits,
            connection,
        } = input(value)?;
        let exposed = ctx.exposed_one(connection.as_deref())?;
        if edits.is_empty() {
            return Err(ToolError::BadInput("no edits".into()));
        }
        let edits = edits.into_iter().map(edit_of).collect::<Result<_, _>>()?;
        let request = BoardRequest::Edit {
            user: exposed.user.clone(),
            board,
            revision,
            edits,
        };
        let board = board_of(ctx.boards(request).await?)?;
        Ok(shown(exposed, &board, Some("Changed")))
    }
}

async fn list(ctx: &Ctx, exposed: &Exposed) -> Result<Vec<Board>, ToolError> {
    let request = BoardRequest::List {
        user: exposed.user.clone(),
    };
    match ctx.boards(request).await? {
        BoardReply::Boards(boards) => Ok(boards),
        BoardReply::Board(_) => Err(ToolError::Failed("unexpected answer".into())),
    }
}

fn board_of(reply: BoardReply) -> Result<Board, ToolError> {
    match reply {
        BoardReply::Board(board) => Ok(board),
        BoardReply::Boards(_) => Err(ToolError::Failed("unexpected answer".into())),
    }
}

/// `board` as the tools show it, after `done` (`Created`, `Changed`) if given.
fn shown(exposed: &Exposed, board: &Board, done: Option<&str>) -> ToolOutput {
    let schema_only = !exposed.access.is_data();
    let shown = Shown {
        connection: exposed.name.clone(),
        id: board.id.clone(),
        name: board.name.clone(),
        revision: board.revision.clone(),
        description: board.description.clone(),
        tiles: board
            .tiles
            .iter()
            .map(|t| ShownTile {
                id: t.id.clone(),
                title: t.title.clone(),
                description: t.description.clone(),
                sql: if schema_only {
                    fingerprint(&t.sql)
                } else {
                    t.sql.clone()
                },
                view: t.view.map_or_else(|| "table".into(), ChartView::to_key),
                width: match t.width {
                    Width::Full => "full",
                    Width::Half => "half",
                },
                by_ai: t.ai.as_ref().map(|mark| match mark.by {
                    AiCaller::Assistant => "assistant",
                    AiCaller::Mcp => "mcp",
                }),
                held: t.held().map(String::from),
            })
            .collect(),
    };
    let mut text = String::new();
    if let Some(done) = done {
        text.push_str(&format!("{done}. "));
    }
    text.push_str(&format!(
        "Board \"{}\" on {} (id {}, revision {})",
        shown.name, shown.connection, shown.id, shown.revision
    ));
    if !shown.description.is_empty() {
        text.push_str(&format!(":\n{}", shown.description));
    }
    if shown.tiles.is_empty() {
        text.push_str("\nNo tiles.");
    }
    for t in &shown.tiles {
        text.push_str(&format!(
            "\n\nTile {} \"{}\" ({}, {} width)",
            t.id, t.title, t.view, t.width
        ));
        if let Some(by) = t.by_ai {
            text.push_str(&format!(", by {by}"));
        }
        if let Some(held) = &t.held {
            text.push_str(&format!(", waits for the user to run it ({held})"));
        }
        if let Some(line) = one_line(Some(t.description.clone())) {
            text.push_str(&format!(": {line}"));
        }
        text.push_str(&format!("\n{}", t.sql.trim()));
    }
    ToolOutput::new(&shown, text)
}

fn tile_of(tile: TileInput) -> Result<Tile, ToolError> {
    let mut out = boards::tile(&tile.title, &tile.sql, view_of(tile.view)?);
    out.description = tile.description.unwrap_or_default().trim().into();
    out.width = tile.width.map_or(Width::Full, width_of);
    Ok(out)
}

fn edit_of(op: EditOp) -> Result<Edit, ToolError> {
    Ok(match op {
        EditOp::Rename { name } => Edit::Rename(name),
        EditOp::Describe { text } => Edit::Describe(text),
        EditOp::AddTile { tile } => Edit::AddTile(tile_of(tile)?),
        EditOp::LabelTile {
            tile,
            title,
            description,
        } => Edit::LabelTile {
            tile,
            title,
            description: description.unwrap_or_default(),
        },
        EditOp::SetQuery { tile, sql, view } => Edit::SetQuery {
            tile,
            sql,
            view: view_of(view)?,
        },
        EditOp::SetView { tile, view } => Edit::SetView {
            tile,
            view: view_of(view)?,
        },
        EditOp::SetWidth { tile, width } => Edit::SetWidth {
            tile,
            width: width_of(width),
        },
        EditOp::MoveTile { tile, to } => Edit::MoveTile { tile, to },
    })
}

fn width_of(width: WidthInput) -> Width {
    match width {
        WidthInput::Full => Width::Full,
        WidthInput::Half => Width::Half,
    }
}

/// `None`/`table`: the table; else a chart by its key.
fn view_of(view: Option<String>) -> Result<Option<ChartView>, ToolError> {
    match view.as_deref().map(str::trim) {
        None | Some("" | "table") => Ok(None),
        Some(key) => ChartView::from_key(key).map(Some).ok_or_else(|| {
            let styles: Vec<&str> = ChartStyle::ALL.iter().map(|s| s.key()).collect();
            ToolError::BadInput(format!(
                "view {key}: table or one of {} (with :n for one column)",
                styles.join(", ")
            ))
        }),
    }
}
