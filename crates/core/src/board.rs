//! Boards: several queries of one database at a glance, as stored (one JSON file
//! each). A board has a name, a description and links on top, and tiles below:
//! a query each, shown as a table or a chart, full or half width. Results are never
//! stored; every tile queries the database when the board is shown, and again as
//! its [`Refresh`] says.
//!
//! ```json
//! {
//!   "id": "b18f…",
//!   "name": "Signups",
//!   "description": "Growth after the launch.",
//!   "links": [{ "label": "Epic", "url": "https://…" }],
//!   "database_id": "p18e…",
//!   "refresh": { "every": 60 },
//!   "tiles": [
//!     { "id": "t1", "title": "Per day", "sql": "select …", "view": "bars", "width": "half" }
//!   ]
//! }
//! ```

use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::chart::ChartView;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Board {
    pub id: String,
    pub name: String,
    /// Plain text; the board shows its first line until asked for all of it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Shown as pills below the description.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    /// The database its tiles query; a board never spans databases.
    pub database_id: String,
    #[serde(default)]
    pub refresh: Refresh,
    /// Kept, but out of the way: no longer needed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
    #[serde(default)]
    pub tiles: Vec<Tile>,
}

impl Board {
    pub fn tile(&self, id: &str) -> Option<&Tile> {
        self.tiles.iter().find(|t| t.id == id)
    }

    pub fn tile_mut(&mut self, id: &str) -> Option<&mut Tile> {
        self.tiles.iter_mut().find(|t| t.id == id)
    }

    /// The tiles' different statements, in order: what one refresh sends (tiles
    /// with the same SQL share a result).
    pub fn statements(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for tile in &self.tiles {
            if !out.contains(&tile.sql.as_str()) {
                out.push(&tile.sql);
            }
        }
        out
    }
}

/// A link on a board: a web address for now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub label: String,
    pub url: String,
}

impl Link {
    /// Whether it may be opened: `http` or `https` only.
    pub fn is_web(&self) -> bool {
        let url = self.url.trim();
        ["http://", "https://"].iter().any(|scheme| {
            url.get(..scheme.len())
                .is_some_and(|s| s.eq_ignore_ascii_case(scheme))
                && url.len() > scheme.len()
        })
    }
}

/// When a board's tiles query the database again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refresh {
    /// Each time the board is opened.
    #[default]
    OnOpen,
    /// When opened, then every so many seconds while it is shown.
    Every(u32),
    /// Only when asked: until then the tiles are empty.
    Manual,
}

impl Refresh {
    /// The shortest interval: anything shorter is run this often.
    pub const MIN_SECONDS: u32 = 5;

    /// Whether opening the board runs its tiles.
    pub fn on_open(self) -> bool {
        !matches!(self, Refresh::Manual)
    }

    /// How often the tiles run again while the board is shown.
    pub fn interval(self) -> Option<Duration> {
        match self {
            Refresh::Every(secs) => Some(Duration::from_secs(secs.max(Self::MIN_SECONDS).into())),
            Refresh::OnOpen | Refresh::Manual => None,
        }
    }
}

/// One query on a board, with how its result is shown. The SQL is a copy: changing
/// the bookmark or tab it came from doesn't change the tile.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Tile {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub sql: String,
    /// The table (`None`) or a chart, in [`ChartView::to_key`]'s form.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "view_out",
        deserialize_with = "view_in"
    )]
    pub view: Option<ChartView>,
    #[serde(default)]
    pub width: Width,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Width {
    #[default]
    Full,
    /// Half the board: two in a row.
    Half,
}

/// A row of the board: what [`rows`] lays out. Indexes into the board's tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Full(usize),
    /// A half tile, and the one next to it if the next tile is half too.
    Halves(usize, Option<usize>),
}

/// The tiles in rows, in order: a full tile alone, two half ones side by side. A
/// half tile whose neighbour is full (or that comes last) has its row to itself,
/// still half wide.
pub fn rows(tiles: &[Tile]) -> Vec<Row> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tiles.len() {
        match tiles[i].width {
            Width::Full => {
                out.push(Row::Full(i));
                i += 1;
            }
            Width::Half => {
                let next =
                    (tiles.get(i + 1).map(|t| t.width) == Some(Width::Half)).then_some(i + 1);
                out.push(Row::Halves(i, next));
                i += if next.is_some() { 2 } else { 1 };
            }
        }
    }
    out
}

fn view_out<S: Serializer>(view: &Option<ChartView>, s: S) -> Result<S::Ok, S::Error> {
    match view {
        Some(view) => s.serialize_str(&view.to_key()),
        None => s.serialize_none(),
    }
}

