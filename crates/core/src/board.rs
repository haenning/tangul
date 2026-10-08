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
//!   "links": [
//!     { "label": "Epic", "url": "https://…" },
//!     { "label": "Orders per day", "bookmark": "p18f…" },
//!     { "label": "By plan", "query": { "sql": "select …", "view": "bars" } }
//!   ],
//!   "database_id": "p18e…",
//!   "refresh": { "every": 60 },
//!   "revision": "kqzmwa",
//!   "tiles": [
//!     { "id": "t1", "title": "Per day", "sql": "select …", "view": "bars", "width": "half" },
//!     { "id": "t2", "title": "By plan", "sql": "select …", "ai": { "by": "mcp", "unseen": true } }
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
    /// New with every change (six random letters): an AI changes the board only
    /// at the revision it read, never at a stale one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub revision: String,
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

/// A link on a board, shown by its label and what it does ("Go to bookmark",
/// "Open in a new tab"); where it points is only stored, never shown.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub label: String,
    #[serde(flatten)]
    pub to: LinkTo,
}

/// Where a link goes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkTo {
    /// A web page (`http` or `https` only).
    Url(String),
    /// A bookmark of the database, by id.
    Bookmark(String),
    /// Another board, by id.
    Board(String),
    /// A query (typed, or from the history) opened in a new tab, shown as `view`.
    Query {
        sql: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "view_out",
            deserialize_with = "view_in"
        )]
        view: Option<ChartView>,
    },
}

impl Link {
    /// Whether it may be opened: a web address must be `http` or `https`, a query
    /// must have SQL, a bookmark or board an id.
    pub fn is_valid(&self) -> bool {
        match &self.to {
            LinkTo::Url(url) => is_web(url),
            LinkTo::Bookmark(id) | LinkTo::Board(id) => !id.trim().is_empty(),
            LinkTo::Query { sql, .. } => !sql.trim().is_empty(),
        }
    }
}

/// `http://` or `https://` and something after it.
pub fn is_web(url: &str) -> bool {
    let url = url.trim();
    ["http://", "https://"].iter().any(|scheme| {
        url.get(..scheme.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(scheme))
            && url.len() > scheme.len()
    })
}

/// What the user typed as a web address, as one that opens: as typed with
/// `http://` or `https://`, else with `https://` in front when it looks like a host
/// (`taend.eu`, `www.x.de/path`, `localhost:3000`). `None` for anything else (other
/// schemes such as `javascript:` or `file:`, spaces, no host).
pub fn web_address(input: &str) -> Option<String> {
    let input = input.trim();
    if is_web(input) {
        return Some(input.to_string());
    }
    if input.is_empty() || input.contains(char::is_whitespace) {
        return None;
    }
    let authority = input.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match authority.split_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => host,
        Some(_) => return None,
        None => authority,
    };
    let labels_ok = host
        .split('.')
        .all(|l| !l.is_empty() && l.chars().all(|c| c.is_alphanumeric() || c == '-'));
    let host_ok = labels_ok && (host.contains('.') || host.eq_ignore_ascii_case("localhost"));
    host_ok.then(|| format!("https://{input}"))
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
    /// Set up or changed by an AI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai: Option<AiMark>,
}

/// That an AI set up or changed a tile, and whether the user has run it since.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiMark {
    pub by: AiCaller,
    /// Changed by the AI and not yet run by the user in a tab of its own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unseen: bool,
}

/// Which AI changed a tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiCaller {
    /// The in-app assistant.
    Assistant,
    /// An MCP client.
    Mcp,
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

pub(crate) fn view_out<S: Serializer>(view: &Option<ChartView>, s: S) -> Result<S::Ok, S::Error> {
    match view {
        Some(view) => s.serialize_str(&view.to_key()),
        None => s.serialize_none(),
    }
}

