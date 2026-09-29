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

/// Double-clicks cell `col` of row `row` (from 0).
fn double_click_cell(window: &mut Window, row: usize, col: usize, cx: &mut App) {
    window.render_frame(cx);
    window.within(("row", row)).double_click(("cell", col), cx);
    window.render_frame(cx);
}

fn viewer_open(workspace: &Entity<Workspace>, cx: &App) -> bool {
    shows(workspace, Panel::Value, cx)
}

#[gpui_kit::test]
fn a_click_selects_and_a_double_click_inspects(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    let rows = [
        [Some("1"), Some("one")],
        [Some("2"), Some("two")],
        [Some("3"), Some("three")],
    ];
    tab_with_rows(handle, &workspace, &rows, cx);
    step(handle, cx, |window, cx| double_click_cell(window, 0, 1, cx));
    step(handle, cx, |window, cx| {
        // A double click opens the viewer on the cell …
        assert!(viewer_open(&workspace, cx));
        assert_eq!(value_a(&workspace, cx), Some(Some("one".into())));
        click_cell(window, 1, 1, Modifiers::default(), cx);
    });
    step(handle, cx, |window, cx| {
        // … a click only selects …
        assert_eq!(value_a(&workspace, cx), Some(Some("one".into())));
        // … and so do the arrow keys.
        window.press("down", cx);
    });
    step(handle, cx, |window, cx| {
        assert_eq!(value_a(&workspace, cx), Some(Some("one".into())));
        // Space closes the open viewer, whatever is selected …
        window.press("space", cx);
    });
    step(handle, cx, |window, cx| {
        assert!(!viewer_open(&workspace, cx));
        assert_eq!(value_a(&workspace, cx), Some(Some("one".into())));
        // … and opens it on the selected cell.
        window.press("space", cx);
    });
    step(handle, cx, |window, cx| {
        assert!(viewer_open(&workspace, cx));
        assert_eq!(value_a(&workspace, cx), Some(Some("three".into())));
        double_click_cell(window, 1, 1, cx);
    });
    step(handle, cx, |_, cx| {
        assert_eq!(value_a(&workspace, cx), Some(Some("two".into())))
    });
}

#[gpui_kit::test]
fn space_opens_and_closes_the_viewer(cx: &mut TestAppContext) {
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
        // A click neither picks A nor opens the viewer …
        assert_eq!(value_a(&workspace, cx), None);
        assert!(window.try_find(RIGHT).is_none());
        // … Space does both.
        window.press("space", cx);
    });
    step(handle, cx, |window, cx| {
        assert!(window.find(RIGHT).visible());
        assert!(window.find("value-panel").visible());
        assert!(window.find("value-viewer-header").visible());
        assert_eq!(value_a(&workspace, cx), Some(Some(doc.to_string())));
        // With only A, the B row says how to pick one.
        assert!(window.find("value-b-hint").visible());
        // Space again closes it (like Quick Look), and opens it again.
        window.press("space", cx);
    });
    step(handle, cx, |window, cx| {
        assert!(!viewer_open(&workspace, cx));
        window.press("space", cx);
    });
    step(handle, cx, |window, cx| {
        assert!(viewer_open(&workspace, cx));
        // In the viewer's text too, where the editor would take Space as text.
        window.click("value-panel", cx);
    });
    step(handle, cx, |window, cx| {
        assert!(
            workspace
                .read(cx)
                .panels
                .value_focus
                .contains_focused(window, cx)
        );
        window.press("space", cx);
    });
    step(handle, cx, |window, cx| {
        assert!(!viewer_open(&workspace, cx));
        // The focus is back in the grid.
        window.press("space", cx);
    });
    step(handle, cx, |_, cx| assert!(viewer_open(&workspace, cx)));
}

