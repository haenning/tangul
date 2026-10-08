//! UI tests: the real workspace in a headless window, driven by clicks and keys
//! through gpui-kit's test harness (`test-support`). No databases and nothing on
//! disk (`Data::in_memory`).
//!
//! Imports are listed one by one: `use gpui_kit::*` would bring GPUI's `test`
//! attribute along and shadow the built-in `#[test]`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

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
        crate::syntax::init();
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
    tab_with_result(handle, workspace, rows, false, cx)
}

/// Like [`tab_with_rows`]; `truncated`: the engine stopped at its row limit.
fn tab_with_result(
    handle: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
    rows: &[[Option<&str>; 2]],
    truncated: bool,
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
            if truncated {
                t.on_query_event(QueryEvent::Exec(ExecEvent::Truncated), cx);
            }
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
        cx.executor().advance_clock(Duration::from_secs(3));
        step(handle, cx, |window, _| {
            assert!(window.try_find("peek-text").is_none())
        });
    }
    // Any value shows its card after the delay.
    step(handle, cx, |window, cx| {
        window.within(("row", 0usize)).hover(("cell", 0usize), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
        window.try_find("peek-text").is_some()
    })
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
                ai_access: Default::default(),
                ai_tools: true,
                editor_values: true,
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
                ai_access: Default::default(),
                ai_tools: true,
                editor_values: true,
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
    step(handle, cx, |window, _| {
        assert!(window.find("ai-user-warning").visible());
        assert!(window.try_find("ai-user-note").is_none());
    });
    // The menu: the warning as a card on top, sections with an (i).
    step(handle, cx, |window, cx| {
        window.click("ai-user-switcher", cx)
    });
    step(handle, cx, |window, cx| {
        assert!(window.find("menu-alert").visible());
        assert!(window.try_find("info-AI connects as").is_some());
        assert!(window.try_find("info-Column values for the AI").is_some());
        window.press("escape", cx);
    });
    step(handle, cx, |_, cx| {
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
            ai_access: Default::default(),
            ai_tools: true,
            editor_values: true,
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
                ai_access: Default::default(),
                ai_tools: true,
                editor_values: true,
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
            assert_eq!(ws.ai_offered(), ["shop"]);
        });
    });
    // Bookmarks are offered as they are saved and deleted, passwords redacted.
    step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| {
            let bookmarks = |ws: &Workspace| ws.ai_offered_bookmarks();
            assert!(bookmarks(ws).is_empty());
            ws.save_bookmark(
                tangul_data::local::Bookmark {
                    id: "b1".into(),
                    name: "Reader".into(),
                    sql: "CREATE ROLE r LOGIN PASSWORD 'hunter2'".into(),
                    database_id: Some("db".into()),
                    ..Default::default()
                },
                window,
                cx,
            );
            assert_eq!(
                bookmarks(ws),
                [tangul_ai::SharedBookmark {
                    name: "Reader".into(),
                    sql: "CREATE ROLE r LOGIN PASSWORD '***'".into(),
                }]
            );
            ws.delete_bookmark("b1", window, cx);
            assert!(bookmarks(ws).is_empty());
        });
    });
    step(handle, cx, |_, cx| {
        workspace.update(cx, |ws, _| {
            // Turned off for AI tools: no longer offered.
            ws.data.profiles.databases[0].ai_tools = false;
            ws.expose_to_ai();
            assert!(ws.ai_offered().is_empty());
        });
        crate::settings::set_for_test(mcp(false), cx);
    });
    step(handle, cx, |_, cx| {
        assert_eq!(workspace.read(cx).mcp_url(), None);
    });
}

#[gpui_kit::test]
fn the_ai_settings_open_on_mcp_from_the_status_bar(cx: &mut TestAppContext) {
    use crate::ai_settings::Page;
    let (handle, workspace) = open(cx);
    step(handle, cx, |_, cx| {
        workspace.update(cx, |ws, cx| {
            shop_for_the_assistant(ws, cx);
            ws.active_db = Some("db".into());
            ws.active_user = Some("app".into());
        });
        let mut s = crate::settings::Settings::default();
        s.mcp.enabled = true;
        s.mcp.port = 0;
        crate::settings::set_for_test(s, cx);
    });
    let settings_tab = |ws: &Workspace| ws.tabs.active().and_then(|t| t.ai_settings()).cloned();
    // The plug opens the MCP page.
    step(handle, cx, |window, cx| window.click("mcp", cx));
    step(handle, cx, |window, cx| {
        let tab = settings_tab(workspace.read(cx)).expect("the settings tab");
        assert_eq!(tab.read(cx).page(), Page::Mcp);
        assert!(window.find("mcp-enabled").visible());
        window.click("mcp-copy-command", cx);
    });
    step(handle, cx, |_, cx| {
        let copied = cx
            .read_from_clipboard()
            .and_then(|c| c.text())
            .unwrap_or_default();
        assert!(copied.starts_with("claude mcp add"), "{copied}");
    });
    // The database's AI settings are here too.
    step(handle, cx, |_, cx| {
        assert_eq!(workspace.read(cx).ai_offered(), ["shop"]);
    });
    step(handle, cx, |window, cx| {
        // Below the server's sections.
        let down = gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-2000.)));
        window.scroll("ai-settings-body", down, cx);
    });
    step(handle, cx, |window, cx| {
        window.click(("ai-tools", 0usize), cx)
    });
    step(handle, cx, |_, cx| {
        let ws = workspace.read(cx);
        assert!(!ws.data.profiles.databases[0].ai_tools);
        assert!(ws.ai_offered().is_empty());
    });
    // The title bar's AI menu shows the same tab, on the assistant's page.
    step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.open_ai_settings(Page::Assistant, window, cx)
        })
    });
    step(handle, cx, |window, cx| {
        let ws = workspace.read(cx);
        let tabs = ws
            .tabs
            .items()
            .filter(|t| t.ai_settings().is_some())
            .count();
        assert_eq!(tabs, 1);
        assert_eq!(settings_tab(ws).unwrap().read(cx).page(), Page::Assistant);
        assert!(window.find("ai-provider-anthropic").visible());
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
                ai_access: Default::default(),
                ai_tools: true,
                editor_values: true,
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
fn the_database_form_offers_every_database_and_saves_the_choices(cx: &mut TestAppContext) {
    use crate::connection_form::{FormMode, OFFERED};
    use tangul_core::{AiAccess, DriverKind};
    let (handle, workspace) = open(cx);
    step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.open_form(FormMode::NewDatabase, None, None, window, cx)
        })
    });
    step(handle, cx, |window, _| {
        for ix in 0..OFFERED.len() {
            assert!(window.find(("driver", ix)).visible());
        }
        // Off until turned on.
        assert!(window.try_find(("ssh-auth", 0usize)).is_none());
    });
    let mysql = OFFERED
        .iter()
        .position(|d| *d == DriverKind::Mysql)
        .unwrap();
    step(handle, cx, |window, cx| window.click(("driver", mysql), cx));
    step(handle, cx, |window, cx| window.click("use-ssh", cx));
    step(handle, cx, |window, _| {
        assert!(window.find(("ssh-auth", 0usize)).visible())
    });
    step(handle, cx, |window, cx| window.click("use-ssh", cx));
    let schema = AiAccess::ALL
        .iter()
        .position(|a| *a == AiAccess::Schema)
        .unwrap();
    step(handle, cx, |window, cx| {
        window.click(("ai-access", schema), cx)
    });
    step(handle, cx, |window, _| {
        // Column values only matter where the AI reads data.
        assert!(window.try_find(("ai-values", 0usize)).is_none())
    });
    step(handle, cx, |window, cx| window.click("form-user", cx));
    step(handle, cx, |window, cx| window.input("app", cx));
    step(handle, cx, |window, cx| window.click("save", cx));
    step(handle, cx, |_, cx| {
        let ws = workspace.read(cx);
        let db = ws.data.profiles.databases.last().expect("saved");
        assert_eq!(db.driver, DriverKind::Mysql);
        // The port follows the database while it is the default.
        assert_eq!(db.port, 3306);
        assert_eq!(db.ai_access, AiAccess::Schema);
        assert!(db.ssh.is_none());
        assert_eq!(db.users[0].user, "app");
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
                ai_access: Default::default(),
                ai_tools: true,
                editor_values: true,
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

/// A connected Postgres database `shop` with user `app`, offered to the assistant.
fn shop_for_the_assistant(ws: &mut Workspace, cx: &mut gpui_kit::Context<Workspace>) {
    use tangul_core::{Database, DbUser, DriverKind, TlsMode};
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
        ai_access: Default::default(),
        ai_tools: true,
        editor_values: true,
    });
    ws.connect("app", cx);
}

