//! UI tests: the real workspace in a headless window, driven by clicks and keys
//! through gpui-kit's test harness (`test-support`). No databases and nothing on
//! disk (`Data::in_memory`).
//!
//! Imports are listed one by one: `use gpui_kit::*` would bring GPUI's `test`
//! attribute along and shadow the built-in `#[test]`.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::{Root, Theme, ThemeMode};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{App, AppContext as _, Entity, TestAppContext, WindowHandle, px, size};
use tangul_data::Data;

use super::{Panel, Workspace};

/// Opens the workspace with the app's theme and key bindings.
fn open(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Workspace>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        crate::settings::init_defaults(cx);
        crate::bind_keys(cx);
    });
    let (engine, events) = tangul_engine::detached();
    let workspace = Rc::new(RefCell::new(None));
    let handle = cx.open_window(size(px(1280.), px(800.)), {
        let workspace = workspace.clone();
        move |window, cx| {
            let view = cx.new(|cx| Workspace::new(engine, events, Data::in_memory(), window, cx));
            *workspace.borrow_mut() = Some(view.clone());
            Root::new(view, window, cx)
        }
    });
    let workspace = workspace.borrow_mut().take().expect("workspace opened");
    (handle, workspace)
}

/// Whether the sidebars show `panel`. (Buttons don't report their selected state to
/// the harness, so this asks the workspace.)
fn shows(workspace: &Entity<Workspace>, panel: Panel, cx: &App) -> bool {
    let ws = workspace.read(cx);
    ws.left.shows(panel) || ws.right.shows(panel)
}

/// The status bar button of `panel`.
fn button(panel: Panel) -> (&'static str, usize) {
    ("panel", panel as usize)
}

const LEFT: (&str, usize) = ("sidebar", 0);
const RIGHT: (&str, usize) = ("sidebar", 1);

#[gpui_kit::test]
fn starts_with_the_schema_panel_on_the_left(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(LEFT).visible());
        assert!(shows(&workspace, Panel::Schema, cx));
        assert!(!shows(&workspace, Panel::History, cx));
        // Nothing lives on the right yet: no sidebar, no buttons.
        assert!(window.try_find(RIGHT).is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn panel_buttons_switch_and_hide_the_sidebar(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(button(Panel::History), cx);
        assert!(shows(&workspace, Panel::History, cx));
        assert!(!shows(&workspace, Panel::Schema, cx));
        assert!(window.find(LEFT).visible());

        // The shown panel's button hides the sidebar …
        window.click(button(Panel::History), cx);
        assert!(window.try_find(LEFT).is_none());
        assert!(!shows(&workspace, Panel::History, cx));

        // … and another one brings it back on that panel.
        window.click(button(Panel::Bookmarks), cx);
        assert!(window.find(LEFT).visible());
        assert!(shows(&workspace, Panel::Bookmarks, cx));
    })
    .unwrap();
}

#[gpui_kit::test]
fn cmd_b_toggles_the_left_sidebar_and_keeps_its_panel(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(button(Panel::Bookmarks), cx);
        window.press("cmd-b", cx);
        assert!(window.try_find(LEFT).is_none());
        window.press("cmd-b", cx);
        assert!(window.find(LEFT).visible());
        assert!(shows(&workspace, Panel::Bookmarks, cx));
    })
    .unwrap();
}