#[gpui_kit::test]
fn the_x_and_esc_close_the_viewer(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    tab_with_rows(handle, &workspace, &[[Some("1"), Some("one")]], cx);
    step(handle, cx, |window, cx| double_click_cell(window, 0, 1, cx));
    step(handle, cx, |window, cx| {
        assert!(viewer_open(&workspace, cx));
        window.click("close-value-viewer", cx);
    });
    step(handle, cx, |window, cx| {
        assert!(!viewer_open(&workspace, cx));
        // Esc in the grid.
        double_click_cell(window, 0, 1, cx);
    });
    step(handle, cx, |window, cx| {
        assert!(viewer_open(&workspace, cx));
        window.press("escape", cx);
    });
    step(handle, cx, |window, cx| {
        assert!(!viewer_open(&workspace, cx));
        // Esc in the viewer.
        window.press("space", cx);
    });
    step(handle, cx, |window, cx| {
        assert!(viewer_open(&workspace, cx));
        window.click("value-viewer-header", cx);
    });
    step(handle, cx, |window, cx| {
        assert!(
            workspace
                .read(cx)
                .panels
                .value_focus
                .contains_focused(window, cx)
        );
        window.press("escape", cx);
    });
    step(handle, cx, |window, cx| {
        assert!(!viewer_open(&workspace, cx));
        // The focus is back in the grid: Space opens the viewer again.
        window.press("space", cx);
    });
    step(handle, cx, |_, cx| assert!(viewer_open(&workspace, cx)));
}