/// An unknown chart (from a newer version, or edited by hand) shows the table.
fn view_in<'de, D: Deserializer<'de>>(d: D) -> Result<Option<ChartView>, D::Error> {
    let key = Option::<String>::deserialize(d)?;
    Ok(key.as_deref().and_then(ChartView::from_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chart::ChartStyle;

    fn tile(id: &str, width: Width) -> Tile {
        Tile {
            id: id.into(),
            title: id.into(),
            sql: format!("select '{id}'"),
            width,
            ..Default::default()
        }
    }

    #[test]
    fn halves_pair_up_and_fulls_stand_alone() {
        use Width::*;
        let tiles: Vec<Tile> = [Half, Half, Full, Half, Full, Half, Half, Half]
            .into_iter()
            .enumerate()
            .map(|(i, w)| tile(&i.to_string(), w))
            .collect();
        assert_eq!(
            rows(&tiles),
            [
                Row::Halves(0, Some(1)),
                Row::Full(2),
                Row::Halves(3, None),
                Row::Full(4),
                Row::Halves(5, Some(6)),
                Row::Halves(7, None),
            ]
        );
        assert_eq!(rows(&[]), []);
    }

    #[test]
    fn refresh_runs_on_open_unless_manual_and_never_faster_than_the_minimum() {
        assert!(Refresh::OnOpen.on_open());
        assert!(Refresh::Every(60).on_open());
        assert!(!Refresh::Manual.on_open());
        assert_eq!(Refresh::OnOpen.interval(), None);
        assert_eq!(Refresh::Manual.interval(), None);
        assert_eq!(Refresh::Every(60).interval(), Some(Duration::from_secs(60)));
        assert_eq!(Refresh::Every(1).interval(), Some(Duration::from_secs(5)));
        assert_eq!(Refresh::Every(0).interval(), Some(Duration::from_secs(5)));
    }

    #[test]
    fn same_sql_runs_once() {
        let mut board = Board {
            tiles: vec![
                tile("a", Width::Full),
                tile("b", Width::Half),
                tile("a", Width::Half),
            ],
            ..Default::default()
        };
        board.tiles[2].id = "c".into();
        assert_eq!(board.statements(), ["select 'a'", "select 'b'"]);
        assert_eq!(board.tile("c").map(|t| t.width), Some(Width::Half));
        board.tile_mut("b").unwrap().title = "B".into();
        assert_eq!(board.tile("b").unwrap().title, "B");
        assert!(board.tile("x").is_none());
    }

    #[test]
    fn only_web_links_open() {
        let link = |url: &str| Link {
            label: "x".into(),
            url: url.into(),
        };
        assert!(link("https://github.com/x").is_web());
        assert!(link(" HTTP://example.com").is_web());
        assert!(!link("https://").is_web());
        assert!(!link("file:///etc/passwd").is_web());
        assert!(!link("javascript:alert(1)").is_web());
        assert!(!link("").is_web());
    }

    #[test]
    fn json_round_trips_and_leaves_defaults_out() {
        let board = Board {
            id: "b".into(),
            name: "Signups".into(),
            description: "Growth.\nMore text.".into(),
            links: vec![Link {
                label: "Epic".into(),
                url: "https://example.com".into(),
            }],
            database_id: "db".into(),
            refresh: Refresh::Every(30),
            archived: true,
            tiles: vec![
                Tile {
                    view: Some(ChartView {
                        style: ChartStyle::StackedBars,
                        series: Some(1),
                    }),
                    description: "per day".into(),
                    ..tile("t", Width::Half)
                },
                tile("u", Width::Full),
            ],
        };
        let json = serde_json::to_string(&board).unwrap();
        assert!(json.contains(r#""refresh":{"every":30}"#), "{json}");
        assert!(json.contains(r#""view":"stacked_bars:1""#), "{json}");
        assert!(json.contains(r#""width":"half""#), "{json}");
        assert_eq!(serde_json::from_str::<Board>(&json).unwrap(), board);

        let plain = Board {
            id: "b".into(),
            name: "n".into(),
            database_id: "db".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&plain).unwrap();
        assert_eq!(
            json,
            r#"{"id":"b","name":"n","database_id":"db","refresh":"on_open","tiles":[]}"#
        );
        assert_eq!(serde_json::from_str::<Board>(&json).unwrap(), plain);
    }

    #[test]
    fn missing_fields_default_and_unknown_charts_show_the_table() {
        let board: Board = serde_json::from_str(
            r#"{"id":"b","name":"n","database_id":"db",
                "tiles":[{"id":"t","title":"T","sql":"select 1","view":"pie"}]}"#,
        )
        .unwrap();
        assert_eq!(board.refresh, Refresh::OnOpen);
        assert!(!board.archived);
        assert_eq!(board.tiles[0].view, None);
        assert_eq!(board.tiles[0].width, Width::Full);
        let manual: Board =
            serde_json::from_str(r#"{"id":"b","name":"n","database_id":"db","refresh":"manual"}"#)
                .unwrap();
        assert_eq!(manual.refresh, Refresh::Manual);
    }
}
