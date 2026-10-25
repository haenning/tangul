//! Boards: the one way to change them, for the UI ([`create`], [`apply`] an
//! [`Edit`], [`copy_to`], [`delete`]) and for an AI ([`create_by_ai`],
//! [`apply_by_ai`]), over the stored boards (`tangul_data::boards`); and what a
//! refresh sends ([`Batch`]) and when ([`due`]). Every change gives the board a new
//! revision.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tangul_assist::guard;
use tangul_assist::lexer::Lexing;
use tangul_core::board::{
    AiCaller, AiMark, Board, Link, LinkTo, Refresh, Tile, Width, web_address,
};
use tangul_core::chart::ChartView;
use tangul_data::boards::Boards;
use tangul_data::profiles::{new_id, new_revision};

/// The most tiles an AI may leave on a board.
pub const AI_MAX_TILES: usize = 24;
/// The longest tile query an AI may write (characters).
pub const AI_MAX_SQL: usize = 10_000;

/// A change to a board.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    Rename(String),
    Describe(String),
    /// All links at once (the pills, in order).
    SetLinks(Vec<Link>),
    /// A link at the end ("Add to board" as a link).
    AddLink(Link),
    SetRefresh(Refresh),
    Archive(bool),
    /// Adds a tile at the end; it gets an id of its own.
    AddTile(Tile),
    /// Title and description of a tile.
    LabelTile {
        tile: String,
        title: String,
        description: String,
    },
    /// The SQL and how its result is shown ("Update tile" from a tab).
    SetQuery {
        tile: String,
        sql: String,
        view: Option<ChartView>,
    },
    SetView {
        tile: String,
        view: Option<ChartView>,
    },
    SetWidth {
        tile: String,
        width: Width,
    },
    /// Moves a tile to position `to` (dragged there); past the end means last.
    MoveTile {
        tile: String,
        to: usize,
    },
    RemoveTile(String),
    /// The user ran a held tile's query in a tab: the board runs it again.
    Release(String),
}

/// A new, empty board for `database_id`.
pub fn create(boards: &mut Boards, database_id: &str, name: &str) -> Result<Board, String> {
    let board = Board {
        id: new_id(),
        name: name_of(name)?,
        database_id: database_id.into(),
        ..Board::default()
    };
    store(boards, board, None, true)
}

/// Applies `edit` to board `id` and saves it; the board as it is now. Only a change
/// to what it shows gives it a new revision: releasing a held tile doesn't, and an
/// edit that changes nothing isn't saved. A query the user sets isn't held.
pub fn apply(boards: &mut Boards, id: &str, edit: Edit) -> Result<Board, String> {
    let old = boards.get(id).cloned().ok_or("no such board")?;
    let mut board = old.clone();
    let release = match &edit {
        Edit::SetQuery { tile, .. } => Some(tile.clone()),
        _ => None,
    };
    let content = !matches!(edit, Edit::Release(_));
    edit_board(&mut board, edit)?;
    if let Some(tile) = release {
        edit_board(&mut board, Edit::Release(tile))?;
    }
    store(boards, board, Some(&old), content)
}

/// How an AI's changes are made: by whom, by the database's lexical rules, and
/// which of its queries wait for the user and why ([`ai_queries`] as keys).
pub struct ByAi<'a> {
    pub by: AiCaller,
    pub lexing: Lexing,
    pub held: &'a HashMap<String, String>,
}

/// The queries `edits` add or change, trimmed: what is checked (its plan) before
/// [`apply_by_ai`] or [`create_by_ai`] decide which wait for the user.
pub fn ai_queries(edits: &[Edit]) -> Vec<String> {
    edits
        .iter()
        .filter_map(|e| match e {
            Edit::AddTile(tile) => Some(tile.sql.trim().to_string()),
            Edit::SetQuery { sql, .. } => Some(sql.trim().to_string()),
            _ => None,
        })
        .collect()
}

/// A new board by an AI for `database_id`, with `tiles` (see [`apply_by_ai`]).
pub fn create_by_ai(
    boards: &mut Boards,
    database_id: &str,
    name: &str,
    description: &str,
    tiles: Vec<Tile>,
    ai: &ByAi,
) -> Result<Board, String> {
    let mut board = Board {
        id: new_id(),
        name: name_of(name)?,
        description: description.trim().into(),
        database_id: database_id.into(),
        ..Board::default()
    };
    let edits = tiles.into_iter().map(Edit::AddTile).collect();
    edit_by_ai(&mut board, edits, ai)?;
    store(boards, board, None, true)
}

