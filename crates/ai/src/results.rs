//! Results kept in tangul so a model can read a whole cell the text cut short
//! (`get_cell`) without the database running the query again. Only what a query
//! already returned is kept, for a while and within a size budget.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Results kept at most.
const KEEP: usize = 8;
/// How long a result is kept.
const TTL: Duration = Duration::from_secs(10 * 60);
/// Bytes all kept results may take; the oldest go first.
const BUDGET: usize = 32 * 1024 * 1024;
/// Longest cell kept; longer ones are kept cut (and say so).
pub(crate) const CELL_BYTES: usize = 1024 * 1024;

/// A handle to the kept results; clones share them. One per client (the MCP server
/// keeps one for all calls of its clients).
#[derive(Clone, Default)]
pub struct Results(Arc<Mutex<Kept>>);

#[derive(Default)]
struct Kept {
    last_id: u64,
    results: VecDeque<Stored>,
}

struct Stored {
    id: String,
    at: Instant,
    /// The exposed connection it ran on, and the statement.
    connection: String,
    sql: String,
    columns: Vec<String>,
    rows: Vec<Vec<Cell>>,
    bytes: usize,
}

/// A kept cell: its value (cut at [`CELL_BYTES`]) and its whole length in characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cell {
    pub value: Option<String>,
    pub chars: usize,
}

/// Why a cell can't be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Missing {
    /// Unknown or expired result: run the query again.
    Result,
    /// The result has no such row or column; what it has.
    Cell { rows: usize, columns: Vec<String> },
}

impl Results {
    /// Keeps `rows` of `sql` on `connection`; returns its id (`r1`, `r2`, …).
    pub(crate) fn keep(
        &self,
        connection: &str,
        sql: &str,
        columns: Vec<String>,
        rows: Vec<Vec<Option<String>>>,
    ) -> String {
        let rows: Vec<Vec<Cell>> = rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|v| Cell {
                        chars: v.as_deref().map_or(0, |v| v.chars().count()),
                        value: v.map(|v| cut_bytes(v, CELL_BYTES)),
                    })
                    .collect()
            })
            .collect();
        let bytes = rows
            .iter()
            .flatten()
            .filter_map(|c| c.value.as_ref())
            .map(String::len)
            .sum();
        let mut kept = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        kept.last_id += 1;
        let id = format!("r{}", kept.last_id);
        kept.results.push_back(Stored {
            id: id.clone(),
            at: Instant::now(),
            connection: connection.to_string(),
            sql: sql.to_string(),
            columns,
            rows,
            bytes,
        });
        kept.prune();
        id
    }

    /// The connection and statement of result `id`, while it is kept.
    pub(crate) fn query(&self, id: &str) -> Option<(String, String)> {
        let mut kept = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        kept.prune();
        kept.results
            .iter()
            .find(|r| r.id.eq_ignore_ascii_case(id.trim()))
            .map(|r| (r.connection.clone(), r.sql.clone()))
    }

    /// Cell `row` (from 1) of `column` (its name, or its number from 1) in result
    /// `id`, if it ran on `connection`.
    pub(crate) fn cell(
        &self,
        id: &str,
        connection: &str,
        row: usize,
        column: &str,
    ) -> Result<Cell, Missing> {
        let mut kept = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        kept.prune();
        let stored = kept
            .results
            .iter()
            .find(|r| r.id.eq_ignore_ascii_case(id.trim()) && r.connection == connection)
            .ok_or(Missing::Result)?;
        let missing = || Missing::Cell {
            rows: stored.rows.len(),
            columns: stored.columns.clone(),
        };
        let col = stored
            .columns
            .iter()
            .position(|c| c == column)
            .or_else(|| {
                stored
                    .columns
                    .iter()
                    .position(|c| c.eq_ignore_ascii_case(column))
            })
            .or_else(|| {
                column
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=stored.columns.len()).contains(n))
                    .map(|n| n - 1)
            })
            .ok_or_else(missing)?;
        row.checked_sub(1)
            .and_then(|r| stored.rows.get(r))
            .and_then(|r| r.get(col))
            .cloned()
            .ok_or_else(missing)
    }
}

impl Kept {
    fn prune(&mut self) {
        self.results.retain(|r| r.at.elapsed() < TTL);
        while self.results.len() > KEEP
            || (self.results.len() > 1
                && self.results.iter().map(|r| r.bytes).sum::<usize>() > BUDGET)
        {
            self.results.pop_front();
        }
    }
}

/// `v` cut to at most `max` bytes, on a character boundary.
fn cut_bytes(mut v: String, max: usize) -> String {
    if v.len() > max {
        let mut end = max;
        while !v.is_char_boundary(end) {
            end -= 1;
        }
        v.truncate(end);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(values: &[&str]) -> Vec<Vec<Option<String>>> {
        values
            .iter()
            .map(|v| vec![Some(v.to_string()), None])
            .collect()
    }

    #[test]
    fn keeps_results_by_id_and_connection() {
        let r = Results::default();
        let cols = vec!["doc".to_string(), "Note".to_string()];
        let a = r.keep("shop", "select 1", cols.clone(), rows(&["one", "two"]));
        let b = r.keep("shop", "select 1", cols.clone(), rows(&["three"]));
        assert_eq!((a.as_str(), b.as_str()), ("r1", "r2"));
        assert_eq!(r.query(" R2 "), Some(("shop".into(), "select 1".into())));
        assert_eq!(r.query("r9"), None);
        let cell = |id: &str, conn: &str, row: usize, col: &str| r.cell(id, conn, row, col);
        assert_eq!(
            cell("r1", "shop", 2, "doc"),
            Ok(Cell {
                value: Some("two".into()),
                chars: 3
            })
        );
        // By number, or any case of its name.
        assert_eq!(
            cell("R1", "shop", 1, "1").unwrap().value.as_deref(),
            Some("one")
        );
        assert_eq!(cell("r1", "shop", 1, "note").unwrap().value, None);
        // Not from another connection's client, not past its rows or columns.
        assert_eq!(cell("r1", "crm", 1, "doc"), Err(Missing::Result));
        assert_eq!(cell("r9", "shop", 1, "doc"), Err(Missing::Result));
        assert_eq!(
            cell("r2", "shop", 2, "doc"),
            Err(Missing::Cell {
                rows: 1,
                columns: cols
            })
        );
        assert!(cell("r2", "shop", 0, "doc").is_err());
        assert!(cell("r2", "shop", 1, "3").is_err());
    }

    #[test]
    fn keeps_the_latest_within_limits() {
        let r = Results::default();
        for _ in 0..KEEP + 2 {
            r.keep("shop", "select 1", vec!["a".into()], rows(&["x"]));
        }
        assert_eq!(r.cell("r2", "shop", 1, "a"), Err(Missing::Result));
        assert!(r.cell("r3", "shop", 1, "a").is_ok());
        // A huge cell is kept cut, on a character boundary, knowing its length.
        let big = "é".repeat(CELL_BYTES);
        let id = r.keep("shop", "select 1", vec!["a".into()], vec![vec![Some(big)]]);
        let cell = r.cell(&id, "shop", 1, "a").unwrap();
        assert!(cell.value.unwrap().len() <= CELL_BYTES);
        assert_eq!(cell.chars, CELL_BYTES);
    }
}