#[gpui_kit::test]
fn a_proposal_opens_unrun_on_your_connection(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    let sql =
        "-- Proposed by the assistant: fix the total\nUPDATE orders SET total = 0 WHERE id = 1";
    let sink = step(handle, cx, |_, cx| {
        workspace.update(cx, |ws, cx| {
            shop_for_the_assistant(ws, cx);
            ws.ai_proposals(cx)
        })
    });
    // As the MCP server does (from its thread in the app; the test scheduler wants
    // this one).
    sink(tangul_ai::SqlProposal {
        connection: "shop".into(),
        user: "app".into(),
        sql: sql.into(),
        title: None,
    });
    cx.run_until_parked();
    step(handle, cx, |_, cx| {
        let ws = workspace.read(cx);
        let tab = ws.tabs.active_query().expect("opened").read(cx);
        assert_eq!(tab.title.as_ref(), "Proposed · shop");
        assert_eq!(tab.sql(cx), sql);
        assert!(!tab.is_running(cx), "not run");
        assert_eq!(tab.row_count(cx), None, "no result");
        let conn = tab.conn.as_ref().expect("on a connection");
        assert_eq!(conn.entity_id(), ws.tab_conn("app").unwrap().entity_id());
    });
}

#[gpui_kit::test]
fn a_shown_result_opens_with_its_chart_and_isnt_run_again(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    let sink = step(handle, cx, |_, cx| {
        workspace.update(cx, |ws, cx| {
            shop_for_the_assistant(ws, cx);
            ws.ai_shows(cx)
        })
    });
    let sql = "SELECT day, orders, revenue FROM daily";
    let col = |n: &str| ColumnInfo { name: n.into() };
    let mut result = tangul_core::ResultSet::new(vec![col("day"), col("orders"), col("revenue")]);
    let mut batch = RowBatch::with_capacity(6);
    for row in [["2024-01-01", "3", "10"], ["2024-01-02", "4", "12"]] {
        for cell in row {
            batch.push_cell(Some(cell));
        }
        batch.finish_row();
    }
    result.append(batch);
    // A chart of one column, by name; and the grid, untitled.
    sink(tangul_ai::ShownResult {
        connection: "shop".into(),
        user: "app".into(),
        sql: sql.into(),
        title: Some("Revenue per day".into()),
        note: Some("last two days".into()),
        chart: Some(tangul_ai::ChartRequest {
            style: tangul_core::chart::ChartStyle::Bars,
            column: Some("revenue".into()),
        }),
        result,
        truncated: false,
        elapsed: Duration::from_millis(12),
    });
    sink(tangul_ai::ShownResult {
        connection: "shop".into(),
        user: "app".into(),
        sql: "SELECT 1 AS n".into(),
        title: None,
        note: None,
        chart: None,
        result: tangul_core::ResultSet::new(vec![col("n")]),
        truncated: false,
        elapsed: Duration::ZERO,
    });
    cx.run_until_parked();
    step(handle, cx, |_, cx| {
        let ws = workspace.read(cx);
        let tab = ws.tabs.active_query().expect("opened").read(cx);
        assert_eq!(tab.title.as_ref(), "Shown · shop");
        assert_eq!(tab.sql(cx), "-- Shown by the assistant\nSELECT 1 AS n");
        assert_eq!(tab.chart_view(cx), None);
        assert_eq!(tab.row_count(cx), Some(0));
    });
    step(handle, cx, |_, cx| {
        workspace.update(cx, |ws, _| {
            let last = ws.tabs.active_index();
            ws.tabs.set_active(last - 1);
        })
    });
    step(handle, cx, |_, cx| {
        let ws = workspace.read(cx);
        let tab = ws.tabs.active_query().expect("opened").read(cx);
        assert_eq!(tab.title.as_ref(), "Revenue per day");
        assert_eq!(
            tab.sql(cx),
            format!("-- Shown by the assistant: last two days\n{sql}")
        );
        // The result is there, not running: nothing was sent to the database.
        assert!(!tab.is_running(cx));
        assert_eq!(tab.result_columns(cx), ["day", "orders", "revenue"]);
        assert_eq!(tab.row_count(cx), Some(2));
        assert_eq!(
            tab.chart_view(cx),
            Some(tangul_core::chart::ChartView {
                style: tangul_core::chart::ChartStyle::Bars,
                series: Some(1),
            })
        );
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
                ai_access: Default::default(),
                ai_tools: true,
                editor_values: true,
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
            ws.record_tool_call(
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

#[gpui_kit::test]
fn a_large_result_gets_a_chart_that_says_it_is_cut(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    // More rows than bars used to allow.
    let labels: Vec<String> = (0..200).map(|i| format!("r{i}")).collect();
    let rows: Vec<[Option<&str>; 2]> = labels
        .iter()
        .map(|l| [Some(l.as_str()), Some("1")])
        .collect();
    let tab = tab_with_result(handle, &workspace, &rows, true, cx);
    step(handle, cx, |_, cx| {
        assert!(tab.read(cx).chart(cx).is_ok());
        tab.update(cx, |t, cx| {
            t.set_chart_view(
                Some(tangul_core::chart::ChartView {
                    style: tangul_core::chart::ChartStyle::Bars,
                    series: Some(0),
                }),
                cx,
            )
        });
    });
    step(handle, cx, |window, _| {
        assert!(window.find("chart-truncated").visible())
    });
}

#[gpui_kit::test]
fn running_formats_the_query_first(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    let tab = step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.add_tab("select id, name from users where id = 1", window, cx)
        })
    });
    step(handle, cx, |window, cx| {
        tab.update(cx, |t, cx| t.run_all(&crate::RunAll, window, cx))
    });
    let formatted = "select\n  id,\n  name\nfrom\n  users\nwhere\n  id = 1";
    step(handle, cx, |window, cx| {
        assert_eq!(tab.read(cx).sql(cx), formatted);
        // Formatted already: a second run leaves the text as it is.
        tab.update(cx, |t, cx| t.run_all(&crate::RunAll, window, cx))
    });
    step(handle, cx, |_, cx| {
        assert_eq!(tab.read(cx).sql(cx), formatted)
    });
}

#[gpui_kit::test]
fn format_formats_without_running(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    let tab = step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| ws.add_tab("select a, b from t", window, cx))
    });
    step(handle, cx, |window, cx| {
        tab.update(cx, |t, cx| t.format(&crate::FormatQuery, window, cx))
    });
    step(handle, cx, |_, cx| {
        let t = tab.read(cx);
        assert_eq!(t.sql(cx), "select\n  a,\n  b\nfrom\n  t");
        assert!(!t.is_running(cx));
    });
}

#[gpui_kit::test]
fn a_chosen_quote_pair_puts_the_cursor_between_the_quotes(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    let sql = "select * from users where email = ";
    let tab = step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| ws.add_tab(sql, window, cx))
    });
    let items = vec![("''".to_string(), 1), ("'%%'".to_string(), 2)];
    step(handle, cx, |window, cx| {
        QueryTab::insert_item(&tab, "''", items.clone(), window, cx)
    });
    step(handle, cx, |_, cx| {
        assert_eq!(tab.read(cx).sql(cx), format!("{sql}''"));
        assert_eq!(tab.read(cx).cursor(cx), sql.len() + 1);
    });
    // Other inserts (and the same text once the menu is gone) leave the cursor at
    // their end.
    step(handle, cx, |window, cx| {
        QueryTab::insert_item(&tab, " and name = ''", Vec::new(), window, cx)
    });
    step(handle, cx, |_, cx| {
        let t = tab.read(cx);
        assert_eq!(t.cursor(cx), t.sql(cx).len());
    });
}