#[gpui_kit::test]
fn esc_closes_a_filter_popover_before_the_viewer(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    tab_with_rows(handle, &workspace, &[[Some("1"), Some("one")]], cx);
    step(handle, cx, |window, cx| double_click_cell(window, 0, 1, cx));
    step(handle, cx, |window, cx| {
        window.hover(("col-header", 1usize), cx)
    });
    step(handle, cx, |window, cx| {
        window.click(("col-filter-button", 1usize), cx)
    });
    step(handle, cx, |window, cx| {
        assert!(window.find("filter-apply").visible());
        window.press("escape", cx);
    });
    step(handle, cx, |window, cx| {
        assert!(window.try_find("filter-apply").is_none());
        assert!(viewer_open(&workspace, cx));
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
    step(handle, cx, |window, cx| double_click_cell(window, 0, 1, cx));
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
    step(handle, cx, |window, cx| double_click_cell(window, 0, 1, cx));
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

#[gpui_kit::test]
fn the_palette_opens_only_once(cx: &mut TestAppContext) {
    let (handle, _) = open(cx);
    step(handle, cx, |window, cx| window.press("cmd-shift-p", cx));
    step(handle, cx, |window, cx| {
        assert!(window.find("palette").visible());
        window.press("cmd-shift-p", cx);
    });
    // `find` fails on two elements with the same id.
    step(handle, cx, |window, _| {
        assert!(window.find("palette").visible())
    });
}

#[gpui_kit::test]
fn the_title_bar_shows_the_ai_user_before_yours(cx: &mut TestAppContext) {
    use tangul_core::{Database, DbUser, DriverKind, TlsMode};
    let (handle, workspace) = open(cx);
    let user = |id: &str| DbUser {
        id: id.into(),
        user: id.into(),
    };
    step(handle, cx, |_, cx| {
        workspace.update(cx, |ws, cx| {
            ws.data.profiles.databases.push(Database {
                id: "db".into(),
                name: "shop".into(),
                driver: DriverKind::Postgres,
                host: "localhost".into(),
                port: 5432,
                database: "shop".into(),
                tls: TlsMode::Disable,
                users: vec![user("app"), user("reader")],
                ssh: None,
                assistant_user: None,
                ai_values: Default::default(),
                ai_tools: true,
            });
            ws.active_db = Some("db".into());
            ws.active_user = Some("app".into());
            cx.notify();
        })
    });
    step(handle, cx, |window, cx| {
        let ai = window.find("ai-user-switcher").bounds();
        let yours = window.find("user-switcher").bounds();
        assert!(ai.origin.x < yours.origin.x, "the AI's user comes first");
        workspace.update(cx, |ws, cx| {
            ws.set_assistant_user("db", Some("reader".into()), window, cx);
            ws.set_ai_values("db", tangul_core::ValueAccess::Statistics, window, cx);
        });
    });
    step(handle, cx, |_, cx| {
        let db = workspace
            .read(cx)
            .data
            .profiles
            .database("db")
            .cloned()
            .unwrap();
        assert_eq!(db.assistant_user.as_deref(), Some("reader"));
        assert_eq!(db.ai_values, tangul_core::ValueAccess::Statistics);
        // The users stay as they were.
        assert_eq!(db.users.len(), 2);
    });
}

#[gpui_kit::test]
fn an_ai_user_that_may_write_turns_the_robot_red(cx: &mut TestAppContext) {
    use tangul_core::{Database, DbUser, DriverKind, TlsMode};
    let (handle, workspace) = open(cx);
    step(handle, cx, |_, cx| {
        workspace.update(cx, |ws, cx| {
            ws.data.profiles.databases.push(Database {
                id: "db".into(),
                name: "shop".into(),
                driver: DriverKind::Postgres,
                host: "localhost".into(),
                port: 5432,
                database: "shop".into(),
                tls: TlsMode::Disable,
                users: vec![DbUser {
                    id: "app".into(),
                    user: "app".into(),
                }],
                ssh: None,
                assistant_user: None,
                ai_values: Default::default(),
                ai_tools: true,
            });
            ws.active_db = Some("db".into());
            ws.active_user = Some("app".into());
            cx.notify();
        })
    });
    // Not checked yet: no warning, the note on your own user instead.
    step(handle, cx, |window, cx| {
        assert!(window.try_find("ai-user-warning").is_none());
        assert!(window.find("ai-user-note").visible());
        workspace.update(cx, |ws, cx| ws.set_ai_writable("db", 3, cx));
    });
    // One piece of advice: the red one replaces the note.
    step(handle, cx, |window, cx| {
        assert!(window.find("ai-user-warning").visible());
        assert!(window.try_find("ai-user-note").is_none());
        workspace.update(cx, |ws, cx| ws.set_ai_writable("db", 0, cx));
    });
    // Read-only on the server: no advice.
    step(handle, cx, |window, _| {
        assert!(window.try_find("ai-user-warning").is_none());
        assert!(window.try_find("ai-user-note").is_none());
    });
}

/// A database `shop` with the users `app` (yours) and `reader`, the AI's as given.
fn shop_with_ai_user(workspace: &Entity<Workspace>, ai: Option<&str>, cx: &mut App) {
    use tangul_core::{Database, DbUser, DriverKind, TlsMode};
    let user = |id: &str| DbUser {
        id: id.into(),
        user: id.into(),
    };
    workspace.update(cx, |ws, cx| {
        ws.data.profiles.databases.push(Database {
            id: "db".into(),
            name: "shop".into(),
            driver: DriverKind::Postgres,
            host: "localhost".into(),
            port: 5432,
            database: "shop".into(),
            tls: TlsMode::Disable,
            users: vec![user("app"), user("reader")],
            ssh: None,
            assistant_user: ai.map(Into::into),
            ai_values: Default::default(),
            ai_tools: true,
        });
        ws.active_db = Some("db".into());
        ws.active_user = Some("app".into());
        cx.notify();
    });
}

#[gpui_kit::test]
fn the_ai_on_your_own_user_is_allowed_with_a_note(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    step(handle, cx, |_, cx| shop_with_ai_user(&workspace, None, cx));
    step(handle, cx, |window, cx| {
        assert!(window.find("ai-user-note").visible());
        assert!(window.try_find("ai-user-warning").is_none());
        workspace.update(cx, |ws, cx| {
            ws.set_assistant_user("db", Some("reader".into()), window, cx)
        });
    });
    step(handle, cx, |window, _| {
        assert!(window.try_find("ai-user-note").is_none());
    });
}

#[gpui_kit::test]
fn a_missing_ai_user_is_shown_never_replaced_by_yours(cx: &mut TestAppContext) {
    use tangul_service::connections::Purpose;
    let (handle, workspace) = open(cx);
    step(handle, cx, |_, cx| {
        shop_with_ai_user(&workspace, Some("gone"), cx)
    });
    step(handle, cx, |window, cx| {
        assert!(window.find("ai-user-warning").visible());
        workspace.update(cx, |ws, _| {
            let ws = &mut *ws;
            let connected =
                ws.connections
                    .connect_assistant(&ws.engine, &mut ws.data.profiles, "app");
            assert!(connected.is_err());
            assert_eq!(ws.connections.get(Purpose::Assistant, "app"), None);
        });
    });
}

#[gpui_kit::test]
fn a_failed_ai_connection_is_shown_never_switched(cx: &mut TestAppContext) {
    use tangul_service::connections::Purpose;
    let (handle, workspace) = open(cx);
    step(handle, cx, |_, cx| {
        shop_with_ai_user(&workspace, Some("reader"), cx)
    });
    step(handle, cx, |window, cx| {
        assert!(window.try_find("ai-user-warning").is_none());
        workspace.update(cx, |ws, cx| {
            let conn = ws
                .connections
                .connect_assistant(&ws.engine, &mut ws.data.profiles, "app")
                .unwrap();
            ws.ai_connect_failed(conn, "password authentication failed", window, cx);
            assert_eq!(
                ws.ai_problems.get("db").map(String::as_str),
                Some("The AI could not connect as reader: password authentication failed")
            );
            // Forgotten, and not connected as your user instead.
            assert_eq!(ws.connections.get(Purpose::Assistant, "reader"), None);
            assert_eq!(ws.connections.get(Purpose::Assistant, "app"), None);
        });
    });
    step(handle, cx, |window, cx| {
        assert!(window.find("ai-user-warning").visible());
        // Connected as the AI's user again: nothing to report.
        workspace.update(cx, |ws, cx| {
            let conn = ws
                .connections
                .connect_assistant(&ws.engine, &mut ws.data.profiles, "app")
                .unwrap();
            ws.ai_connected(conn, cx);
        });
    });
    step(handle, cx, |window, _| {
        assert!(window.try_find("ai-user-warning").is_none());
    });
}

#[gpui_kit::test]
fn long_comments_show_three_lines_until_asked(cx: &mut TestAppContext) {
    use crate::ui::TextCard;
    cx.update(|cx| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
    });
    let card = |cx: &mut TestAppContext, text: String| {
        cx.open_window(size(px(400.), px(300.)), move |window, cx| {
            let view = TextCard::builder(text)(window, cx);
            Root::new(view, window, cx)
        })
    };
    let long = card(cx, "A comment someone wrote a lot into. ".repeat(12));
    step(long, cx, |window, cx| {
        assert!(window.find("text-card").visible());
        window.click("show-all", cx);
    });
    step(long, cx, |window, _| {
        assert!(window.try_find("show-all").is_none())
    });
    let short = card(cx, "Registered customers".into());
    step(short, cx, |window, _| {
        assert!(window.try_find("show-all").is_none())
    });
}

#[gpui_kit::test]
fn the_mcp_server_offers_the_connected_databases(cx: &mut TestAppContext) {
    use tangul_core::{Database, DbUser, DriverKind, TlsMode};
    let (handle, workspace) = open(cx);
    let mcp = |on: bool| {
        let mut s = crate::settings::Settings::default();
        s.mcp.enabled = on;
        // Any free port.
        s.mcp.port = 0;
        s
    };
    step(handle, cx, |window, cx| {
        assert!(window.find("mcp").visible());
        workspace.update(cx, |ws, cx| {
            ws.data.profiles.databases.push(Database {
                id: "db".into(),
                name: "shop".into(),
                driver: DriverKind::Postgres,
                host: "localhost".into(),
                port: 5432,
                database: "shop".into(),
                tls: TlsMode::Disable,
                users: vec![DbUser {
                    id: "app".into(),
                    user: "app".into(),
                }],
                ssh: None,
                assistant_user: None,
                ai_values: Default::default(),
                ai_tools: true,
            });
            ws.connect("app", cx);
            assert_eq!(ws.mcp_url(), None);
        });
        crate::settings::set_for_test(mcp(true), cx);
    });
    step(handle, cx, |_, cx| {
        workspace.update(cx, |ws, _| {
            let url = ws.mcp_url().expect("running");
            assert!(url.starts_with("http://127.0.0.1:") && url.ends_with("/mcp"));
            // For Claude Code in every project, and for Claude Desktop.
            let command = ws.mcp_command().unwrap();
            assert!(
                command.starts_with(&format!(
                    "claude mcp add --scope user --transport http tangul {url} --header \"Authorization: Bearer "
                )),
                "{command}"
            );
            let desktop: serde_json::Value =
                serde_json::from_str(&ws.mcp_desktop_config().unwrap()).unwrap();
            assert_eq!(desktop["tangul"]["command"], "npx");
            assert_eq!(desktop["tangul"]["args"][2], url.as_str());
            assert!(
                desktop["tangul"]["env"]["TANGUL_AUTH"]
                    .as_str()
                    .unwrap()
                    .starts_with("Bearer ")
            );
            assert_eq!(ws.mcp_offered(), ["shop"]);
            // Turned off for AI tools: no longer offered.
            ws.data.profiles.databases[0].ai_tools = false;
            ws.expose_to_mcp();
            assert!(ws.mcp_offered().is_empty());
        });
        crate::settings::set_for_test(mcp(false), cx);
    });
    step(handle, cx, |_, cx| {
        assert_eq!(workspace.read(cx).mcp_url(), None);
    });
}

#[gpui_kit::test]
fn a_read_only_ai_user_is_set_up_from_the_database_form(cx: &mut TestAppContext) {
    use tangul_core::{Database, DbUser, DriverKind, TlsMode};
    let (handle, workspace) = open(cx);
    step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.data.profiles.databases.push(Database {
                id: "db".into(),
                name: "shop".into(),
                driver: DriverKind::Postgres,
                host: "localhost".into(),
                port: 5432,
                database: "shop".into(),
                tls: TlsMode::Disable,
                users: vec![DbUser {
                    id: "app".into(),
                    user: "app".into(),
                }],
                ssh: None,
                assistant_user: None,
                ai_values: Default::default(),
                ai_tools: true,
            });
            ws.active_db = Some("db".into());
            ws.active_user = Some("app".into());
            ws.open_form(
                crate::connection_form::FormMode::EditDatabase,
                Some("db".into()),
                None,
                window,
                cx,
            );
        })
    });
    step(handle, cx, |window, cx| {
        window.click("set-up-ai-account", cx)
    });
    step(handle, cx, |window, cx| window.click("save", cx));
    step(handle, cx, |_, cx| {
        let ws = workspace.read(cx);
        let db = ws.data.profiles.database("db").unwrap().clone();
        let ai = db
            .users
            .iter()
            .find(|u| u.user == "tangul_ai")
            .expect("added");
        assert_eq!(db.assistant_user.as_deref(), Some(ai.id.as_str()));
        let tab = ws.tabs.active_query().expect("statements opened").clone();
        let t = tab.read(cx);
        assert_eq!(t.title.as_ref(), "Read-only AI user");
        let sql = t.sql(cx);
        assert!(
            sql.contains("CREATE ROLE tangul_ai LOGIN PASSWORD '"),
            "{sql}"
        );
        assert!(sql.contains("GRANT SELECT ON ALL TABLES IN SCHEMA public TO tangul_ai"));
        // Not run: nothing went to the engine for it.
        assert!(!t.is_running(cx));
    });
    // Its password is kept (in the session cache for in-memory profiles).
    workspace.update(cx, |ws, _| {
        let ai = ws.data.profiles.databases[0].users[1].id.clone();
        assert_eq!(ws.data.profiles.password(&ai).map(|p| p.len()), Some(24));
    });
}

