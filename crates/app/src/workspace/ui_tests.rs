//! UI tests: the real workspace in a headless window, driven by clicks and keys
//! through gpui-kit's test harness (`test-support`). No databases and nothing on
//! disk (`Data::in_memory`).
//!
//! Imports are listed one by one: `use gpui_kit::*` would bring GPUI's `test`
//! attribute along and shadow the built-in `#[test]`.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::{Root, Theme, ThemeMode};
use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
use gpui_kit::{
    App, AppContext as _, Entity, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent,
    TestAppContext, Window, WindowHandle, px, size,
};
use tangul_core::{ColumnInfo, QueryId, RowBatch};
use tangul_data::Data;
use tangul_engine::{ExecEvent, QueryEvent};

use super::{Panel, Workspace};
use crate::query_tab::QueryTab;

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

/// Opens a query tab showing a finished result with columns `id` and `doc`.
fn tab_with_rows(
    handle: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
    rows: &[[Option<&str>; 2]],
    cx: &mut TestAppContext,
) -> Entity<QueryTab> {
    cx.update_window(handle.into(), |_, window, cx| {
        let tab = workspace.update(cx, |ws, cx| ws.add_tab("select id, doc from t", window, cx));
        tab.update(cx, |t, cx| {
            t.started(QueryId(1), "select id, doc from t", cx);
            let col = |n: &str| ColumnInfo { name: n.into() };
            t.on_query_event(
                QueryEvent::Exec(ExecEvent::ResultStart(vec![col("id"), col("doc")])),
                cx,
            );
            let mut batch = RowBatch::with_capacity(rows.len() * 2);
            for row in rows {
                for cell in row {
                    batch.push_cell(*cell);
                }
                batch.finish_row();
            }
            t.on_query_event(QueryEvent::Exec(ExecEvent::Rows(batch)), cx);
            t.on_query_event(
                QueryEvent::Finished {
                    elapsed: Default::default(),
                    error: None,
                    error_position: None,
                    in_transaction: false,
                    cancelled: false,
                },
                cx,
            );
        });
        window.render_frame(cx);
        tab
    })
    .unwrap()
}

/// A click on cell `col` of row `row` (from 0), with `modifiers` held.
fn click_cell(window: &mut Window, row: usize, col: usize, modifiers: Modifiers, cx: &mut App) {
    window.render_frame(cx);
    let position = window
        .within(("row", row))
        .find(("cell", col))
        .bounds()
        .center();
    window.dispatch_event(
        gpui_kit::PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
            first_mouse: false,
        }),
        cx,
    );
    window.dispatch_event(
        gpui_kit::PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
        }),
        cx,
    );
    window.render_frame(cx);
}

fn value_a(workspace: &Entity<Workspace>, cx: &App) -> Option<Option<String>> {
    let panel = workspace.read(cx).panels.value.read(cx);
    panel.a().map(|p| p.cell.value.clone())
}

fn changes(workspace: &Entity<Workspace>, cx: &App) -> usize {
    workspace.read(cx).panels.value.read(cx).changes()
}

fn value_b(workspace: &Entity<Workspace>, cx: &App) -> Option<Option<String>> {
    let panel = workspace.read(cx).panels.value.read(cx);
    panel.b().map(|p| p.cell.value.clone())
}

#[gpui_kit::test]
fn the_right_sidebar_opens_on_an_empty_value_viewer(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(RIGHT).is_none());
        window.press("alt-cmd-b", cx);
        assert!(window.find(RIGHT).visible());
        assert!(window.find("value-empty").visible());
        assert!(shows(&workspace, Panel::Value, cx));
        // Its status bar button hides it again; the left one stays.
        window.click(button(Panel::Value), cx);
        assert!(window.try_find(RIGHT).is_none());
        assert!(window.find(LEFT).visible());
    })
    .unwrap();
}

