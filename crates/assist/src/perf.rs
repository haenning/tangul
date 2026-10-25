//! Ready-made performance boards: what a dialect offers (indexes, queries, tables &
//! storage) for the server it is connected to. SQL only, no I/O: one query reads what
//! the server has ([`crate::dialect::Dialect::perf_facts_sql`], read with
//! [`Facts::read`]), the dialect builds the boards from that. Every tile reads the
//! server's own statistics, most important rows first, at most [`TOP`] of them.

use tangul_core::board::Width;

/// Rows a tile shows at most.
pub const TOP: usize = 25;

/// Which board.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Indexes,
    Queries,
    Storage,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Indexes, Kind::Queries, Kind::Storage];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Indexes => "Indexes",
            Kind::Queries => "Queries",
            Kind::Storage => "Tables & storage",
        }
    }
}

/// What the server has that the boards read, from one row of the dialect's facts
/// query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// Major and minor version of the server.
    pub version: (u32, u32),
    /// MariaDB rather than MySQL.
    pub mariadb: bool,
    /// Statistics per statement (Postgres: `pg_stat_statements`; MySQL:
    /// `performance_schema`).
    pub statements: Stats,
}

/// Whether statement statistics can be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stats {
    Yes,
    /// Not there.
    Missing,
    /// There but not collecting (Postgres: installed, not preloaded).
    NotLoaded,
}

impl Facts {
    /// The facts in the columns `major`, `minor`, `mariadb`, `statements` (`yes`,
    /// `no`, `not loaded`) of the facts query's row; what can't be read is assumed
    /// to be there (a tile that then fails says why).
    ///
    /// ```
    /// use tangul_assist::perf::{Facts, Stats};
    /// let row = [("major", Some("16")), ("minor", Some("4")), ("mariadb", Some("0")),
    ///            ("statements", Some("not loaded"))];
    /// let facts = Facts::read(&row);
    /// assert_eq!((facts.version, facts.mariadb, facts.statements), ((16, 4), false, Stats::NotLoaded));
    /// ```
    pub fn read(row: &[(&str, Option<&str>)]) -> Self {
        let get = |name: &str| {
            row.iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .and_then(|(_, v)| *v)
                .map(str::trim)
        };
        let number = |name| get(name).and_then(|v| v.parse().ok()).unwrap_or(0);
        Facts {
            version: (number("major"), number("minor")),
            mariadb: matches!(get("mariadb"), Some("1" | "t" | "true")),
            statements: match get("statements") {
                Some("no") => Stats::Missing,
                Some("not loaded") => Stats::NotLoaded,
                _ => Stats::Yes,
            },
        }
    }
}

/// A board as created: copied into a new board, tiles in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Board {
    pub kind: Kind,
    pub description: String,
    pub tiles: Vec<Tile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile {
    pub title: String,
    pub description: String,
    pub sql: String,
    pub width: Width,
}

impl Tile {
    pub fn new(title: &str, description: &str, sql: &str) -> Self {
        Tile {
            title: title.into(),
            description: description.into(),
            sql: sql.trim().to_string(),
            width: Width::Full,
        }
    }

    pub fn half(self) -> Self {
        Tile {
            width: Width::Half,
            ..self
        }
    }
}

/// A board the menu offers, or why it can't now (with what to do about it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub board: Board,
    pub unavailable: Option<String>,
}

impl Offer {
    pub fn ready(board: Board) -> Self {
        Offer {
            board,
            unavailable: None,
        }
    }