#[gpui_kit::test]
fn comments_are_shown_as_the_tabs_database_reads_them(cx: &mut TestAppContext) {
    use tangul_core::{Database, DbUser, DriverKind, TlsMode};
    let (handle, workspace) = open(cx);
    let sql = "# select * from t\nselect 1 /* b /* c */";
    step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.data.profiles.databases.push(Database {
                id: "db".into(),
                name: "shop".into(),
                driver: DriverKind::Mysql,
                host: "localhost".into(),
                port: 3306,
                database: "shop".into(),
                tls: TlsMode::Disable,
                users: vec![DbUser {
                    id: "app".into(),
                    user: "app".into(),
                }],
                ssh: None,
                assistant_user: None,
                ai_values: Default::default(),
                ai_tools: true,
            });
            ws.connect("app", cx);
            ws.active_db = Some("db".into());
            ws.active_user = Some("app".into());
            ws.open_sql("Comments", sql, window, cx);
        })
    });
    step(handle, cx, |_, cx| {
        let ws = workspace.read(cx);
        let tab = ws.tabs.active_query().expect("opened").read(cx);
        // MySQL: `#` starts a comment, `/* */` doesn't nest.
        let comments: Vec<_> = tab.comments(cx).into_iter().map(|r| &sql[r]).collect();
        assert_eq!(comments, ["# select * from t", "/* b /* c */"]);
    });
}

