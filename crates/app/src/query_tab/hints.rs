//! What an empty query tab tells about the app: a few short hints under the
//! editor's placeholder, chosen at random for each new tab.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// How many hints a tab shows.
const SHOWN: usize = 2;

/// One thing the app can do: a title and a line on how.
pub struct Hint {
    pub title: &'static str,
    pub text: &'static str,
}

/// Every hint. Each names a feature that exists; check the shortcuts against the
/// key bindings in `main.rs` when you add one.
const HINTS: &[Hint] = &[
    Hint {
        title: "Joins written for you",
        text: "After FROM users u, type JOIN and pick a join from the suggestions.",
    },
    Hint {
        title: "Charts",
        text: "Run a grouped query (GROUP BY date or category), then show it as a chart.",
    },
    Hint {
        title: "Follow relations",
        text: "Select rows, right-click, and open the rows they reference or the rows that reference them.",
    },
    Hint {
        title: "Mark and copy",
        text: "Click a column name to mark it, ⇧-click or drag for a rectangle; ⌘C copies the mark.",
    },
    Hint {
        title: "Copy as CSV, JSON, INSERT",
        text: "Right-click a result: Copy gives CSV or an IN list, Copy with headers CSV, JSON, Markdown or INSERT statements.",
    },
    Hint {
        title: "Save as Excel, CSV, JSON",
        text: "Right-click a result and pick Save as: the mark, or everything loaded, as a file.",
    },
    Hint {
        title: "Command palette",
        text: "⌘⇧P searches commands, bookmarks and history.",
    },
    Hint {
        title: "Bookmarks",
        text: "⌘D bookmarks the query in the editor.",
    },
    Hint {
        title: "Compare values",
        text: "Press Space on a cell to inspect it, then right-click another and pick Compare with A.",
    },
    Hint {
        title: "Boards",
        text: "Right-click a query tab and pick Add to board to keep its result on a dashboard.",
    },
    Hint {
        title: "Rename a tab",
        text: "Double-click a tab's title and type the new one.",
    },
    Hint {
        title: "Ask in plain words",
        text: "With the assistant set up, a question typed here goes to it instead of the database.",
    },
];

/// The hints for a new tab.
pub fn for_new_tab() -> Vec<&'static Hint> {
    pick(seed())
}

/// `SHOWN` different hints, which ones depending on `seed`.
fn pick(seed: u64) -> Vec<&'static Hint> {
    let mut pool: Vec<&Hint> = HINTS.iter().collect();
    let mut state = seed;
    let mut picked = Vec::new();
    while picked.len() < SHOWN && !pool.is_empty() {
        // splitmix64
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        picked.push(pool.swap_remove((z % pool.len() as u64) as usize));
    }
    picked
}

/// Differs between tabs, also those opened in the same instant.
fn seed() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    now ^ COUNTER.fetch_add(1, Ordering::Relaxed).rotate_left(32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_hints_each_time() {
        for seed in 0..50 {
            let picked = pick(seed);
            assert_eq!(picked.len(), SHOWN);
            assert_ne!(picked[0].title, picked[1].title);
        }
        let all: std::collections::HashSet<_> = (0..50)
            .map(|seed| pick(seed).iter().map(|h| h.title).collect::<Vec<_>>())
            .collect();
        assert!(all.len() > 1);
    }
}
