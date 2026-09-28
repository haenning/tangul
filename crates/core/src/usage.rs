//! What tangul learns from the queries you ran, as stored: uses of tables, columns
//! and joins, and repeated queries. Ranking with them is `tangul-assist`'s.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageKind {
    Table,
    Column,
    Join,
}

impl UsageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UsageKind::Table => "table",
            UsageKind::Column => "column",
            UsageKind::Join => "join",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "table" => UsageKind::Table,
            "column" => UsageKind::Column,
            "join" => UsageKind::Join,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UsageStat {
    pub count: u32,
    /// Unix seconds.
    pub last_used: i64,
}

/// A distinct query (by fingerprint) from history.
#[derive(Debug, Clone)]
pub struct HistoryPattern {
    /// The most recent run of it.
    pub sql: String,
    pub runs: u32,
}