/// Applies an AI's `edits` to board `id`, all or none, and only at `revision`: the
/// one it read. A board that changed since is left as it is. Only names, texts,
/// queries, views, widths and order: links, refresh, removing tiles or the board stay
/// the user's. A tile's query must be one statement that reads ([`check_read`]); it
/// is saved under a comment saying who wrote it. The tiles it adds or changes carry
/// the AI's mark, held where `ai.held` says why.
pub fn apply_by_ai(
    boards: &mut Boards,
    id: &str,
    revision: &str,
    edits: Vec<Edit>,
    ai: &ByAi,
) -> Result<Board, String> {
    let old = boards.get(id).cloned().ok_or("no such board")?;
    if old.revision != revision {
        return Err(format!(
            "the board changed since revision {revision} (now {}): nothing was changed; \
             read it again and redo the changes on what it is now",
            old.revision
        ));
    }
    let mut board = old.clone();
    edit_by_ai(&mut board, edits, ai)?;
    store(boards, board, Some(&old), true)
}

fn edit_by_ai(board: &mut Board, edits: Vec<Edit>, ai: &ByAi) -> Result<(), String> {
    for edit in edits {
        // The tile it marks (`None` with `added`: the new one), and its new query.
        let (marked, query) = match &edit {
            Edit::AddTile(tile) => (None, Some(tile.sql.trim())),
            Edit::SetQuery { tile, sql, .. } => (Some(tile.clone()), Some(sql.trim())),
            Edit::LabelTile { tile, .. } | Edit::SetView { tile, .. } => (Some(tile.clone()), None),
            Edit::Rename(_) | Edit::Describe(_) | Edit::SetWidth { .. } | Edit::MoveTile { .. } => {
                (None, None)
            }
            Edit::SetLinks(_)
            | Edit::AddLink(_)
            | Edit::SetRefresh(_)
            | Edit::Archive(_)
            | Edit::RemoveTile(_)
            | Edit::Release(_) => return Err("only the user may do that".into()),
        };
        if let Some(sql) = query {
            check_read(sql, ai.lexing)?;
        }
        let new_query = query.is_some();
        let held = query.and_then(|sql| ai.held.get(sql).cloned());
        let added = matches!(edit, Edit::AddTile(_));
        let edit = match edit {
            Edit::AddTile(mut tile) => {
                tile.sql = annotated(&tile.sql, ai.by, "Set up");
                Edit::AddTile(tile)
            }
            Edit::SetQuery { tile, sql, view } => Edit::SetQuery {
                tile,
                sql: annotated(&sql, ai.by, "Changed"),
                view,
            },
            other => other,
        };
        edit_board(board, edit)?;
        let tile = match (added, marked) {
            (true, _) => board.tiles.last_mut(),
            (false, Some(id)) => board.tile_mut(&id),
            (false, None) => None,
        };
        if let Some(tile) = tile {
            // A new query is checked anew; a new title or view keeps the hold.
            let held = if new_query {
                held
            } else {
                tile.held().map(String::from)
            };
            tile.ai = Some(AiMark { by: ai.by, held });
        }
    }
    if board.tiles.len() > AI_MAX_TILES {
        return Err(format!("at most {AI_MAX_TILES} tiles on a board"));
    }
    Ok(())
}

/// `sql` under one comment line saying which AI wrote it (`-- Set up by MCP`), in
/// place of such a line it had.
fn annotated(sql: &str, by: AiCaller, done: &str) -> String {
    let who = match by {
        AiCaller::Assistant => "the assistant",
        AiCaller::Mcp => "MCP",
    };
    let rest: Vec<&str> = sql
        .trim()
        .lines()
        .skip_while(|line| {
            let line = line.trim();
            line.starts_with("-- Set up by ") || line.starts_with("-- Changed by ")
        })
        .collect();
    format!("-- {done} by {who}\n{}", rest.join("\n"))
}

/// That `sql` is one statement that reads, as the gate will check it on a board.
pub fn check_read(sql: &str, lexing: Lexing) -> Result<(), String> {
    if sql.chars().count() > AI_MAX_SQL {
        return Err(format!(
            "a tile's query has at most {AI_MAX_SQL} characters"
        ));
    }
    if let Some((keyword, _)) = guard::not_reads_with(sql, lexing).into_iter().next() {
        return Err(format!("a tile only reads; this query is {keyword}"));
    }
    match guard::not_one_read(&guard::read_text(sql, lexing)) {
        Some(reason) => Err(reason.into()),
        None => Ok(()),
    }
}