#[gpui_kit::test]
fn a_proposal_opens_unrun_on_your_connection(cx: &mut TestAppContext) {
    use tangul_core::{Database, DbUser, DriverKind, TlsMode};
    let (handle, workspace) = open(cx);
    let sql =
        "-- Proposed by the assistant: fix the total\nUPDATE orders SET total = 0 WHERE id = 1";
    let sink = step(handle, cx, |_, cx| {
        workspace.update(cx, |ws, cx| {
            ws.data.profiles.databases.push(Database {
                id: "db".into(),
                name: "shop".into(),
                driver: DriverKind::Postgres,
                host: "localhost".into(),
                port: 5432,
                database: "shop".into(),
                tls: TlsMode::Disable,
                users: vec![DbUser {
                    id: "app".into(),
                    user: "app".into(),
                }],
                ssh: None,
                assistant_user: None,
                ai_values: Default::default(),
                ai_tools: true,
            });
            ws.connect("app", cx);
            ws.mcp_proposals(cx)
        })
    });
    // As the MCP server does (from its thread in the app; the test scheduler wants
    // this one).
    sink(tangul_ai::SqlProposal {
        connection: "shop".into(),
        user: "app".into(),
        sql: sql.into(),
        chart: None,
    });
    // A query to look at as a chart: set before it runs.
    sink(tangul_ai::SqlProposal {
        connection: "shop".into(),
        user: "app".into(),
        sql: "SELECT day, orders FROM daily".into(),
        chart: Some(tangul_core::chart::ChartStyle::Bars),
    });
    cx.run_until_parked();
    step(handle, cx, |_, cx| {
        let ws = workspace.read(cx);
        let tab = ws.tabs.active_query().expect("opened").read(cx);
        assert_eq!(tab.sql(cx), "SELECT day, orders FROM daily");
        assert!(!tab.is_running(cx), "not run");
        assert_eq!(
            tab.chart_view(cx),
            Some(tangul_core::chart::ChartView {
                style: tangul_core::chart::ChartStyle::Bars,
                series: None
            })
        );
    });
    // The first proposal's tab, before it.
    step(handle, cx, |_, cx| {
        workspace.update(cx, |ws, _| {
            let last = ws.tabs.active_index();
            ws.tabs.set_active(last - 1);
        })
    });
    step(handle, cx, |_, cx| {
        let ws = workspace.read(cx);
        let tab = ws.tabs.active_query().expect("opened").read(cx);
        assert_eq!(tab.title.as_ref(), "Proposed · shop");
        assert_eq!(tab.sql(cx), sql);
        assert!(!tab.is_running(cx), "not run");
        let conn = tab.conn.as_ref().expect("on a connection");
        assert_eq!(conn.entity_id(), ws.tab_conn("app").unwrap().entity_id());
    });
}