/// A model that answers every question with the same text.
struct Echo(String);

#[async_trait::async_trait]
impl tangul_ai::llm::Provider for Echo {
    async fn chat(
        &self,
        _: &tangul_ai::llm::Request,
        on: &mut (dyn FnMut(tangul_ai::llm::Delta) + Send),
    ) -> Result<tangul_ai::llm::Reply, tangul_ai::llm::Error> {
        on(tangul_ai::llm::Delta::Text(self.0.clone()));
        Ok(tangul_ai::llm::Reply {
            message: tangul_ai::llm::Message::assistant(vec![tangul_ai::llm::Part::Text {
                text: self.0.clone(),
            }]),
            ..Default::default()
        })
    }

    async fn models(&self) -> Result<Vec<String>, tangul_ai::llm::Error> {
        Ok(Vec::new())
    }
}

/// Runs the window until `done` holds: the assistant answers on its own runtime.
fn wait_for(cx: &mut TestAppContext, mut done: impl FnMut(&mut TestAppContext) -> bool) {
    for _ in 0..500 {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("not in time");
}

#[gpui_kit::test]
fn the_assistant_answers_in_the_right_sidebar_and_keeps_the_chat(cx: &mut TestAppContext) {
    use tangul_ai::llm::ProviderKind;
    let (handle, workspace) = open(cx);
    // No provider yet: it says so, and the question stays.
    step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.ask_assistant("which orders are open?".into(), true, window, cx)
        })
    });
    step(handle, cx, |window, cx| {
        assert!(shows(&workspace, Panel::Assistant, cx));
        assert!(window.find("assistant-problem").visible());
        assert!(window.find("assistant-set-up").visible());
        let panel = workspace.read(cx).panels.assistant.clone();
        assert!(panel.read(cx).messages().is_empty());
    });
    // With a model: the answer comes, its SQL opens in a tab.
    step(handle, cx, |_, cx| {
        workspace.update(cx, |ws, cx| {
            shop_for_the_assistant(ws, cx);
            ws.active_db = Some("db".into());
            ws.active_user = Some("app".into());
            ws.assistant.provider = Some(Arc::new(Echo(
                "Like this:\n```sql\nSELECT id FROM orders WHERE status = 'open'\n```\n".into(),
            )));
        });
        let mut s = crate::settings::Settings::default();
        s.ai.provider = Some(ProviderKind::Ollama);
        s.ai.providers.insert(
            ProviderKind::Ollama,
            crate::settings::ProviderSettings {
                model: Some("m".into()),
                base_url: None,
            },
        );
        crate::settings::set_for_test(s, cx);
    });
    step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.ask_assistant("which orders are open?".into(), false, window, cx)
        })
    });
    wait_for(cx, |cx| {
        workspace.read_with(cx, |ws, cx| {
            let panel = ws.panels.assistant.read(cx);
            !panel.is_running() && panel.messages().len() == 2
        })
    });
    step(handle, cx, |window, cx| {
        let ws = workspace.read(cx);
        // Kept in local.db, as the list shows it.
        let chats = ws.data.local.chats(10);
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].title, "which orders are open?");
        assert_eq!(chats[0].database_id.as_deref(), Some("db"));
        assert_eq!(ws.data.local.chat_messages(chats[0].id).len(), 2);
        assert!(window.try_find("assistant-problem").is_none());
        // The answer's text, then its SQL block (message 1, part 0, segment 1).
        window.click(("sql-open", 100_001usize), cx);
    });
    step(handle, cx, |_, cx| {
        let ws = workspace.read(cx);
        let tab = ws.tabs.active_query().expect("opened").read(cx);
        assert_eq!(tab.sql(cx), "SELECT id FROM orders WHERE status = 'open'");
        assert!(!tab.is_running(cx), "not run");
    });
}

#[gpui_kit::test]
fn a_question_run_in_the_editor_goes_to_the_assistant_once_it_is_set_up(cx: &mut TestAppContext) {
    use tangul_ai::llm::ProviderKind;
    let (handle, workspace) = open(cx);
    let question = "-- asked by Ann\nwhich customers ordered twice last month?";
    let run = |text: &'static str, cx: &mut TestAppContext| {
        step(handle, cx, |window, cx| {
            workspace.update(cx, |ws, cx| {
                ws.open_sql("Q", text, window, cx);
                let tab = ws.tabs.active_query().unwrap().clone();
                QueryTab::focus_editor(&tab, window, cx);
            });
        });
        step(handle, cx, |window, cx| {
            window.dispatch_action(Box::new(crate::RunQuery), cx)
        });
    };
    let running = |cx: &mut TestAppContext| {
        workspace.read_with(cx, |ws, cx| {
            ws.tabs.active_query().unwrap().read(cx).is_running(cx)
        })
    };
    step(handle, cx, |_, cx| {
        workspace.update(cx, |ws, cx| {
            shop_for_the_assistant(ws, cx);
            ws.active_db = Some("db".into());
            ws.active_user = Some("app".into());
        })
    });
    // No AI set up for the chat: everything runs as SQL, as before.
    run(question, cx);
    assert!(running(cx));
    step(handle, cx, |_, cx| {
        assert!(!shows(&workspace, Panel::Assistant, cx));
    });
    // Set up: a question goes to the assistant, not the database (its comment
    // doesn't count).
    step(handle, cx, |_, cx| {
        workspace.update(cx, |ws, _| {
            ws.assistant.provider = Some(Arc::new(Echo("Here you go.".into())));
        });
        let mut s = crate::settings::Settings::default();
        s.ai.provider = Some(ProviderKind::Ollama);
        s.ai.providers.insert(
            ProviderKind::Ollama,
            crate::settings::ProviderSettings {
                model: Some("m".into()),
                base_url: None,
            },
        );
        crate::settings::set_for_test(s, cx);
    });
    run(question, cx);
    assert!(!running(cx));
    wait_for(cx, |cx| {
        workspace.read_with(cx, |ws, cx| {
            let panel = ws.panels.assistant.read(cx);
            !panel.is_running() && panel.messages().len() == 2
        })
    });
    step(handle, cx, |_, cx| {
        assert!(shows(&workspace, Panel::Assistant, cx));
        let ws = workspace.read(cx);
        let asked = ws.panels.assistant.read(cx).messages()[0].text();
        assert_eq!(asked, question);
    });
    // SQL runs as always.
    run("select id from orders", cx);
    assert!(running(cx));
}

#[gpui_kit::test]
fn tool_calls_show_a_few_then_pages_of_25(cx: &mut TestAppContext) {
    use crate::ai_settings::{CallsView, Page};
    use tangul_data::local::AuditEntry;
    let (handle, workspace) = open(cx);
    step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| {
            for at in 0..32 {
                ws.data
                    .local
                    .add_audit(&AuditEntry {
                        at,
                        caller: "mcp".into(),
                        target: format!("tool:run_readonly shop {at}"),
                        sql: "{}".into(),
                        outcome: "ran".into(),
                        error: None,
                        duration_ms: Some(1),
                        rows: None,
                    })
                    .unwrap();
            }
            ws.open_ai_settings(Page::Mcp, window, cx);
        })
    });
    let shown = |cx: &mut TestAppContext| {
        workspace.read_with(cx, |ws, cx| {
            let tab = ws
                .tabs
                .active()
                .and_then(|t| t.ai_settings())
                .unwrap()
                .read(cx);
            let calls = &tab.state().mcp_calls;
            (tab.calls_view("mcp"), calls.entries.len(), calls.total)
        })
    };
    assert_eq!(shown(cx), (CallsView::Collapsed, 5, 32));
    let down = || gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-5000.)));
    step(handle, cx, |window, cx| {
        window.scroll("ai-settings-body", down(), cx)
    });
    step(handle, cx, |window, cx| window.click("mcp-calls-more", cx));
    assert_eq!(shown(cx), (CallsView::Page(0), 25, 32));
    step(handle, cx, |window, cx| {
        window.scroll("ai-settings-body", down(), cx)
    });
    step(handle, cx, |window, cx| window.click("mcp-calls-next", cx));
    assert_eq!(shown(cx), (CallsView::Page(1), 7, 32));
    step(handle, cx, |window, cx| {
        window.scroll("ai-settings-body", down(), cx)
    });
    step(handle, cx, |window, cx| window.click("mcp-calls-less", cx));
    assert_eq!(shown(cx), (CallsView::Collapsed, 5, 32));
}

