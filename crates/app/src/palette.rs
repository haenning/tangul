//! Entries of the command palette (⌘⇧P): app commands, bookmarks and history in one
//! searchable list. The dialog itself is opened by the workspace.

use tangul_assist::suggest::one_line;

use tangul_data::local::{Bookmark, HistoryEntry};
use tangul_data::settings::{Appearance, ThemeSettings};
use tangul_data::themes::ThemeMode;

/// Length at which a history entry's SQL is cut in the list.
const LABEL_LEN: usize = 120;

#[derive(Debug, Clone, PartialEq)]
pub enum PaletteAction {
    Settings,
    /// Run everything in the active query tab's editor.
    RunAll,
    /// Format the active query tab's editor.
    Format,
    /// Bookmark id.
    Bookmark(String),
    /// SQL of the history entry.
    History(String),
    /// Which theme shows (`theme.appearance`).
    Appearance(Appearance),
    /// Use the theme of this name for its mode (`theme.dark` / `theme.light`).
    Theme {
        mode: ThemeMode,
        name: String,
    },
    /// Save the theme of this name as a file in the themes folder and open it.
    SaveTheme(String),
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

/// The commands that choose and save themes: `themes` as (name, mode), `chosen`
/// what the settings say (marked "current").
fn theme_commands(themes: &[(String, ThemeMode)], chosen: &ThemeSettings) -> Vec<PaletteItem> {
    let current = |yes: bool| if yes { "  (current)" } else { "" };
    let appearance = Appearance::ALL.map(|a| {
        let what = match a {
            Appearance::Auto => "follow the system",
            Appearance::Dark => "dark",
            Appearance::Light => "light",
        };
        PaletteItem {
            label: format!("Appearance: {what}{}", current(chosen.appearance == a)),
            search: "theme mode dark light auto system colors".into(),
            action: PaletteAction::Appearance(a),
        }
    });
    let choose = themes.iter().map(|(name, mode)| {
        let (kind, set) = match mode {
            ThemeMode::Dark => ("Dark", &chosen.dark),
            ThemeMode::Light => ("Light", &chosen.light),
        };
        PaletteItem {
            label: format!("{kind} theme: {name}{}", current(set == name)),
            search: "theme colors appearance".into(),
            action: PaletteAction::Theme {
                mode: *mode,
                name: name.clone(),
            },
        }
    });
    let save = themes.iter().map(|(name, _)| PaletteItem {
        label: format!("Save theme as a file: {name}"),
        search: "theme colors export edit json".into(),
        action: PaletteAction::SaveTheme(name.clone()),
    });
    appearance.into_iter().chain(choose).chain(save).collect()
}

/// Groups in display order; groups without entries are left out.
pub fn groups(
    bookmarks: &[Bookmark],
    history: &[HistoryEntry],
    themes: &[(String, ThemeMode)],
    chosen: &ThemeSettings,
) -> Vec<PaletteGroup> {
    let mut commands = vec![
        PaletteItem {
            label: "Open settings".into(),
            search: "preferences settings.json font size editor".into(),
            action: PaletteAction::Settings,
        },
        PaletteItem {
            label: "Run all statements  ⌘⇧↵".into(),
            search: "execute script whole editor".into(),
            action: PaletteAction::RunAll,
        },
        PaletteItem {
            label: "Format SQL  ⌥⌘L".into(),
            search: "format beautify indent pretty print".into(),
            action: PaletteAction::Format,
        },
    ];
    commands.extend(theme_commands(themes, chosen));
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
            ..Default::default()
        }
    }

    fn history(sql: &str) -> HistoryEntry {
        HistoryEntry {
            source: Default::default(),
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
            &[],
            &ThemeSettings::default(),
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
        let groups = groups(&[], &[], &[], &ThemeSettings::default());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].kind, PaletteKind::Command);
    }

    #[test]
    fn sql_is_searchable_and_shown_on_one_line() {
        let long = format!("SELECT {}\nFROM t", "x, ".repeat(100));
        let groups = groups(
            &[bookmark("b", "Report", &long)],
            &[history(&long)],
            &[],
            &ThemeSettings::default(),
        );
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

    #[test]
    fn themes_are_chosen_per_appearance_and_saved() {
        let themes = [
            ("Tangul Dark".to_string(), ThemeMode::Dark),
            ("Tangul Light".to_string(), ThemeMode::Light),
        ];
        let groups = groups(&[], &[], &themes, &ThemeSettings::default());
        let labels: Vec<_> = groups[0].items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"Appearance: follow the system  (current)"));
        assert!(labels.contains(&"Appearance: dark"));
        assert!(labels.contains(&"Dark theme: Tangul Dark  (current)"));
        assert!(labels.contains(&"Light theme: Tangul Light  (current)"));
        assert!(labels.contains(&"Save theme as a file: Tangul Light"));
        let pick = groups[0]
            .items
            .iter()
            .find(|i| i.label.starts_with("Light theme"))
            .unwrap();
        assert_eq!(
            pick.action,
            PaletteAction::Theme {
                mode: ThemeMode::Light,
                name: "Tangul Light".into()
            }
        );
    }
}
