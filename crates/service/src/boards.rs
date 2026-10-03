//! Boards: the one way to change them, for the UI now and the assistant and MCP
//! tools later ([`create`], [`apply`] an [`Edit`], [`copy_to`], [`delete`]), over the
//! stored boards (`tangul_data::boards`); and what a refresh sends ([`Batch`]) and
//! when ([`due`]).

use std::time::{Duration, Instant};

use tangul_core::board::{Board, Link, LinkTo, Refresh, Tile, Width};
use tangul_core::chart::ChartView;
use tangul_data::boards::Boards;
use tangul_data::profiles::new_id;

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
}

/// A new, empty board for `database_id`.
pub fn create(boards: &mut Boards, database_id: &str, name: &str) -> Result<Board, String> {
    let board = Board {
        id: new_id(),
        name: name_of(name)?,
        database_id: database_id.into(),
        ..Board::default()
    };
    boards.save(board.clone())?;
    Ok(board)
}

/// Applies `edit` to board `id` and saves it; the board as it is now.
pub fn apply(boards: &mut Boards, id: &str, edit: Edit) -> Result<Board, String> {
    let mut board = boards.get(id).cloned().ok_or("no such board")?;
    edit_board(&mut board, edit)?;
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
    boards.save(board.clone())?;
    Ok(board)
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
        LinkTo::Url(url) => LinkTo::Url(url.trim().into()),
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
        assert_eq!(boards.get(&id).unwrap().links.len(), 3, "unchanged");
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
}