#[gpui_kit::test]
fn the_status_bar_ends_with_the_ai_then_the_value_viewer(cx: &mut TestAppContext) {
    let (handle, _) = open(cx);
    step(handle, cx, |window, _| {
        let x = |id: gpui_kit::ElementId| window.find(id).bounds().origin.x;
        let assistant = x(button(Panel::Assistant).into());
        let mcp = x("mcp".into());
        let value = x(button(Panel::Value).into());
        assert!(
            assistant < mcp && mcp < value,
            "{assistant:?} {mcp:?} {value:?}"
        );
        // The ACP placeholder is gone.
        assert!(window.try_find("ai-agent").is_none());
    });
}

#[gpui_kit::test]
fn the_theme_follows_the_appearance_setting(cx: &mut TestAppContext) {
    use crate::settings::{Appearance, Settings};
    use tangul_data::themes::{ThemeMode, default_for};
    let (handle, _) = open(cx);
    let set = |appearance: Appearance, light: &str, cx: &mut TestAppContext| {
        let mut s = Settings::default();
        s.theme.appearance = appearance;
        s.theme.light = light.into();
        cx.update(|cx| crate::settings::set_for_test(s, cx));
    };
    let background = |cx: &mut TestAppContext| cx.update(|cx| Theme::global(cx).background);
    let dark = default_for(ThemeMode::Dark);
    let light = default_for(ThemeMode::Light);
    set(Appearance::Dark, "Tangul Light", cx);
    assert_eq!(background(cx), crate::theme::color(dark.colors.background));
    set(Appearance::Light, "Tangul Light", cx);
    assert_eq!(background(cx), crate::theme::color(light.colors.background));
    assert_eq!(
        cx.update(|cx| crate::theme::current(cx).name.clone()),
        "Tangul Light"
    );
    // A theme of the user's, chosen for light.
    let mut mine = light.clone();
    mine.name = "Paper".into();
    mine.colors.background = tangul_data::settings::Rgb::parse("#f4f1ea").unwrap();
    mine.charts = vec![mine.colors.accent, mine.colors.info];
    cx.update(|cx| {
        cx.global_mut::<crate::theme::Themes>()
            .list
            .push(mine.clone())
    });
    set(Appearance::Light, "Paper", cx);
    assert_eq!(background(cx), crate::theme::color(mine.colors.background));
    // Several chart colors: one per series, in turn.
    let colors = cx.update(|cx| crate::theme::chart_colors(3, cx));
    assert_eq!(colors[2], crate::theme::color(mine.colors.accent));
    // A name that isn't a light theme: the built-in one, and why.
    set(Appearance::Light, "Tangul Dark", cx);
    assert_eq!(background(cx), crate::theme::color(light.colors.background));
    let problems = cx.update(|cx| cx.global::<crate::theme::Themes>().problems.clone());
    assert_eq!(problems, ["theme: Tangul Dark is not a light theme"]);
    // One chart color: shades of it, from full to faint.
    set(Appearance::Dark, "Tangul Light", cx);
    let colors = cx.update(|cx| crate::theme::chart_colors(4, cx));
    assert_eq!(colors[0], crate::theme::color(dark.charts[0]));
    assert!(colors.windows(2).all(|w| w[0] != w[1]));
    assert!(colors[3].l < colors[0].l, "fainter on a dark background");
    // A rainbow theme: seven colors, one per series.
    let mut s = Settings::default();
    s.theme.appearance = Appearance::Dark;
    s.theme.dark = "Tangul Dark Rainbow".into();
    cx.update(|cx| crate::settings::set_for_test(s, cx));
    let colors = cx.update(|cx| crate::theme::chart_colors(8, cx));
    let distinct: std::collections::HashSet<_> = colors.iter().map(|c| format!("{c:?}")).collect();
    assert_eq!(distinct.len(), 7);
    assert_eq!(colors[7], colors[0]);
    step(handle, cx, |window, _| {
        assert!(window.find(("panel", 0usize)).visible())
    });
}

#[gpui_kit::test]
fn schema_only_opens_no_ai_connection(cx: &mut TestAppContext) {
    use tangul_core::AiAccess;
    use tangul_service::connections::Purpose;
    let (handle, workspace) = open(cx);
    let offered = |ws: &Workspace| {
        let e = ws.ai_exposed().into_iter().next().expect("offered");
        (e.access, e.conn.is_some())
    };
    step(handle, cx, |_, cx| {
        workspace.update(cx, |ws, cx| {
            shop_for_the_assistant(ws, cx);
            ws.active_db = Some("db".into());
            ws.active_user = Some("app".into());
            ws.ai.assistant_used = true;
            ws.expose_to_ai();
            assert_eq!(offered(ws), (AiAccess::Data, true));
            assert!(ws.connections.get(Purpose::Assistant, "app").is_some());
        })
    });
    // Switched in the title bar's menu: applies at once, the AI connection closes.
    step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.set_ai_access("db", AiAccess::Schema, window, cx);
            assert_eq!(offered(ws), (AiAccess::Schema, false));
            assert!(ws.connections.get(Purpose::Assistant, "app").is_none());
            let db = ws.data.profiles.database("db").unwrap();
            assert_eq!(db.ai_access, AiAccess::Schema);
            // No AI user, so nothing to warn about.
            assert_eq!(ws.knowledge.ai_writable("db"), Some(0));
        });
    });
    step(handle, cx, |window, cx| {
        assert!(window.find("ai-user-switcher").visible());
        workspace.update(cx, |ws, cx| {
            ws.set_ai_access("db", AiAccess::Data, window, cx);
            assert_eq!(offered(ws), (AiAccess::Data, true));
        });
        // MCP clients only ever get the schema; the assistant may read.
        let mut s = crate::settings::Settings::default();
        s.mcp.access = AiAccess::Schema;
        crate::settings::set_for_test(s, cx);
    });
    step(handle, cx, |_, cx| {
        workspace.update(cx, |ws, cx| {
            let events = ws.test_events(cx);
            let calls = Arc::default();
            let mcp = ws.ai_ctx_source("mcp", events.clone(), Arc::clone(&calls))();
            let assistant = ws.ai_ctx_source("assistant", events, calls)();
            assert!(!mcp.reads_data());
            assert_eq!(mcp.exposed()[0].conn, None);
            assert!(assistant.reads_data());
            // The assistant still reads, so its connection stays.
            assert_eq!(offered(ws), (AiAccess::Data, true));
        });
    });
}

/// A board of `shop` with tiles `t1`…: `sqls` in order, `t1` and `t3` alike when
/// asked for; opened in its tab.
fn board_with_tiles(
    handle: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
    sqls: &[&str],
    cx: &mut TestAppContext,
) -> String {
    use tangul_service::boards::{self, Edit};
    cx.update(|cx| shop_with_ai_user(workspace, None, cx));
    let id = workspace.update(cx, |ws, _| {
        let board = boards::create(&mut ws.data.boards, "db", "Signups").unwrap();
        for (i, sql) in sqls.iter().enumerate() {
            let tile = boards::tile(&format!("Tile {}", i + 1), sql, None);
            boards::apply(&mut ws.data.boards, &board.id, Edit::AddTile(tile)).unwrap();
        }
        board.id
    });
    cx.update_window(handle.into(), |_, window, cx| {
        workspace.update(cx, |ws, cx| ws.open_board(&id, window, cx));
        window.render_frame(cx);
    })
    .unwrap();
    id
}