#[gpui_kit::test]
fn history_shows_everyone_s_queries_with_who_ran_them(cx: &mut TestAppContext) {
    use tangul_core::{Database, DbUser, DriverKind, TlsMode};
    use tangul_data::local::{HistoryEntry, NewHistory, Source};
    let (handle, workspace) = open(cx);
    step(handle, cx, |_, cx| {
        workspace.update(cx, |ws, cx| {
            ws.data.profiles.databases.push(Database {
                id: "db".into(),
                name: "shop".into(),
                driver: DriverKind::Postgres,
                host: "localhost".into(),
                port: 5432,
                database: "shop".into(),
                tls: TlsMode::Disable,
                users: vec![DbUser {
                    id: "app".into(),
                    user: "app".into(),
                }],
                ssh: None,
                assistant_user: None,
                ai_values: Default::default(),
                ai_tools: true,
            });
            ws.active_db = Some("db".into());
            ws.active_user = Some("app".into());
            ws.connect("app", cx);
        });
        let mut s = crate::settings::Settings::default();
        s.mcp.enabled = true;
        s.mcp.port = 0;
        crate::settings::set_for_test(s, cx);
    });
    step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.data
                .local
                .add_history(&NewHistory {
                    database_id: "db",
                    user_id: "app",
                    sql: "SELECT 1",
                    fingerprint: "one",
                    entry: HistoryEntry {
                        sql: "SELECT 1".into(),
                        source: Source::App,
                        started_at: 1,
                        duration_ms: 1,
                        rows: Some(1),
                        error: None,
                    },
                })
                .unwrap();
            // What the server hands over after a client's run_readonly.
            ws.record_mcp_call(
                tangul_ai::ToolCall {
                    at: 100,
                    caller: "mcp",
                    tool: "run_readonly".into(),
                    connection: Some("shop".into()),
                    input: String::new(),
                    sql: Some("SELECT count(*) FROM orders".into()),
                    rows: Some(1),
                    outcome: "ran",
                    error: None,
                    duration_ms: 7,
                },
                cx,
            );
        });
        window.click(button(Panel::History), cx);
    });
    let shown = |cx: &mut TestAppContext| -> Vec<(Source, String)> {
        workspace.read_with(cx, |ws, _| {
            ws.panels
                .history
                .entries
                .iter()
                .map(|e| (e.source, e.sql.clone()))
                .collect()
        })
    };
    // None chosen: everyone's, each with who ran it.
    assert_eq!(
        shown(cx),
        [
            (Source::Mcp, "SELECT count(*) FROM orders".to_string()),
            (Source::App, "SELECT 1".into())
        ]
    );
    step(handle, cx, |window, cx| {
        assert!(window.find(("history-source-icon", 0usize)).visible());
        assert!(window.find(("history-source-icon", 1usize)).visible());
        window.click(("history-source", Source::Mcp as usize), cx);
    });
    assert_eq!(
        shown(cx),
        [(Source::Mcp, "SELECT count(*) FROM orders".to_string())]
    );
    step(handle, cx, |window, cx| {
        // Filtered, the row still says who ran it.
        assert!(window.find(("history-source-icon", 0usize)).visible());
        window.click(("history-source", Source::App as usize), cx);
    });
    assert_eq!(shown(cx).len(), 2, "app and mcp chosen");
    step(handle, cx, |window, cx| {
        window.click(("history-source", Source::Mcp as usize), cx);
        window.click(("history-source", Source::App as usize), cx);
    });
    assert_eq!(shown(cx).len(), 2, "none chosen again");
    step(handle, cx, |window, cx| {
        window.click(("history-source", Source::Ai as usize), cx)
    });
    assert!(shown(cx).is_empty(), "the assistant ran nothing yet");
}