/// Saves `board` unless it is `old` as it was; with a new revision if `content`
/// (what the board shows) may have changed.
fn store(
    boards: &mut Boards,
    mut board: Board,
    old: Option<&Board>,
    content: bool,
) -> Result<Board, String> {
    if old == Some(&board) {
        return Ok(board);
    }
    if content {
        board.revision = new_revision();
    }
    boards.save(board.clone())?;
    Ok(board)
}

/// A copy of board `id` for another database (with the same schema: staging and
/// production), named `name`.
pub fn copy_to(
    boards: &mut Boards,
    id: &str,
    database_id: &str,
    name: &str,
) -> Result<Board, String> {
    let board = Board {
        id: new_id(),
        name: name_of(name)?,
        database_id: database_id.into(),
        archived: false,
        ..boards.get(id).cloned().ok_or("no such board")?
    };
    store(boards, board, None, true)
}

pub fn delete(boards: &mut Boards, id: &str) -> Result<(), String> {
    boards.delete(id)
}

/// A new board for `database_id` from a ready-made one (a performance board,
/// [`tangul_assist::perf`]): a copy of its tiles, named `name`, refreshed when
/// opened. Each tile only reads (checked like a tile an AI writes).
pub fn create_from(
    boards: &mut Boards,
    database_id: &str,
    name: &str,
    template: &tangul_assist::perf::Board,
    lexing: Lexing,
) -> Result<Board, String> {
    let tiles = template
        .tiles
        .iter()
        .map(|t| {
            check_read(&t.sql, lexing)?;
            Ok(Tile {
                id: new_id(),
                description: t.description.clone(),
                width: t.width,
                ..tile(&t.title, &t.sql, None)
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let board = Board {
        id: new_id(),
        name: name_of(name)?,
        description: template.description.clone(),
        database_id: database_id.into(),
        tiles,
        ..Board::default()
    };
    store(boards, board, None, true)
}

/// A tile for a query from a tab or a bookmark: a copy of its SQL, full width.
pub fn tile(title: &str, sql: &str, view: Option<ChartView>) -> Tile {
    Tile {
        title: title.trim().into(),
        sql: sql.trim().into(),
        view,
        ..Tile::default()
    }
}

fn edit_board(board: &mut Board, edit: Edit) -> Result<(), String> {
    match edit {
        Edit::Rename(name) => board.name = name_of(&name)?,
        Edit::Describe(text) => board.description = text.trim().into(),
        Edit::SetLinks(links) => {
            board.links = links.into_iter().map(link_of).collect::<Result<_, _>>()?;
        }
        Edit::AddLink(link) => board.links.push(link_of(link)?),
        Edit::SetRefresh(refresh) => {
            board.refresh = match refresh {
                Refresh::Every(secs) => Refresh::Every(secs.max(Refresh::MIN_SECONDS)),
                other => other,
            }
        }
        Edit::Archive(archived) => board.archived = archived,
        Edit::AddTile(mut tile) => {
            if tile.sql.trim().is_empty() {
                return Err("a tile needs a query".into());
            }
            tile.id = free_tile_id(board);
            board.tiles.push(tile);
        }
        Edit::LabelTile {
            tile,
            title,
            description,
        } => {
            let tile = tile_mut(board, &tile)?;
            tile.title = title.trim().into();
            tile.description = description.trim().into();
        }
        Edit::SetQuery { tile, sql, view } => {
            if sql.trim().is_empty() {
                return Err("a tile needs a query".into());
            }
            let tile = tile_mut(board, &tile)?;
            tile.sql = sql.trim().into();
            tile.view = view;
        }
        Edit::SetView { tile, view } => tile_mut(board, &tile)?.view = view,
        Edit::SetWidth { tile, width } => tile_mut(board, &tile)?.width = width,
        Edit::MoveTile { tile, to } => {
            let from = position(board, &tile)?;
            let moved = board.tiles.remove(from);
            board.tiles.insert(to.min(board.tiles.len()), moved);
        }
        Edit::RemoveTile(tile) => {
            let at = position(board, &tile)?;
            board.tiles.remove(at);
        }
        Edit::Release(tile) => {
            if let Some(mark) = &mut tile_mut(board, &tile)?.ai {
                mark.held = None;
            }
        }
    }
    Ok(())
}

/// `link` trimmed, if it has a label and somewhere to go.
fn link_of(link: Link) -> Result<Link, String> {
    let label = link.label.trim().to_string();
    if label.is_empty() {
        return Err("a link needs a label".into());
    }
    let to = match link.to {
        // `taend.eu` opens as `https://taend.eu`.
        LinkTo::Url(url) => LinkTo::Url(web_address(&url).unwrap_or(url)),
        LinkTo::Query { sql, view } => LinkTo::Query {
            sql: sql.trim().into(),
            view,
        },
        other => other,
    };
    let link = Link { label, to };
    if !link.is_valid() {
        return Err(match &link.to {
            LinkTo::Url(url) => format!("not a web address: {url}"),
            LinkTo::Query { .. } => format!("{}: a query link needs a query", link.label),
            LinkTo::Bookmark(_) | LinkTo::Board(_) => format!("{}: goes nowhere", link.label),
        });
    }
    Ok(link)
}

fn name_of(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a board needs a name".into());
    }
    Ok(name.into())
}

fn position(board: &Board, tile: &str) -> Result<usize, String> {
    board
        .tiles
        .iter()
        .position(|t| t.id == tile)
        .ok_or_else(|| "no such tile".into())
}

fn tile_mut<'a>(board: &'a mut Board, tile: &str) -> Result<&'a mut Tile, String> {
    board.tile_mut(tile).ok_or_else(|| "no such tile".into())
}

/// `t1`, `t2`, …: the first one the board doesn't use.
fn free_tile_id(board: &Board) -> String {
    (1..)
        .map(|n| format!("t{n}"))
        .find(|id| board.tile(id).is_none())
        .expect("a free tile id")
}

/// What one refresh sends: each different statement once, and which of them each
/// tile that runs shows (held tiles wait for the user).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub statements: Vec<String>,
    /// The tiles that run, in the board's order: id and index in `statements`.
    pub tiles: Vec<(String, usize)>,
}