fn tile_ids(workspace: &Entity<Workspace>, board: &str, cx: &App) -> Vec<String> {
    let ws = workspace.read(cx);
    let board = ws.data.boards.get(board).unwrap();
    board.tiles.iter().map(|t| t.id.clone()).collect()
}

fn board_tab(workspace: &Entity<Workspace>, cx: &App) -> Entity<crate::board::BoardTab> {
    let ws = workspace.read(cx);
    ws.tabs
        .active()
        .and_then(|t| t.board())
        .cloned()
        .expect("a board tab")
}

#[gpui_kit::test]
fn the_boards_panel_lists_the_databases_boards_and_opens_them(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    let id = board_with_tiles(handle, &workspace, &["select 1"], cx);
    // Closed, then opened from the panel.
    step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.remove_tab(ws.tabs.active_index(), window, cx)
        })
    });
    step(handle, cx, |window, cx| {
        window.click(button(Panel::Boards), cx)
    });
    step(handle, cx, |window, cx| {
        window.click(("board-row", 0usize), cx)
    });
    cx.update(|cx| {
        assert!(shows(&workspace, Panel::Boards, cx));
        let tab = board_tab(&workspace, cx);
        assert_eq!(tab.read(cx).title.as_ref(), "Signups");
        assert_eq!(tab.read(cx).board().id, id);
    });
    // One tab per board: opening it again shows the same one.
    let open = cx.update(|cx| workspace.read(cx).tabs.items().count());
    step(handle, cx, |window, cx| {
        window.click(("board-row", 0usize), cx)
    });
    cx.update(|cx| assert_eq!(workspace.read(cx).tabs.items().count(), open));
}

#[gpui_kit::test]
fn board_edit_mode_resizes_moves_and_removes_tiles(cx: &mut TestAppContext) {
    use tangul_core::board::Width;
    let (handle, workspace) = open(cx);
    let id = board_with_tiles(
        handle,
        &workspace,
        &["select 1", "select 2", "select 3"],
        cx,
    );
    let width = |cx: &mut TestAppContext| {
        cx.update(|cx| workspace.read(cx).data.boards.get(&id).unwrap().tiles[0].width)
    };
    // Not in edit mode: no handles.
    step(handle, cx, |window, _| {
        assert!(window.try_find(("tile-grip", 0usize)).is_none())
    });
    step(handle, cx, |window, cx| window.click("board-edit", cx));

    step(handle, cx, |window, cx| {
        window.click(("tile-width", 0usize), cx)
    });
    assert_eq!(width(cx), Width::Half);
    step(handle, cx, |window, cx| {
        window.click(("tile-width", 0usize), cx)
    });
    assert_eq!(width(cx), Width::Full);

    // Dragged onto the first tile: it takes its place.
    step(handle, cx, |window, cx| {
        window.drag_to(("tile-grip", 1usize), "tile-t1", cx)
    });
    cx.update(|cx| assert_eq!(tile_ids(&workspace, &id, cx), ["t2", "t1", "t3"]));

    // Opening a tile in a tab works in edit mode too.
    step(handle, cx, |window, cx| {
        window.click(("tile-open", 0usize), cx)
    });
    cx.update(|cx| assert!(workspace.read(cx).tabs.active_query().is_some()));
    cx.update(|cx| {
        workspace.update(cx, |ws, _| {
            let board = ws.tabs.items().position(|t| t.board().is_some()).unwrap();
            ws.tabs.set_active(board);
        })
    });

    step(handle, cx, |window, cx| {
        window.click(("tile-remove", 1usize), cx)
    });
    cx.update(|cx| {
        assert_eq!(tile_ids(&workspace, &id, cx), ["t2", "t3"]);
        // The tab shows the board as saved.
        let tab = board_tab(&workspace, cx);
        assert_eq!(tab.read(cx).board().tiles.len(), 2);
    });
}

#[gpui_kit::test]
fn board_tiles_show_their_results_and_share_a_statement(cx: &mut TestAppContext) {
    use tangul_core::ResultSet;
    use tangul_engine::{QueryError, QueryOutput};
    let (handle, workspace) = open(cx);
    board_with_tiles(
        handle,
        &workspace,
        &["select a", "select b", "select a"],
        cx,
    );
    let rows = |n: usize| {
        let mut result = ResultSet::new(vec![ColumnInfo { name: "x".into() }]);
        let mut batch = RowBatch::default();
        for i in 0..n {
            batch.push_cell(Some(&i.to_string()));
            batch.finish_row();
        }
        result.append(batch);
        QueryOutput {
            results: vec![result],
            rows: Some(n as u64),
            ..QueryOutput::default()
        }
    };
    // One result per different statement: `select a`, then `select b`.
    let failed = QueryError {
        message: "boom".into(),
        position: None,
        proposal: None,
    };
    let tab = cx.update(|cx| board_tab(&workspace, cx));
    step(handle, cx, |_, cx| {
        tab.update(cx, |t, cx| {
            t.show_results(vec![Ok(rows(2)), Err(failed)], cx)
        })
    });
    cx.update(|cx| {
        let result = |tile: &str| tab.read(cx).tile_result(tile).unwrap();
        assert_eq!(result("t1").read(cx).row_count(cx), Some(2));
        assert_eq!(result("t3").read(cx).row_count(cx), Some(2));
        assert_eq!(result("t2").read(cx).row_count(cx), None);
    });
}

#[gpui_kit::test]
fn a_tile_is_shown_as_the_chart_chosen_in_edit_mode(cx: &mut TestAppContext) {
    use tangul_core::ResultSet;
    use tangul_core::chart::{ChartStyle, ChartView};
    use tangul_engine::QueryOutput;
    let (handle, workspace) = open(cx);
    let id = board_with_tiles(handle, &workspace, &["select day, signups"], cx);
    let output = |columns: &[&str], rows: &[&[&str]]| {
        let mut result = ResultSet::new(
            columns
                .iter()
                .map(|c| ColumnInfo {
                    name: c.to_string(),
                })
                .collect(),
        );
        let mut batch = RowBatch::default();
        for row in rows {
            for cell in *row {
                batch.push_cell(Some(cell));
            }
            batch.finish_row();
        }
        result.append(batch);
        QueryOutput {
            results: vec![result],
            rows: Some(rows.len() as u64),
            ..QueryOutput::default()
        }
    };
    step(handle, cx, |window, cx| window.click("board-edit", cx));
    // The menu only in edit mode.
    step(handle, cx, |window, _| {
        assert!(window.try_find(("tile-view", 0usize)).is_some())
    });
    let tab = cx.update(|cx| board_tab(&workspace, cx));
    // Not the default (bars for categories), so the pick has to come through.
    let areas = Some(ChartView {
        style: ChartStyle::Area,
        series: None,
    });
    step(handle, cx, |_, cx| {
        tab.update(cx, |t, cx| t.set_tile_view("t1", areas, cx))
    });
    cx.update(|cx| {
        let board = workspace.read(cx).data.boards.get(&id).unwrap().clone();
        assert_eq!(board.tiles[0].view, areas, "saved with the board");
    });
    let fits = output(&["day", "signups"], &[&["mon", "3"], &["tue", "5"]]);
    step(handle, cx, |_, cx| {
        tab.update(cx, |t, cx| t.show_results(vec![Ok(fits)], cx))
    });
    // Picked before the result: "all columns" of one is that one.
    step(handle, cx, |window, cx| {
        let result = tab.read(cx).tile_result("t1").unwrap();
        let shown = result.read(cx).chart_shown();
        assert_eq!(
            shown,
            Some(ChartView {
                style: ChartStyle::Area,
                series: Some(0)
            })
        );
        assert!(window.try_find(("tile-unfit", 0usize)).is_none());
    });
    // Stacked bars of one column: bars.
    let stacked = Some(ChartView {
        style: ChartStyle::StackedBars,
        series: None,
    });
    step(handle, cx, |_, cx| {
        tab.update(cx, |t, cx| t.set_tile_view("t1", stacked, cx))
    });
    step(handle, cx, |_, cx| {
        let result = tab.read(cx).tile_result("t1").unwrap();
        let shown = result.read(cx).chart_shown().map(|v| v.style);
        assert_eq!(shown, Some(ChartStyle::Bars));
    });
    // A result that can't be a chart: the table, and why.
    let text = output(&["day", "signups"], &[&["mon", "a lot"], &["tue", "5"]]);
    step(handle, cx, |_, cx| {
        tab.update(cx, |t, cx| t.show_results(vec![Ok(text)], cx))
    });
    step(handle, cx, |window, cx| {
        let result = tab.read(cx).tile_result("t1").unwrap();
        assert_eq!(result.read(cx).chart_shown(), None);
        assert!(window.try_find(("tile-unfit", 0usize)).is_some());
    });
}