/// An unknown chart (from a newer version, or edited by hand) shows the table.
pub(crate) fn view_in<'de, D: Deserializer<'de>>(d: D) -> Result<Option<ChartView>, D::Error> {
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
    fn only_web_addresses_and_complete_links_open() {
        let link = |to: LinkTo| Link {
            label: "x".into(),
            to,
        };
        let url = |u: &str| link(LinkTo::Url(u.into())).is_valid();
        assert!(url("https://github.com/x"));
        assert!(url(" HTTP://example.com"));
        assert!(!url("https://"));
        assert!(!url("file:///etc/passwd"));
        assert!(!url("javascript:alert(1)"));
        assert!(!url(""));
        // As typed, without a scheme: https.
        let typed = |u: &str| web_address(u);
        assert_eq!(typed("taend.eu").as_deref(), Some("https://taend.eu"));
        assert_eq!(
            typed(" www.example.de/a/b?c=1 ").as_deref(),
            Some("https://www.example.de/a/b?c=1")
        );
        assert_eq!(
            typed("localhost:3000").as_deref(),
            Some("https://localhost:3000")
        );
        assert_eq!(typed("http://x.io").as_deref(), Some("http://x.io"));
        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "mailto:a@b.c",
            "taend",
            "taend .eu",
            "a..b",
            "x.io:abc",
            "",
        ] {
            assert_eq!(typed(bad), None, "{bad}");
        }
        assert!(link(LinkTo::Bookmark("b1".into())).is_valid());
        assert!(!link(LinkTo::Board(" ".into())).is_valid());
        let query = |sql: &str| LinkTo::Query {
            sql: sql.into(),
            view: None,
        };
        assert!(link(query("select 1")).is_valid());
        assert!(!link(query("  ")).is_valid());
    }

    #[test]
    fn links_are_stored_by_what_they_point_to() {
        let links = vec![
            Link {
                label: "Epic".into(),
                to: LinkTo::Url("https://x.io".into()),
            },
            Link {
                label: "Orders".into(),
                to: LinkTo::Bookmark("b1".into()),
            },
            Link {
                label: "Other".into(),
                to: LinkTo::Board("b2".into()),
            },
            Link {
                label: "By plan".into(),
                to: LinkTo::Query {
                    sql: "select 1".into(),
                    view: ChartView::from_key("bars"),
                },
            },
            Link {
                label: "Rows".into(),
                to: LinkTo::Query {
                    sql: "select 2".into(),
                    view: None,
                },
            },
        ];
        let json = serde_json::to_string(&links).unwrap();
        assert_eq!(
            json,
            r#"[{"label":"Epic","url":"https://x.io"},{"label":"Orders","bookmark":"b1"},{"label":"Other","board":"b2"},{"label":"By plan","query":{"sql":"select 1","view":"bars"}},{"label":"Rows","query":{"sql":"select 2"}}]"#
        );
        assert_eq!(serde_json::from_str::<Vec<Link>>(&json).unwrap(), links);
    }

    #[test]
    fn json_round_trips_and_leaves_defaults_out() {
        let board = Board {
            id: "b".into(),
            name: "Signups".into(),
            description: "Growth.\nMore text.".into(),
            links: vec![Link {
                label: "Epic".into(),
                to: LinkTo::Url("https://example.com".into()),
            }],
            database_id: "db".into(),
            refresh: Refresh::Every(30),
            archived: true,
            revision: "kqzmwa".into(),
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
                Tile {
                    ai: Some(AiMark {
                        by: AiCaller::Mcp,
                        unseen: true,
                    }),
                    ..tile("v", Width::Full)
                },
            ],
        };
        let json = serde_json::to_string(&board).unwrap();
        assert!(json.contains(r#""refresh":{"every":30}"#), "{json}");
        assert!(json.contains(r#""revision":"kqzmwa""#), "{json}");
        assert!(
            json.contains(r#""ai":{"by":"mcp","unseen":true}"#),
            "{json}"
        );
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
