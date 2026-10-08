//! Boards: the one way to change them, for the UI ([`create`], [`apply`] an
//! [`Edit`], [`copy_to`], [`delete`]) and for an AI ([`create_by_ai`],
//! [`apply_by_ai`]), over the stored boards (`tangul_data::boards`); and what a
//! refresh sends ([`Batch`]) and when ([`due`]). Every change gives the board a new
//! revision.

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
    /// The user ran the tile's query in a tab: what an AI changed is seen.
    Seen(String),
}

/// A new, empty board for `database_id`.
pub fn create(boards: &mut Boards, database_id: &str, name: &str) -> Result<Board, String> {
    let board = Board {
        id: new_id(),
        name: name_of(name)?,
        database_id: database_id.into(),
        ..Board::default()
    };
    save(boards, board)
}

/// Applies `edit` to board `id` and saves it; the board as it is now. A query the
/// user sets counts as seen.
pub fn apply(boards: &mut Boards, id: &str, edit: Edit) -> Result<Board, String> {
    let mut board = boards.get(id).cloned().ok_or("no such board")?;
    let seen = match &edit {
        Edit::SetQuery { tile, .. } => Some(tile.clone()),
        _ => None,
    };
    edit_board(&mut board, edit)?;
    if let Some(tile) = seen {
        edit_board(&mut board, Edit::Seen(tile))?;
    }
    save(boards, board)
}

/// A new board by an AI for `database_id`, with `tiles` (see [`apply_by_ai`]).
pub fn create_by_ai(
    boards: &mut Boards,
    database_id: &str,
    name: &str,
    description: &str,
    tiles: Vec<Tile>,
    by: AiCaller,
    lexing: Lexing,
) -> Result<Board, String> {
    let mut board = Board {
        id: new_id(),
        name: name_of(name)?,
        description: description.trim().into(),
        database_id: database_id.into(),
        ..Board::default()
    };
    let edits = tiles.into_iter().map(Edit::AddTile).collect();
    edit_by_ai(&mut board, edits, by, lexing)?;
    save(boards, board)
}

/// Applies an AI's `edits` to board `id`, all or none, and only at `revision`: the
/// one it read. A board that changed since is left as it is. Only names, texts,
/// queries, views, widths and order: links, refresh, removing tiles or the board stay
/// the user's. A tile's query must be one statement that reads (by the database's
/// `lexing`). The tiles it adds or changes are marked as changed by `by`, unseen.
pub fn apply_by_ai(
    boards: &mut Boards,
    id: &str,
    revision: &str,
    edits: Vec<Edit>,
    by: AiCaller,
    lexing: Lexing,
) -> Result<Board, String> {
    let mut board = boards.get(id).cloned().ok_or("no such board")?;
    if board.revision != revision {
        return Err(format!(
            "the board changed since revision {revision} (now {}): nothing was changed; \
             read it again and redo the changes on what it is now",
            board.revision
        ));
    }
    edit_by_ai(&mut board, edits, by, lexing)?;
    save(boards, board)
}

fn edit_by_ai(
    board: &mut Board,
    edits: Vec<Edit>,
    by: AiCaller,
    lexing: Lexing,
) -> Result<(), String> {
    for edit in edits {
        let marked = match &edit {
            Edit::AddTile(tile) => {
                check_read(&tile.sql, lexing)?;
                None
            }
            Edit::SetQuery { tile, sql, .. } => {
                check_read(sql, lexing)?;
                Some(tile.clone())
            }
            Edit::LabelTile { tile, .. } | Edit::SetView { tile, .. } => Some(tile.clone()),
            Edit::Rename(_) | Edit::Describe(_) | Edit::SetWidth { .. } | Edit::MoveTile { .. } => {
                None
            }
            Edit::SetLinks(_)
            | Edit::AddLink(_)
            | Edit::SetRefresh(_)
            | Edit::Archive(_)
            | Edit::RemoveTile(_)
            | Edit::Seen(_) => return Err("only the user may do that".into()),
        };
        let added = matches!(edit, Edit::AddTile(_));
        edit_board(board, edit)?;
        let tile = match (added, marked) {
            (true, _) => board.tiles.last_mut(),
            (false, Some(id)) => board.tile_mut(&id),
            (false, None) => None,
        };
        if let Some(tile) = tile {
            tile.ai = Some(AiMark { by, unseen: true });
        }
    }
    if board.tiles.len() > AI_MAX_TILES {
        return Err(format!("at most {AI_MAX_TILES} tiles on a board"));
    }
    Ok(())
}