#[gpui_kit::test]
fn add_to_board_makes_a_new_board_and_a_tile_opens_in_a_tab(cx: &mut TestAppContext) {
    use crate::board::Target;
    use tangul_service::boards;
    let (handle, workspace) = open(cx);
    cx.update(|cx| shop_with_ai_user(&workspace, None, cx));
    step(handle, cx, |window, cx| {
        let tile = boards::Edit::AddTile(boards::tile("Per day", "select 1", None));
        workspace.update(cx, |ws, cx| {
            ws.add_to_board("db", Target::New("Growth".into()), Some(tile), window, cx)
        })
    });
    let board = cx.update(|cx| {
        let board = board_tab(&workspace, cx).read(cx).board().clone();
        assert_eq!(board.name, "Growth");
        assert_eq!(board.database_id, "db");
        assert_eq!(board.tiles[0].title, "Per day");
        assert_eq!(workspace.read(cx).data.boards.for_database("db").len(), 1);
        board
    });
    // A tile opens its query in a tab, linked back to the tile.
    step(handle, cx, |window, cx| {
        window.click(("tile-open", 0usize), cx)
    });
    cx.update(|cx| {
        let query = workspace.read(cx).tabs.active_query().cloned().unwrap();
        assert_eq!(query.read(cx).title.as_ref(), "Per day");
        let link = query.read(cx).board_tile.clone().unwrap();
        assert_eq!((link.board, link.tile), (board.id, Some("t1".to_string())));
    });
}

#[gpui_kit::test]
fn a_tiles_tab_saves_to_the_tile_and_a_new_one_adds_a_tile(cx: &mut TestAppContext) {
    use crate::board::BoardEvent;
    use crate::query_tab::TabEvent;
    let (handle, workspace) = open(cx);
    let id = board_with_tiles(handle, &workspace, &["select 1"], cx);
    let board = cx.update(|cx| board_tab(&workspace, cx));
    let open_tile = |cx: &mut TestAppContext| {
        step(handle, cx, |_, cx| {
            board.update(cx, |_, cx| {
                cx.emit(BoardEvent::OpenTile {
                    tile: "t1".into(),
                    title: "Tile 1".into(),
                    sql: "select 1".into(),
                    view: None,
                })
            })
        })
    };
    open_tile(cx);
    let (query, tabs) = cx.update(|cx| {
        let ws = workspace.read(cx);
        (
            ws.tabs.active_query().cloned().unwrap(),
            ws.tabs.items().count(),
        )
    });
    // One tab per tile; titled as the tile, with nothing to save yet.
    open_tile(cx);
    cx.update(|cx| {
        let ws = workspace.read(cx);
        assert_eq!(ws.tabs.items().count(), tabs);
        assert_eq!(ws.tabs.active_query(), Some(&query));
        assert_eq!(query.read(cx).title.as_ref(), "Tile 1");
        assert!(!query.read(cx).tile_changed(cx));
    });
    let sql_of_tile = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            workspace.read(cx).data.boards.get(&id).unwrap().tiles[0]
                .sql
                .clone()
        })
    };
    let save = |cx: &mut TestAppContext| {
        step(handle, cx, |_, cx| {
            query.update(cx, |_, cx| cx.emit(TabEvent::SaveToBoard))
        })
    };
    // Changed: unsaved until saved, then the tile has it.
    step(handle, cx, |window, cx| {
        QueryTab::insert_item(&query, " + 1", Vec::new(), window, cx)
    });
    cx.update(|cx| assert!(query.read(cx).tile_changed(cx)));
    save(cx);
    let saved = sql_of_tile(cx);
    assert_eq!(saved, "select 1 + 1");
    cx.update(|cx| assert!(!query.read(cx).tile_changed(cx)));
    // A write never goes on the board.
    step(handle, cx, |window, cx| {
        QueryTab::insert_item(&query, "; delete from users", Vec::new(), window, cx)
    });
    save(cx);
    assert_eq!(sql_of_tile(cx), saved);
    // Never pinned.
    let tab_id = cx.update(|cx| query.read(cx).id);
    cx.update(|cx| workspace.update(cx, |ws, cx| ws.toggle_tab_pin(tab_id, cx)));
    cx.update(|cx| assert!(workspace.read(cx).tabs.pinned().next().is_none()));

    // A new tile: written in a tab, added on the first save, titled as the tab.
    step(handle, cx, |_, cx| {
        board.update(cx, |_, cx| {
            cx.emit(BoardEvent::NewTileTab {
                title: "Orders".into(),
                sql: String::new(),
            })
        })
    });
    let new = cx.update(|cx| {
        let new = workspace.read(cx).tabs.active_query().cloned().unwrap();
        assert_ne!(new, query);
        assert_eq!(new.read(cx).title.as_ref(), "Orders");
        assert_eq!(new.read(cx).board_tile.as_ref().unwrap().tile, None);
        new
    });
    step(handle, cx, |window, cx| {
        new.update(cx, |t, cx| t.insert_sql("select 2", window, cx))
    });
    step(handle, cx, |_, cx| {
        new.update(cx, |_, cx| cx.emit(TabEvent::SaveToBoard))
    });
    cx.update(|cx| {
        let board = workspace.read(cx).data.boards.get(&id).unwrap().clone();
        assert_eq!(board.tiles.len(), 2);
        assert_eq!(board.tiles[1].title, "Orders");
        assert_eq!(board.tiles[1].sql, "select 2");
        let link = new.read(cx).board_tile.clone().unwrap();
        assert_eq!(link.tile.as_deref(), Some(board.tiles[1].id.as_str()));
        assert!(!new.read(cx).tile_changed(cx));
    });
}

#[gpui_kit::test]
fn board_links_open_what_they_point_to_and_say_when_it_is_gone(cx: &mut TestAppContext) {
    use tangul_core::board::{Link, LinkTo};
    use tangul_core::chart::ChartView;
    use tangul_data::local::Bookmark;
    use tangul_service::boards::Edit;
    let (handle, workspace) = open(cx);
    let id = board_with_tiles(handle, &workspace, &[], cx);
    let bars = ChartView::from_key("bars");
    step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| {
            let bookmark = Bookmark {
                id: "bm".into(),
                name: "Orders per day".into(),
                sql: "select 2".into(),
                database_id: Some("db".into()),
                ..Default::default()
            };
            ws.data.local.upsert_bookmark(&bookmark, "fp").unwrap();
            let link = |label: &str, to| Link {
                label: label.into(),
                to,
            };
            let links = vec![
                link("Orders", LinkTo::Bookmark("bm".into())),
                link(
                    "By plan",
                    LinkTo::Query {
                        sql: "select 3".into(),
                        view: bars,
                    },
                ),
                link("Old board", LinkTo::Board("gone".into())),
            ];
            ws.edit_board(&id, Edit::SetLinks(links), window, cx);
            ws.check_board_links(cx);
        })
    });
    let tabs = |cx: &mut TestAppContext| cx.update(|cx| workspace.read(cx).tabs.items().count());
    let before = tabs(cx);

    // A query opens in a new tab, shown as it was set.
    step(handle, cx, |window, cx| {
        window.click(("board-link", 1usize), cx)
    });
    cx.update(|cx| {
        let query = workspace.read(cx).tabs.active_query().cloned().unwrap();
        assert_eq!(query.read(cx).title.as_ref(), "By plan");
        assert_eq!(query.read(cx).sql(cx), "select 3");
        assert_eq!(query.read(cx).chart_view(cx), bars);
    });

    // A bookmark opens as the bookmark.
    let show_board = |cx: &mut TestAppContext| {
        step(handle, cx, |window, cx| {
            workspace.update(cx, |ws, cx| ws.open_board(&id, window, cx))
        })
    };
    show_board(cx);
    step(handle, cx, |window, cx| {
        window.click(("board-link", 0usize), cx)
    });
    cx.update(|cx| {
        let query = workspace.read(cx).tabs.active_query().cloned().unwrap();
        assert_eq!(query.read(cx).title.as_ref(), "Orders per day");
        assert_eq!(query.read(cx).bookmark_id.as_deref(), Some("bm"));
    });

    // A board that is gone: the link says so and opens nothing.
    show_board(cx);
    let opened = tabs(cx);
    assert_eq!(opened, before + 2);
    step(handle, cx, |window, cx| {
        window.click(("board-link", 2usize), cx)
    });
    cx.update(|cx| {
        assert_eq!(workspace.read(cx).tabs.items().count(), opened);
        assert!(board_tab(&workspace, cx).read(cx).is_broken(2));
        assert!(!board_tab(&workspace, cx).read(cx).is_broken(0));
    });
}

