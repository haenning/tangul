//! The menu bar (macOS; elsewhere `set_menus` shows nothing). Each item is an
//! action, so it shows the action's key binding and runs where the key would.

use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use gpui_kit::{Menu, MenuItem, OsAction, SystemMenuType};

use crate::{
    CloseTab, HideApp, HideOthers, NewDatabase, NewTab, OpenSettings, OpenSqliteFile, Quit, ShowAll,
};

pub fn menus() -> Vec<Menu> {
    vec![
        Menu::new("tangul").items([
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide tangul", HideApp),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit tangul", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Tab", NewTab),
            MenuItem::action("New Database…", NewDatabase),
            MenuItem::action("Open SQLite File…", OpenSqliteFile),
            MenuItem::separator(),
            MenuItem::action("Close Tab", CloseTab),
        ]),
        // A text field's own actions: only there do they apply.
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menus_are_there_in_order() {
        let names: Vec<_> = menus().iter().map(|m| m.name.to_string()).collect();
        assert_eq!(names, ["tangul", "File", "Edit"]);
        let file: Vec<_> = menus()[1]
            .items
            .iter()
            .filter_map(|i| match i {
                MenuItem::Action { name, .. } => Some(name.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            file,
            ["New Tab", "New Database…", "Open SQLite File…", "Close Tab"]
        );
    }
}