/// Runs `step` in the window, then lets the effects it queued (events between
/// entities, deferred updates) run before the next step.
fn step<R>(
    handle: WindowHandle<Root>,
    cx: &mut TestAppContext,
    step: impl FnOnce(&mut Window, &mut App) -> R,
) -> R {
    let out = cx
        .update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            step(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    out
}

#[gpui_kit::test]
fn space_shows_the_clicked_cell(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    let doc = r#"{"b": 1, "a": [1, 2]}"#;
    tab_with_rows(
        handle,
        &workspace,
        &[[Some("1"), Some(doc)], [Some("2"), None]],
        cx,
    );
    step(handle, cx, |window, cx| {
        click_cell(window, 0, 1, Modifiers::default(), cx)
    });
    step(handle, cx, |window, cx| {
        // A click picks A without opening the viewer …
        assert_eq!(value_a(&workspace, cx), Some(Some(doc.to_string())));
        assert!(window.try_find(RIGHT).is_none());
        // … Space opens it.
        window.press("space", cx);
    });
    step(handle, cx, |window, cx| {
        assert!(window.find(RIGHT).visible());
        assert!(window.find("value-panel").visible());
        // With only A, the B row says how to pick one.
        assert!(window.find("value-b-hint").visible());
        // The arrow keys move A.
        window.press("down", cx);
    });
    step(handle, cx, |_, cx| {
        assert_eq!(value_a(&workspace, cx), Some(None))
    });
}

#[gpui_kit::test]
fn alt_click_compares_and_the_x_stops_it(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    let rows = [
        [Some("1"), Some(r#"{"a": 1, "b": 2}"#)],
        [Some("2"), Some(r#"{"b": 2, "a": 1}"#)],
    ];
    tab_with_rows(handle, &workspace, &rows, cx);
    let alt = Modifiers {
        alt: true,
        ..Default::default()
    };
    step(handle, cx, |window, cx| {
        click_cell(window, 0, 1, Modifiers::default(), cx)
    });
    step(handle, cx, |window, cx| click_cell(window, 1, 1, alt, cx));
    step(handle, cx, |window, cx| {
        assert_eq!(
            value_b(&workspace, cx),
            Some(rows[1][1].map(str::to_string))
        );
        // ⌥-click keeps A and opens the viewer on the comparison.
        assert_eq!(
            value_a(&workspace, cx),
            Some(rows[0][1].map(str::to_string))
        );
        assert!(window.find(RIGHT).visible());
        assert!(window.find("compare-changes").visible());
        assert!(window.try_find("compare-columns-warning").is_none());
        assert!(window.try_find("value-b-hint").is_none());
        // Same JSON in another key order: identical while key order is ignored …
        assert_eq!(changes(&workspace, cx), 0);
        window.click("compare-normalize", cx);
    });
    step(handle, cx, |window, cx| {
        // … and not when it counts.
        assert!(changes(&workspace, cx) > 0);
        window.click("clear-compared", cx);
    });
    step(handle, cx, |window, cx| {
        assert_eq!(value_b(&workspace, cx), None);
        assert!(window.try_find("compare-changes").is_none());
    });
}

#[gpui_kit::test]
async fn a_long_hover_shows_the_value(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    tab_with_rows(
        handle,
        &workspace,
        &[[Some("1"), None], [Some(""), None]],
        cx,
    );
    // NULL and an empty string have nothing more to show: no card, however long
    // the hover.
    for (row, col) in [(0usize, 1usize), (1, 0)] {
        step(handle, cx, |window, cx| {
            window.within(("row", row)).hover(("cell", col), cx)
        });
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(3));
        step(handle, cx, |window, _| {
            assert!(window.try_find("peek-text").is_none())
        });
    }
    // Any value shows its card after the delay.
    step(handle, cx, |window, cx| {
        window.within(("row", 0usize)).hover(("cell", 0usize), cx)
    });
    cx.wait_for(
        handle.into(),
        std::time::Duration::from_secs(3),
        |window, _| window.try_find("peek-text").is_some(),
    )
    .await;
}

#[gpui_kit::test]
fn comparing_different_columns_warns(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    tab_with_rows(handle, &workspace, &[[Some("1"), Some("one")]], cx);
    let alt = Modifiers {
        alt: true,
        ..Default::default()
    };
    step(handle, cx, |window, cx| {
        click_cell(window, 0, 1, Modifiers::default(), cx)
    });
    step(handle, cx, |window, cx| click_cell(window, 0, 0, alt, cx));
    step(handle, cx, |window, _| {
        // Still compared: the user decides.
        assert!(window.find("compare-changes").visible());
        assert!(window.find("compare-columns-warning").visible());
    });
}

// Not tested here: the row menu on right click (the table in cell mode stops the
// click; see `render_td`). gpui-component's context menu keeps each opened menu
// alive through its own subscription, which GPUI's leak check at the end of a
// test reports. Checked once by hand-running such a test: the menu opens with the
// fix and not without it.
