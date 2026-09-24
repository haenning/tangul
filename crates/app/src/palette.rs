//! Entries of the command palette (⌘⇧P): app commands, bookmarks and history in one
//! searchable list. The dialog itself is opened by the workspace.

use rusdb_assist::suggest::one_line;

use crate::local::{Bookmark, HistoryEntry};

/// Length at which a history entry's SQL is cut in the list.
const LABEL_LEN: usize = 120;

#[derive(Debug, Clone, PartialEq)]
pub enum PaletteAction {
    Settings,
    /// Bookmark id.
    Bookmark(String),
    /// SQL of the history entry.
    History(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PaletteKind {
    Command,
    Bookmark,
    History,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PaletteItem {
    pub label: String,
    /// Searched as well as the label.
    pub search: String,
    pub action: PaletteAction,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PaletteGroup {
    pub kind: PaletteKind,
    pub label: &'static str,
    pub items: Vec<PaletteItem>,
}

/// Groups in display order; groups without entries are left out.
pub fn groups(bookmarks: &[Bookmark], history: &[HistoryEntry]) -> Vec<PaletteGroup> {
    let commands = vec![PaletteItem {
        label: "Open settings".into(),
        search: "preferences settings.json font size editor".into(),
        action: PaletteAction::Settings,
    }];
    let bookmarks = bookmarks
        .iter()
        .map(|b| PaletteItem {
            label: b.name.clone(),
            search: one_line(&b.sql, usize::MAX),
            action: PaletteAction::Bookmark(b.id.clone()),
        })
        .collect();
    let history = history
        .iter()
        .map(|h| {
            let label = one_line(&h.sql, LABEL_LEN);
            PaletteItem {
                search: one_line(&h.sql, usize::MAX),
                label,
                action: PaletteAction::History(h.sql.clone()),
            }
        })
        .collect();
    [
        (PaletteKind::Command, "Commands", commands),
        (PaletteKind::Bookmark, "Bookmarks", bookmarks),
        (PaletteKind::History, "History", history),
    ]
    .into_iter()
    .filter(|(_, _, items): &(_, _, Vec<PaletteItem>)| !items.is_empty())
    .map(|(kind, label, items)| PaletteGroup { kind, label, items })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bookmark(id: &str, name: &str, sql: &str) -> Bookmark {
        Bookmark {
            id: id.into(),
            name: name.into(),
            sql: sql.into(),
            database_id: None,
        }
    }

    fn history(sql: &str) -> HistoryEntry {
        HistoryEntry {
            sql: sql.into(),
            started_at: 0,
            duration_ms: 1,
            rows: None,
            error: None,
        }
    }

    #[test]
    fn commands_come_first_then_bookmarks_then_history() {
        let groups = groups(
            &[bookmark("b1", "Active users", "SELECT *\nFROM users")],
            &[history("SELECT 1")],
        );
        let kinds: Vec<_> = groups.iter().map(|g| g.kind).collect();
        assert_eq!(
            kinds,
            [
                PaletteKind::Command,
                PaletteKind::Bookmark,
                PaletteKind::History
            ]
        );
        assert_eq!(groups[0].items[0].action, PaletteAction::Settings);
        assert_eq!(
            groups[1].items[0].action,
            PaletteAction::Bookmark("b1".into())
        );
        assert_eq!(
            groups[2].items[0].action,
            PaletteAction::History("SELECT 1".into())
        );
    }

    #[test]
    fn empty_groups_are_left_out() {
        let groups = groups(&[], &[]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].kind, PaletteKind::Command);
    }

    #[test]
    fn sql_is_searchable_and_shown_on_one_line() {
        let long = format!("SELECT {}\nFROM t", "x, ".repeat(100));
        let groups = groups(&[bookmark("b", "Report", &long)], &[history(&long)]);
        // Bookmarks are found by their SQL as well as their name.
        assert!(groups[1].items[0].search.contains("FROM t"));
        assert!(!groups[1].items[0].search.contains('\n'));
        // History shows the SQL itself, cut at the end and searchable in full.
        let item = &groups[2].items[0];
        assert!(item.label.ends_with('…'));
        assert!(!item.label.contains('\n'));
        assert!(item.search.ends_with("FROM t"));
        // The full SQL is what gets opened.
        assert_eq!(item.action, PaletteAction::History(long));
    }
}