#[gpui_kit::test]
fn add_to_board_as_a_link(cx: &mut TestAppContext) {
    use crate::board::Target;
    use tangul_core::board::{Link, LinkTo};
    use tangul_service::boards::Edit;
    let (handle, workspace) = open(cx);
    cx.update(|cx| shop_with_ai_user(&workspace, None, cx));
    step(handle, cx, |window, cx| {
        let link = Edit::AddLink(Link {
            label: "Raw rows".into(),
            to: LinkTo::Query {
                sql: "select 1".into(),
                view: None,
            },
        });
        workspace.update(cx, |ws, cx| {
            ws.add_to_board("db", Target::New("Growth".into()), Some(link), window, cx)
        })
    });
    cx.update(|cx| {
        let board = board_tab(&workspace, cx).read(cx).board().clone();
        assert!(board.tiles.is_empty());
        assert_eq!(board.links[0].label, "Raw rows");
    });
}

#[gpui_kit::test]
fn board_details_keep_links_without_showing_where_they_point(cx: &mut TestAppContext) {
    use tangul_core::board::{Link, LinkTo};
    use tangul_service::boards::Edit;
    let (handle, workspace) = open(cx);
    let id = board_with_tiles(handle, &workspace, &[], cx);
    let links = vec![
        Link {
            label: "Epic".into(),
            to: LinkTo::Url("https://example.com/epic".into()),
        },
        Link {
            label: "Other".into(),
            to: LinkTo::Board("gone".into()),
        },
    ];
    step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| {
            ws.edit_board(&id, Edit::SetLinks(links.clone()), window, cx)
        })
    });
    step(handle, cx, |window, cx| window.click("board-edit", cx));
    step(handle, cx, |window, cx| window.click("board-details", cx));
    step(handle, cx, |window, _| {
        assert!(
            window.try_find("board-links").is_some(),
            "the dialog is open"
        );
    });
    step(handle, cx, |window, cx| window.click("save", cx));
    cx.update(|cx| {
        let board = workspace.read(cx).data.boards.get(&id).unwrap().clone();
        assert_eq!(board.links, links);
    });
}

#[gpui_kit::test]
fn add_tile_offers_bookmarks_and_recent_queries(cx: &mut TestAppContext) {
    use tangul_data::local::{Bookmark, HistoryEntry, NewHistory, Source};
    let (handle, workspace) = open(cx);
    let id = board_with_tiles(handle, &workspace, &[], cx);
    cx.update(|cx| {
        workspace.update(cx, |ws, _| {
            let bookmark = Bookmark {
                id: "bm".into(),
                name: "Orders per day".into(),
                sql: "select 2".into(),
                database_id: Some("db".into()),
                ..Default::default()
            };
            ws.data.local.upsert_bookmark(&bookmark, "fp").unwrap();
            for (at, sql) in [(1, "select 5"), (2, "select 6"), (3, "select 5")] {
                ws.data
                    .local
                    .add_history(&NewHistory {
                        database_id: "db",
                        user_id: "app",
                        sql,
                        fingerprint: sql,
                        entry: HistoryEntry {
                            sql: sql.into(),
                            source: Source::App,
                            started_at: at,
                            duration_ms: 1,
                            rows: Some(1),
                            error: None,
                        },
                    })
                    .unwrap();
            }
        })
    });
    cx.update(|cx| {
        let choices = board_tab(&workspace, cx).read(cx).choices(cx);
        assert_eq!(choices.bookmarks[0].sql, "select 2");
        // Newest first, each query once.
        assert_eq!(choices.history, ["select 5", "select 6"]);
        // No assistant set up: no "Ask AI".
        assert!(!choices.ai);
        assert!(choices.boards.iter().all(|(b, _)| *b != id));
    });
}

#[gpui_kit::test]
fn an_answer_to_ask_ai_goes_onto_the_board_it_was_asked_from(cx: &mut TestAppContext) {
    use crate::board::BoardEvent;
    let (handle, workspace) = open(cx);
    let id = board_with_tiles(handle, &workspace, &["select 1"], cx);
    let tab = cx.update(|cx| board_tab(&workspace, cx));
    step(handle, cx, |_, cx| {
        tab.update(cx, |_, cx| {
            cx.emit(BoardEvent::AskAi("Signups per day".into()))
        })
    });
    cx.update(|cx| assert!(shows(&workspace, Panel::Assistant, cx)));
    // "Add to board" on the answer's SQL: the dialog has that board chosen.
    step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| ws.add_answer_to_board("select 9", window, cx))
    });
    step(handle, cx, |window, cx| window.click("add", cx));
    cx.update(|cx| {
        let board = workspace.read(cx).data.boards.get(&id).unwrap().clone();
        assert_eq!(board.tiles.len(), 2);
        assert_eq!(board.tiles[1].title, "Signups per day");
        assert_eq!(board.tiles[1].sql, "select 9");
    });
}

#[gpui_kit::test]
fn enter_in_the_new_board_dialog_creates_the_board(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    cx.update(|cx| shop_with_ai_user(&workspace, None, cx));
    step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| ws.new_board(window, cx))
    });
    step(handle, cx, |window, cx| window.click("new-board-name", cx));
    step(handle, cx, |window, cx| window.input("Growth", cx));
    step(handle, cx, |window, cx| window.press("enter", cx));
    step(handle, cx, |window, cx| {
        assert!(window.try_find("add").is_none(), "closed");
        let boards = workspace.read(cx).data.boards.for_database("db");
        assert_eq!(boards.len(), 1, "created");
        assert_eq!(boards[0].name, "Growth");
    });
    // Without a name: the dialog stays, with the reason.
    step(handle, cx, |window, cx| {
        workspace.update(cx, |ws, cx| ws.new_board(window, cx))
    });
    step(handle, cx, |window, cx| window.click("new-board-name", cx));
    step(handle, cx, |window, cx| window.press("enter", cx));
    step(handle, cx, |window, cx| {
        assert!(window.try_find("add").is_some(), "still open");
        assert_eq!(workspace.read(cx).data.boards.for_database("db").len(), 1);
    });
}

/// Databases `db1` (user `u1`) and `db2` (user `u2`), none shown yet.
fn two_databases(workspace: &Entity<Workspace>, cx: &mut App) {
    use tangul_core::{Database, DbUser, DriverKind, TlsMode};
    workspace.update(cx, |ws, _| {
        for n in [1, 2] {
            ws.data.profiles.databases.push(Database {
                id: format!("db{n}"),
                name: format!("shop{n}"),
                driver: DriverKind::Postgres,
                host: "localhost".into(),
                port: 5432,
                database: format!("shop{n}"),
                tls: TlsMode::Disable,
                users: vec![DbUser {
                    id: format!("u{n}"),
                    user: format!("u{n}"),
                }],
                ssh: None,
                assistant_user: None,
                ai_values: Default::default(),
                ai_access: Default::default(),
                ai_tools: true,
                editor_values: true,
            });
        }
    });
}