/// That `sql` is one statement that reads, as the gate will check it on a board.
fn check_read(sql: &str, lexing: Lexing) -> Result<(), String> {
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

/// Saves `board` with a new revision.
fn save(boards: &mut Boards, mut board: Board) -> Result<Board, String> {
    board.revision = new_revision();
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
    save(boards, board)
}

pub fn delete(boards: &mut Boards, id: &str) -> Result<(), String> {
    boards.delete(id)
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
        Edit::Seen(tile) => {
            if let Some(mark) = &mut tile_mut(board, &tile)?.ai {
                mark.unseen = false;
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
/// tile shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub statements: Vec<String>,
    /// Per tile, in the board's order: its index in `statements`.
    pub tiles: Vec<usize>,
}

impl Batch {
    pub fn of(board: &Board) -> Self {
        let statements: Vec<String> = board.statements().into_iter().map(String::from).collect();
        let tiles = board
            .tiles
            .iter()
            .map(|t| {
                statements
                    .iter()
                    .position(|s| *s == t.sql)
                    .expect("every tile's statement")
            })
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
                tiles: vec![0, 1, 0],
            }
        );
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
    fn every_change_gives_a_new_revision() {
        let mut boards = Boards::open(None);
        let board = create(&mut boards, "db", "Signups").unwrap();
        assert_eq!(board.revision.len(), 6);
        assert!(board.revision.chars().all(|c| c.is_ascii_lowercase()));
        let renamed = apply(&mut boards, &board.id, Edit::Rename("Growth".into())).unwrap();
        assert_ne!(renamed.revision, board.revision);
        assert_eq!(boards.get(&board.id).unwrap().revision, renamed.revision);
        let copy = copy_to(&mut boards, &board.id, "db2", "Copy").unwrap();
        assert_eq!(copy.revision.len(), 6);
    }

    #[test]
    fn an_ai_changes_a_board_only_at_the_revision_it_read() {
        let mut boards = Boards::open(None);
        let board = with_tiles(&mut boards, 1);
        let ai = |boards: &mut Boards, revision: &str, edits: Vec<Edit>| {
            apply_by_ai(
                boards,
                &board.id,
                revision,
                edits,
                AiCaller::Mcp,
                Lexing::default(),
            )
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
            unseen: true,
        });
        assert_eq!(b.tiles[0].ai, mark);
        assert_eq!(b.tiles[1].ai, mark);
        assert_ne!(b.revision, board.revision);
    }

    #[test]
    fn an_ai_writes_tiles_that_read_and_leaves_the_rest_to_the_user() {
        let mut boards = Boards::open(None);
        let board = with_tiles(&mut boards, 1);
        let ai = |boards: &mut Boards, edit: Edit| {
            let revision = boards.get(&board.id).unwrap().revision.clone();
            apply_by_ai(
                boards,
                &board.id,
                &revision,
                vec![edit],
                AiCaller::Assistant,
                Lexing::default(),
            )
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
            Edit::Seen("t1".into()),
        ] {
            assert_eq!(
                ai(&mut boards, edit).unwrap_err(),
                "only the user may do that"
            );
        }
        let tiles = (0..=AI_MAX_TILES).map(|i| tile("T", &format!("select {i}"), None));
        assert!(
            create_by_ai(
                &mut boards,
                "db",
                "Many",
                "",
                tiles.collect(),
                AiCaller::Mcp,
                Lexing::default()
            )
            .is_err()
        );

        let b = ai(&mut boards, query("select 3")).unwrap();
        assert!(b.tiles[0].ai.unwrap().unseen);
        // Run in a tab, or saved there by the user: seen.
        let b = apply(&mut boards, &board.id, Edit::Seen("t1".into())).unwrap();
        assert_eq!(
            b.tiles[0].ai,
            Some(AiMark {
                by: AiCaller::Assistant,
                unseen: false
            })
        );
        ai(&mut boards, query("select 4")).unwrap();
        let b = apply(
            &mut boards,
            &board.id,
            Edit::SetQuery {
                tile: "t1".into(),
                sql: "select 5".into(),
                view: None,
            },
        )
        .unwrap();
        assert!(!b.tiles[0].ai.unwrap().unseen);
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
            AiCaller::Assistant,
            Lexing::default(),
        )
        .unwrap();
        assert_eq!(
            (b.name.as_str(), b.description.as_str()),
            ("Revenue", "Per month.")
        );
        assert_eq!(ids(&b), ["t1"]);
        assert_eq!(b.tiles[0].ai.unwrap().by, AiCaller::Assistant);
        assert_eq!(boards.get(&b.id), Some(&b));
    }
}