    pub fn needs(board: Board, why: impl Into<String>) -> Self {
        Offer {
            board,
            unavailable: Some(why.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::dialect_for;
    use crate::guard;
    use tangul_core::DriverKind;

    fn facts(version: (u32, u32), mariadb: bool, statements: Stats) -> Facts {
        Facts {
            version,
            mariadb,
            statements,
        }
    }

    /// Every board of every dialect for each kind of server: the facts query and
    /// each tile pass the checks the gate makes for a board's reads.
    #[test]
    fn every_tile_is_one_read_the_gate_lets_through() {
        let servers = [
            (DriverKind::Postgres, facts((16, 4), false, Stats::Yes)),
            (DriverKind::Postgres, facts((12, 0), false, Stats::Missing)),
            (
                DriverKind::Postgres,
                facts((14, 0), false, Stats::NotLoaded),
            ),
            (DriverKind::Mysql, facts((8, 4), false, Stats::Yes)),
            (DriverKind::Mysql, facts((11, 4), true, Stats::Missing)),
            (DriverKind::Mysql, facts((8, 0), false, Stats::Missing)),
        ];
        for (kind, facts) in servers {
            let d = dialect_for(kind);
            let reads = |sql: &str, what: &str| {
                let lexing = d.lexing();
                assert_eq!(guard::not_reads_with(sql, lexing), [], "{kind:?} {what}");
                assert_eq!(
                    guard::not_one_read(&guard::read_text(sql, lexing)),
                    None,
                    "{kind:?} {what}"
                );
            };
            reads(d.perf_facts_sql().unwrap(), "facts");
            let offers = d.perf_boards(&facts);
            let kinds: Vec<Kind> = offers.iter().map(|o| o.board.kind).collect();
            assert_eq!(kinds, Kind::ALL, "{kind:?}");
            for offer in &offers {
                let board = &offer.board;
                assert!(!board.tiles.is_empty(), "{kind:?} {:?}", board.kind);
                let mut titles: Vec<&str> = board.tiles.iter().map(|t| t.title.as_str()).collect();
                titles.dedup();
                assert_eq!(titles.len(), board.tiles.len(), "{kind:?} {:?}", board.kind);
                for tile in &board.tiles {
                    reads(&tile.sql, &tile.title);
                    // A list is capped; a one-row summary is half wide.
                    assert!(
                        tile.sql.ends_with(&format!("LIMIT {TOP}")) || tile.width == Width::Half,
                        "{kind:?} {}",
                        tile.title
                    );
                }
            }
        }
        // What isn't there says what to do.
        let pg = dialect_for(DriverKind::Postgres);
        let missing = pg.perf_boards(&facts((16, 0), false, Stats::Missing));
        assert!(
            missing[1]
                .unavailable
                .as_deref()
                .unwrap()
                .contains("CREATE EXTENSION")
        );
        let loaded = pg.perf_boards(&facts((16, 0), false, Stats::NotLoaded));
        assert!(
            loaded[1]
                .unavailable
                .as_deref()
                .unwrap()
                .contains("shared_preload_libraries")
        );
        assert!(
            pg.perf_boards(&facts((16, 0), false, Stats::Yes))
                .iter()
                .all(|o| o.unavailable.is_none())
        );
        let my = dialect_for(DriverKind::Mysql);
        let off = my.perf_boards(&facts((8, 0), false, Stats::Missing));
        assert!(
            off[1]
                .unavailable
                .as_deref()
                .unwrap()
                .contains("performance_schema")
        );
        assert!(off[0].unavailable.is_none(), "indexes without usage still");
        // Postgres before 13 names pg_stat_statements' columns differently.
        let old = pg.perf_boards(&facts((12, 0), false, Stats::Yes));
        assert!(
            old[1]
                .board
                .tiles
                .iter()
                .any(|t| t.sql.contains("total_time"))
        );
        assert!(
            !old[1]
                .board
                .tiles
                .iter()
                .any(|t| t.sql.contains("total_exec_time"))
        );
        // SQLite has none.
        assert!(dialect_for(DriverKind::Sqlite).perf_facts_sql().is_none());
    }

    #[test]
    fn facts_assume_what_cant_be_read() {
        let facts = Facts::read(&[("MAJOR", Some(" 8 ")), ("mariadb", Some("1"))]);
        assert_eq!(facts.version, (8, 0));
        assert!(facts.mariadb);
        assert_eq!(facts.statements, Stats::Yes);
        assert_eq!(
            Facts::read(&[("statements", Some("no"))]).statements,
            Stats::Missing
        );
        assert_eq!(Kind::Storage.name(), "Tables & storage");
        let tile = Tile::new("t", "", "  SELECT 1\n").half();
        assert_eq!((tile.sql.as_str(), tile.width), ("SELECT 1", Width::Half));
    }
}