impl Batch {
    pub fn of(board: &Board) -> Self {
        let statements: Vec<String> = board.statements().into_iter().map(String::from).collect();
        let tiles = board
            .tiles
            .iter()
            .filter(|t| t.held().is_none())
            .filter_map(|t| Some((t.id.clone(), statements.iter().position(|s| *s == t.sql)?)))
            .collect();
        Self { statements, tiles }
    }
}

/// Whether a shown board refreshes now: never while a refresh still runs, on the
/// interval since the last one started, and on opening unless it is manual (no
/// refresh yet: `last` is `None`).
pub fn due(refresh: Refresh, last: Option<Instant>, running: bool, now: Instant) -> bool {
    if running {
        return false;
    }
    match (last, refresh.interval()) {
        (None, _) => refresh.on_open(),
        (Some(last), Some(interval)) => now.saturating_duration_since(last) >= interval,
        (Some(_), None) => false,
    }
}

/// How long until [`due`] may say yes, for a timer; `None` without an interval.
pub fn next_in(refresh: Refresh, last: Option<Instant>, now: Instant) -> Option<Duration> {
    let interval = refresh.interval()?;
    let last = last?;
    Some(interval.saturating_sub(now.saturating_duration_since(last)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(board: &Board) -> Vec<&str> {
        board.tiles.iter().map(|t| t.id.as_str()).collect()
    }

    fn with_tiles(boards: &mut Boards, n: usize) -> Board {
        let board = create(boards, "db", " Signups ").unwrap();
        for i in 0..n {
            let t = tile(&format!("T{i}"), &format!("select {i}"), None);
            apply(boards, &board.id, Edit::AddTile(t)).unwrap();
        }
        boards.get(&board.id).unwrap().clone()
    }

    #[test]
    fn a_board_is_created_named_and_described() {
        let mut boards = Boards::open(None);
        let board = with_tiles(&mut boards, 0);
        assert_eq!(board.name, "Signups");
        assert_eq!(board.refresh, Refresh::OnOpen);
        assert_eq!(boards.for_database("db").len(), 1);
        assert!(create(&mut boards, "db", "  ").is_err());

        let id = board.id.as_str();
        let b = apply(&mut boards, id, Edit::Rename("Growth".into())).unwrap();
        assert_eq!(b.name, "Growth");
        assert!(apply(&mut boards, id, Edit::Rename(" ".into())).is_err());
        let b = apply(
            &mut boards,
            id,
            Edit::Describe(" After launch.\nMore. ".into()),
        )
        .unwrap();
        assert_eq!(b.description, "After launch.\nMore.");
        let b = apply(&mut boards, id, Edit::SetRefresh(Refresh::Every(1))).unwrap();
        assert_eq!(b.refresh, Refresh::Every(5));
        let b = apply(&mut boards, id, Edit::SetRefresh(Refresh::Manual)).unwrap();
        assert_eq!(b.refresh, Refresh::Manual);
        let b = apply(&mut boards, id, Edit::Archive(true)).unwrap();
        assert!(b.archived);
        assert_eq!(
            apply(&mut boards, "nope", Edit::Archive(true)).unwrap_err(),
            "no such board"
        );
        delete(&mut boards, id).unwrap();
        assert!(boards.get(id).is_none());
    }

    #[test]
    fn a_performance_board_is_a_copy_of_its_template() {
        use tangul_assist::dialect::dialect_for;
        use tangul_assist::perf::{Facts, Kind, Stats};
        let mut boards = Boards::open(None);
        let pg = dialect_for(tangul_core::DriverKind::Postgres);
        let facts = Facts {
            version: (16, 0),
            mariadb: false,
            statements: Stats::Yes,
        };
        let template = pg
            .perf_boards(&facts)
            .into_iter()
            .find(|o| o.board.kind == Kind::Indexes)
            .unwrap()
            .board;
        let board = create_from(&mut boards, "db", "Indexes", &template, pg.lexing()).unwrap();
        assert_eq!(board.name, "Indexes");
        assert_eq!(board.description, template.description);
        assert_eq!(board.refresh, Refresh::OnOpen);
        let copied: Vec<(&str, &str, Width)> = (board.tiles.iter())
            .map(|t| (t.title.as_str(), t.sql.as_str(), t.width))
            .collect();
        let wanted: Vec<(&str, &str, Width)> = (template.tiles.iter())
            .map(|t| (t.title.as_str(), t.sql.as_str(), t.width))
            .collect();
        assert_eq!(copied, wanted);
        assert!(
            board
                .tiles
                .iter()
                .all(|t| !t.id.is_empty() && t.ai.is_none())
        );
        assert_eq!(boards.get(&board.id), Some(&board));
        // A template's tiles only read.
        let mut writes = template.clone();
        writes.tiles[0].sql = "DELETE FROM t".into();
        let e = create_from(&mut boards, "db", "x", &writes, pg.lexing()).unwrap_err();
        assert!(e.contains("only reads"), "{e}");
    }

    #[test]
    fn links_need_a_label_and_somewhere_to_go() {
        let mut boards = Boards::open(None);
        let id = with_tiles(&mut boards, 0).id;
        let link = |label: &str, to: LinkTo| Link {
            label: label.into(),
            to,
        };
        let query = |sql: &str| LinkTo::Query {
            sql: sql.into(),
            view: ChartView::from_key("bars"),
        };
        let b = apply(
            &mut boards,
            &id,
            Edit::SetLinks(vec![
                link(" Epic ", LinkTo::Url(" https://x.io ".into())),
                link("Orders", LinkTo::Bookmark("b1".into())),
            ]),
        )
        .unwrap();
        assert_eq!(
            b.links,
            [
                link("Epic", LinkTo::Url("https://x.io".into())),
                link("Orders", LinkTo::Bookmark("b1".into())),
            ]
        );
        let b = apply(
            &mut boards,
            &id,
            Edit::AddLink(link("By plan", query(" select 1 "))),
        )
        .unwrap();
        assert_eq!(b.links[2], link("By plan", query("select 1")));
        let b = apply(
            &mut boards,
            &id,
            Edit::AddLink(link("Site", LinkTo::Url(" taend.eu ".into()))),
        )
        .unwrap();
        assert_eq!(
            b.links[3],
            link("Site", LinkTo::Url("https://taend.eu".into()))
        );

        for (bad, why) in [
            (
                link("Epic", LinkTo::Url("file:///etc".into())),
                "file:///etc",
            ),
            (link(" ", LinkTo::Board("b2".into())), "label"),
            (link("Empty", query(" ")), "needs a query"),
            (link("Gone", LinkTo::Bookmark("".into())), "nowhere"),
        ] {
            let error = apply(&mut boards, &id, Edit::AddLink(bad)).unwrap_err();
            assert!(error.contains(why), "{error}");
        }
        assert_eq!(boards.get(&id).unwrap().links.len(), 4, "unchanged");
    }

    #[test]
    fn tiles_are_added_labelled_resized_moved_and_removed() {
        let mut boards = Boards::open(None);
        let board = with_tiles(&mut boards, 3);
        let id = board.id.as_str();
        assert_eq!(ids(&board), ["t1", "t2", "t3"]);
        assert!(apply(&mut boards, id, Edit::AddTile(tile("x", " ", None))).is_err());

        let edit = |tile: &str| Edit::LabelTile {
            tile: tile.into(),
            title: " Per day ".into(),
            description: "Signups".into(),
        };
        let b = apply(&mut boards, id, edit("t2")).unwrap();
        assert_eq!(
            (b.tiles[1].title.as_str(), b.tiles[1].description.as_str()),
            ("Per day", "Signups")
        );
        assert_eq!(
            apply(&mut boards, id, edit("t9")).unwrap_err(),
            "no such tile"
        );

        let width = |tile: &str, width| Edit::SetWidth {
            tile: tile.into(),
            width,
        };
        let b = apply(&mut boards, id, width("t1", Width::Half)).unwrap();
        assert_eq!(b.tiles[0].width, Width::Half);

        let view = Some(ChartView::from_key("bars").unwrap());
        let b = apply(
            &mut boards,
            id,
            Edit::SetView {
                tile: "t3".into(),
                view,
            },
        )
        .unwrap();
        assert_eq!(b.tiles[2].view, view);
        let b = apply(
            &mut boards,
            id,
            Edit::SetQuery {
                tile: "t3".into(),
                sql: " select 42 ".into(),
                view: None,
            },
        )
        .unwrap();
        assert_eq!(
            (b.tiles[2].sql.as_str(), b.tiles[2].view),
            ("select 42", None)
        );
        assert!(
            apply(
                &mut boards,
                id,
                Edit::SetQuery {
                    tile: "t3".into(),
                    sql: String::new(),
                    view: None
                }
            )
            .is_err()
        );

        let to = |tile: &str, to| Edit::MoveTile {
            tile: tile.into(),
            to,
        };
        assert_eq!(
            ids(&apply(&mut boards, id, to("t3", 0)).unwrap()),
            ["t3", "t1", "t2"]
        );
        assert_eq!(
            ids(&apply(&mut boards, id, to("t3", 99)).unwrap()),
            ["t1", "t2", "t3"]
        );
        assert_eq!(
            ids(&apply(&mut boards, id, to("t1", 1)).unwrap()),
            ["t2", "t1", "t3"]
        );
        assert!(apply(&mut boards, id, to("t9", 0)).is_err());

        let b = apply(&mut boards, id, Edit::RemoveTile("t1".into())).unwrap();
        assert_eq!(ids(&b), ["t2", "t3"]);
        assert!(apply(&mut boards, id, Edit::RemoveTile("t1".into())).is_err());
        // A new tile takes the first free id.
        let b = apply(&mut boards, id, Edit::AddTile(tile("n", "select 9", None))).unwrap();
        assert_eq!(ids(&b), ["t2", "t3", "t1"]);
    }

    #[test]
    fn a_board_is_copied_to_another_database() {
        let mut boards = Boards::open(None);
        let board = with_tiles(&mut boards, 2);
        apply(&mut boards, &board.id, Edit::Archive(true)).unwrap();
        let copy = copy_to(&mut boards, &board.id, "prod", "Signups (prod)").unwrap();
        assert_ne!(copy.id, board.id);
        assert_eq!(copy.database_id, "prod");
        assert!(!copy.archived);
        assert_eq!(copy.tiles, board.tiles);
        assert_eq!(boards.for_database("prod").len(), 1);
        assert_eq!(boards.for_database("db").len(), 1);
        assert!(copy_to(&mut boards, "nope", "prod", "x").is_err());
    }

    #[test]
    fn a_refresh_sends_each_statement_once() {
        let mut board = Board::default();
        for (id, sql) in [("a", "select 1"), ("b", "select 2"), ("c", "select 1")] {
            board.tiles.push(Tile {
                id: id.into(),
                sql: sql.into(),
                ..Tile::default()
            });
        }
        assert_eq!(
            Batch::of(&board),
            Batch {
                statements: vec!["select 1".into(), "select 2".into()],
                tiles: vec![("a".into(), 0), ("b".into(), 1), ("c".into(), 0)],
            }
        );
        board.tiles[1].ai = Some(AiMark {
            by: AiCaller::Mcp,
            held: Some("heavy".into()),
        });
        let batch = Batch::of(&board);
        assert_eq!(batch.statements, ["select 1"]);
        assert_eq!(batch.tiles, [("a".into(), 0), ("c".into(), 0)]);
        assert_eq!(Batch::of(&Board::default()).statements.len(), 0);
    }

    #[test]
    fn refreshes_are_due_on_open_and_on_the_interval() {
        let now = Instant::now();
        let ago = |secs| now - Duration::from_secs(secs);
        assert!(due(Refresh::OnOpen, None, false, now));
        assert!(!due(Refresh::OnOpen, Some(ago(3600)), false, now));
        assert!(!due(Refresh::Manual, None, false, now));
        assert!(!due(Refresh::Manual, Some(ago(3600)), false, now));
        assert!(due(Refresh::Every(30), None, false, now));
        assert!(!due(Refresh::Every(30), Some(ago(10)), false, now));
        assert!(due(Refresh::Every(30), Some(ago(30)), false, now));
        assert!(
            !due(Refresh::Every(30), Some(ago(30)), true, now),
            "still running"
        );
        assert!(!due(Refresh::OnOpen, None, true, now));

        assert_eq!(
            next_in(Refresh::Every(30), Some(ago(10)), now),
            Some(Duration::from_secs(20))
        );
        assert_eq!(
            next_in(Refresh::Every(30), Some(ago(40)), now),
            Some(Duration::ZERO)
        );
        assert_eq!(next_in(Refresh::Every(30), None, now), None);
        assert_eq!(next_in(Refresh::OnOpen, Some(ago(10)), now), None);
    }

    #[test]
    fn a_change_gives_a_new_revision_and_nothing_else_does() {
        let mut boards = Boards::open(None);
        let board = create(&mut boards, "db", "Signups").unwrap();
        assert_eq!(board.revision.len(), 6);
        assert!(board.revision.chars().all(|c| c.is_ascii_lowercase()));
        let renamed = apply(&mut boards, &board.id, Edit::Rename("Growth".into())).unwrap();
        assert_ne!(renamed.revision, board.revision);
        assert_eq!(boards.get(&board.id).unwrap().revision, renamed.revision);
        let same = apply(&mut boards, &board.id, Edit::Rename("Growth".into())).unwrap();
        assert_eq!(same.revision, renamed.revision);
        let copy = copy_to(&mut boards, &board.id, "db2", "Copy").unwrap();
        assert_eq!(copy.revision.len(), 6);
    }

    fn by_ai(held: &HashMap<String, String>) -> ByAi<'_> {
        ByAi {
            by: AiCaller::Mcp,
            lexing: Lexing::default(),
            held,
        }
    }

    #[test]
    fn an_ai_changes_a_board_only_at_the_revision_it_read() {
        let mut boards = Boards::open(None);
        let board = with_tiles(&mut boards, 1);
        let none = HashMap::new();
        let ai = |boards: &mut Boards, revision: &str, edits: Vec<Edit>| {
            apply_by_ai(boards, &board.id, revision, edits, &by_ai(&none))
        };
        let stale = ai(&mut boards, "aaaaaa", vec![Edit::Rename("X".into())]).unwrap_err();
        assert!(
            stale.starts_with("the board changed since revision aaaaaa"),
            "{stale}"
        );
        assert_eq!(boards.get(&board.id).unwrap().name, "Signups");

        // All or none: the second edit fails, so the rename doesn't happen either.
        let err = ai(
            &mut boards,
            &board.revision,
            vec![
                Edit::Rename("Growth".into()),
                Edit::AddTile(tile("Gone", "delete from t", None)),
            ],
        )
        .unwrap_err();
        assert_eq!(err, "a tile only reads; this query is DELETE");
        assert_eq!(boards.get(&board.id).unwrap().revision, board.revision);

        let b = ai(
            &mut boards,
            &board.revision,
            vec![
                Edit::Rename("Growth".into()),
                Edit::AddTile(tile("New", "select 2", None)),
                Edit::SetView {
                    tile: "t1".into(),
                    view: ChartView::from_key("bars"),
                },
                Edit::SetWidth {
                    tile: "t2".into(),
                    width: Width::Half,
                },
            ],
        )
        .unwrap();
        assert_eq!(b.name, "Growth");
        let mark = Some(AiMark {
            by: AiCaller::Mcp,
            held: None,
        });
        assert_eq!(b.tiles[0].ai, mark);
        assert_eq!(b.tiles[1].ai, mark);
        assert_eq!(b.tiles[1].sql, "-- Set up by MCP\nselect 2");
        assert_ne!(b.revision, board.revision);
    }

    #[test]
    fn an_ai_writes_tiles_that_read_and_leaves_the_rest_to_the_user() {
        let mut boards = Boards::open(None);
        let board = with_tiles(&mut boards, 1);
        let none = HashMap::new();
        let ai = |boards: &mut Boards, edit: Edit| {
            let revision = boards.get(&board.id).unwrap().revision.clone();
            apply_by_ai(boards, &board.id, &revision, vec![edit], &by_ai(&none))
        };
        let query = |sql: &str| Edit::SetQuery {
            tile: "t1".into(),
            sql: sql.into(),
            view: None,
        };
        assert_eq!(
            ai(&mut boards, query("select 1; select 2")).unwrap_err(),
            guard::not_one_read("select 1; select 2").unwrap()
        );
        assert!(ai(&mut boards, query(&"x".repeat(AI_MAX_SQL + 1))).is_err());
        for edit in [
            Edit::RemoveTile("t1".into()),
            Edit::Archive(true),
            Edit::SetRefresh(Refresh::Every(5)),
            Edit::AddLink(Link {
                label: "x".into(),
                to: LinkTo::Url("https://x.io".into()),
            }),
            Edit::Release("t1".into()),
        ] {
            assert_eq!(
                ai(&mut boards, edit).unwrap_err(),
                "only the user may do that"
            );
        }
        let tiles = (0..=AI_MAX_TILES).map(|i| tile("T", &format!("select {i}"), None));
        let many = create_by_ai(
            &mut boards,
            "db",
            "Many",
            "",
            tiles.collect(),
            &by_ai(&none),
        );
        assert!(many.is_err());

        // Its own comment replaces the one before.
        let b = ai(&mut boards, query("-- Set up by the assistant\nselect 3")).unwrap();
        assert_eq!(b.tiles[0].sql, "-- Changed by MCP\nselect 3");
    }

    #[test]
    fn a_query_the_plan_objects_to_waits_until_the_user_ran_it() {
        let mut boards = Boards::open(None);
        let board = with_tiles(&mut boards, 1);
        let held = HashMap::from([("select * from big".to_string(), "heavy".to_string())]);
        let edit = Edit::SetQuery {
            tile: "t1".into(),
            sql: " select * from big ".into(),
            view: None,
        };
        assert_eq!(
            ai_queries(std::slice::from_ref(&edit)),
            ["select * from big"]
        );
        let b = apply_by_ai(
            &mut boards,
            &board.id,
            &board.revision,
            vec![edit],
            &by_ai(&held),
        )
        .unwrap();
        assert_eq!(b.tiles[0].held(), Some("heavy"));
        assert!(Batch::of(&b).tiles.is_empty());

        // A new title keeps the hold.
        let label = Edit::LabelTile {
            tile: "t1".into(),
            title: "All".into(),
            description: String::new(),
        };
        let b = apply_by_ai(
            &mut boards,
            &board.id,
            &b.revision,
            vec![label],
            &by_ai(&held),
        )
        .unwrap();
        assert_eq!(b.tiles[0].held(), Some("heavy"));

        // Run in a tab: released, same revision.
        let released = apply(&mut boards, &board.id, Edit::Release("t1".into())).unwrap();
        assert_eq!(released.tiles[0].held(), None);
        assert_eq!(released.revision, b.revision);
        assert_eq!(boards.get(&board.id).unwrap().tiles[0].held(), None);
    }

    #[test]
    fn a_query_the_user_sets_is_not_held() {
        let mut boards = Boards::open(None);
        let board = with_tiles(&mut boards, 1);
        let held = HashMap::from([("select 9".to_string(), "heavy".to_string())]);
        let edit = Edit::SetQuery {
            tile: "t1".into(),
            sql: "select 9".into(),
            view: None,
        };
        let b = apply_by_ai(
            &mut boards,
            &board.id,
            &board.revision,
            vec![edit],
            &by_ai(&held),
        )
        .unwrap();
        assert!(b.tiles[0].held().is_some());
        let edit = Edit::SetQuery {
            tile: "t1".into(),
            sql: "select 10".into(),
            view: None,
        };
        let b = apply(&mut boards, &board.id, edit).unwrap();
        assert_eq!(b.tiles[0].held(), None);
        assert_eq!(b.tiles[0].ai.as_ref().map(|m| m.by), Some(AiCaller::Mcp));
    }

    #[test]
    fn an_ai_creates_a_board_with_its_tiles() {
        let mut boards = Boards::open(None);
        let b = create_by_ai(
            &mut boards,
            "db",
            " Revenue ",
            " Per month. ",
            vec![tile("Per month", "select 1", ChartView::from_key("bars"))],
            &ByAi {
                by: AiCaller::Assistant,
                lexing: Lexing::default(),
                held: &HashMap::new(),
            },
        )
        .unwrap();
        assert_eq!(
            (b.name.as_str(), b.description.as_str()),
            ("Revenue", "Per month.")
        );
        assert_eq!(ids(&b), ["t1"]);
        assert_eq!(b.tiles[0].sql, "-- Set up by the assistant\nselect 1");
        assert_eq!(b.tiles[0].ai.as_ref().unwrap().by, AiCaller::Assistant);
        assert_eq!(boards.get(&b.id), Some(&b));
    }
}