#[gpui_kit::test]
fn hovering_a_number_column_shows_its_summary(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    tab_with_rows(
        handle,
        &workspace,
        &[[Some("1"), Some("a")], [Some("2"), None]],
        cx,
    );
    step(handle, cx, |window, cx| {
        window.hover(("col-header", 0usize), cx)
    });
    step(handle, cx, |window, _| {
        assert!(window.find("column-stats").visible())
    });
}

#[gpui_kit::test]
fn the_summary_comes_back_after_the_filter_closes(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    tab_with_rows(
        handle,
        &workspace,
        &[[Some("1"), Some("a")], [Some("2"), None]],
        cx,
    );
    step(handle, cx, |window, cx| {
        window.hover(("col-header", 0usize), cx)
    });
    step(handle, cx, |window, cx| {
        window.click(("col-filter-button", 0usize), cx)
    });
    step(handle, cx, |window, cx| {
        assert!(window.find("filter-apply").visible());
        window.press("escape", cx);
    });
    step(handle, cx, |window, cx| {
        window.hover(("col-header", 1usize), cx)
    });
    step(handle, cx, |window, cx| {
        window.hover(("col-header", 0usize), cx)
    });
    step(handle, cx, |window, _| {
        assert!(window.try_find("filter-apply").is_none());
        assert!(window.find("column-stats").visible())
    });
}

#[gpui_kit::test]
fn a_text_column_shows_its_counts(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    tab_with_rows(
        handle,
        &workspace,
        &[[Some("1"), Some("a")], [Some("2"), None]],
        cx,
    );
    step(handle, cx, |window, cx| {
        window.hover(("col-header", 1usize), cx)
    });
    step(handle, cx, |window, _| {
        assert!(window.find("column-stats").visible())
    });
}