fn show_db(workspace: &Entity<Workspace>, n: u8, window: &mut Window, cx: &mut App) {
    workspace.update(cx, |ws, cx| {
        ws.select_user(format!("db{n}"), format!("u{n}"), window, cx)
    });
}

/// The shown tabs' titles, `*` marking the pinned ones.
fn tab_titles(workspace: &Entity<Workspace>, cx: &App) -> Vec<String> {
    let ws = workspace.read(cx);
    ws.tabs
        .items()
        .enumerate()
        .map(|(ix, t)| {
            let pin = if ws.tabs.is_pinned(ix) { "*" } else { "" };
            format!("{pin}{}", t.title(cx))
        })
        .collect()
}

fn titled_tab(
    workspace: &Entity<Workspace>,
    sql: &str,
    title: &str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<QueryTab> {
    let tab = workspace.update(cx, |ws, cx| ws.add_tab(sql, window, cx));
    tab.update(cx, |t, _| t.title = title.to_string().into());
    tab
}

#[gpui_kit::test]
fn each_database_has_its_own_tabs(cx: &mut TestAppContext) {
    use crate::ai_settings::Page;
    let (handle, workspace) = open(cx);
    cx.update(|cx| two_databases(&workspace, cx));
    step(handle, cx, |window, cx| {
        show_db(&workspace, 1, window, cx);
        titled_tab(&workspace, "select 1", "one", window, cx);
        titled_tab(&workspace, "select 2", "two", window, cx);
        workspace.update(cx, |ws, cx| {
            ws.open_ai_settings(Page::Assistant, window, cx)
        });
    });
    // The other database: none of the first one's tabs; the AI settings go along.
    step(handle, cx, |window, cx| show_db(&workspace, 2, window, cx));
    cx.update(|cx| assert_eq!(tab_titles(&workspace, cx), ["AI settings"]));
    step(handle, cx, |window, cx| {
        titled_tab(&workspace, "select 3", "three", window, cx);
    });
    // Back: the first database's tabs as they were, and only those.
    step(handle, cx, |window, cx| show_db(&workspace, 1, window, cx));
    cx.update(|cx| assert_eq!(tab_titles(&workspace, cx), ["one", "two", "AI settings"]));
    step(handle, cx, |window, cx| show_db(&workspace, 2, window, cx));
    cx.update(|cx| {
        assert_eq!(tab_titles(&workspace, cx), ["three", "AI settings"]);
        // Both databases' query tabs are still there (a bookmark rename reaches them).
        assert_eq!(workspace.read(cx).all_query_tabs().len(), 3);
    });
}

#[gpui_kit::test]
fn pinned_tabs_are_kept_per_database(cx: &mut TestAppContext) {
    use crate::ai_settings::Page;
    use tangul_core::chart::{ChartStyle, ChartView};
    use tangul_core::pinned::PinnedTab;
    use tangul_service::boards;
    let (handle, workspace) = open(cx);
    cx.update(|cx| two_databases(&workspace, cx));
    let board = cx.update(|cx| {
        workspace.update(cx, |ws, _| {
            boards::create(&mut ws.data.boards, "db1", "Growth")
                .unwrap()
                .id
        })
    });
    let bars = Some(ChartView {
        style: ChartStyle::Bars,
        series: None,
    });
    let pin = |workspace: &Entity<Workspace>, cx: &mut App| {
        workspace.update(cx, |ws, cx| {
            let id = ws.tabs.active().unwrap().id(cx);
            ws.toggle_tab_pin(id, cx);
        })
    };
    let query = step(handle, cx, |window, cx| {
        show_db(&workspace, 1, window, cx);
        let query = titled_tab(&workspace, "select 1", "Signups", window, cx);
        query.update(cx, |t, cx| t.set_chart_view(bars, cx));
        pin(&workspace, cx);
        titled_tab(&workspace, "select 9", "scratch", window, cx);
        workspace.update(cx, |ws, cx| ws.open_board(&board, window, cx));
        pin(&workspace, cx);
        workspace.update(cx, |ws, cx| {
            ws.open_ai_settings(Page::Assistant, window, cx)
        });
        pin(&workspace, cx);
        query
    });
    let saved =
        |cx: &mut TestAppContext| cx.update(|cx| workspace.read(cx).data.local.pinned_tabs("db1"));
    let signups = |sql: &str| PinnedTab::Query {
        title: "Signups".into(),
        sql: sql.into(),
        user_id: Some("u1".into()),
        bookmark_id: None,
        view: bars,
    };
    // Only query and board tabs are kept, in order.
    assert_eq!(
        saved(cx),
        [
            signups("select 1"),
            PinnedTab::Board {
                board_id: board.clone()
            }
        ]
    );
    // An edit is saved after a pause; passwords never are.
    step(handle, cx, |window, cx| {
        QueryTab::insert_item(
            &query,
            "; create role x password 'secret'",
            Vec::new(),
            window,
            cx,
        )
    });
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    let PinnedTab::Query { sql, .. } = &saved(cx)[0] else {
        panic!("the query first");
    };
    assert!(
        sql.starts_with("select 1; create role x") && !sql.contains("secret"),
        "{sql}"
    );

    // What a previous run kept for db2 comes back the first time it is shown:
    // pinned, in order; a board that is gone is skipped.
    cx.update(|cx| {
        let ws = workspace.read(cx);
        ws.data.local.set_pinned_tabs(
            "db2",
            &[
                PinnedTab::Board {
                    board_id: "gone".into(),
                },
                PinnedTab::Query {
                    title: "Orders".into(),
                    sql: "select 2".into(),
                    user_id: Some("nobody".into()),
                    bookmark_id: Some("gone".into()),
                    view: bars,
                },
            ],
        );
    });
    step(handle, cx, |window, cx| show_db(&workspace, 2, window, cx));
    cx.update(|cx| {
        assert_eq!(tab_titles(&workspace, cx), ["*Orders", "*AI settings"]);
        let ws = workspace.read(cx);
        let tab = ws.tabs.get(0).and_then(|t| t.query()).unwrap().read(cx);
        assert_eq!(tab.sql(cx), "select 2");
        assert_eq!(tab.chart_view(cx), bars);
        assert_eq!(tab.bookmark_id, None);
    });
    // Unpinning takes it off the list; showing db2 again doesn't bring it back.
    cx.update(|cx| {
        workspace.update(cx, |ws, _| ws.tabs.set_active(0));
        pin(&workspace, cx);
    });
    cx.update(|cx| assert!(workspace.read(cx).data.local.pinned_tabs("db2").is_empty()));
    step(handle, cx, |window, cx| show_db(&workspace, 1, window, cx));
    cx.update(|cx| {
        assert_eq!(
            tab_titles(&workspace, cx),
            ["*Signups", "scratch", "*Growth", "*AI settings"]
        )
    });
    step(handle, cx, |window, cx| show_db(&workspace, 2, window, cx));
    cx.update(|cx| assert_eq!(tab_titles(&workspace, cx), ["Orders", "*AI settings"]));
}

#[gpui_kit::test]
fn an_empty_tab_shows_hints_until_something_is_typed(cx: &mut TestAppContext) {
    let (handle, workspace) = open(cx);
    let tab = cx.update(|cx| {
        handle
            .update(cx, |_, window, cx| {
                workspace.update(cx, |ws, cx| ws.add_tab("", window, cx))
            })
            .unwrap()
    });
    cx.update(|cx| assert!(tab.read(cx).hints_shown(cx)));
    step(handle, cx, |window, cx| {
        tab.update(cx, |t, cx| t.insert_sql("select 1", window, cx))
    });
    cx.update(|cx| assert!(!tab.read(cx).hints_shown(cx)));
}
