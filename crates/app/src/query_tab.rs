//! One query tab: SQL editor on top, results below.

use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Editor, EditorState},
    table::{DataTable, TableState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rusdb_core::{ConnId, DriverKind, QueryId, TabId};
use rusdb_engine::{Command, EngineHandle, ExecEvent, QueryEvent};

use crate::{BookmarkQuery, RunQuery, assets::Lucide, results::ResultsDelegate};

/// Connection a tab runs against.
#[derive(Clone)]
pub struct TabConn {
    pub id: ConnId,
    pub name: SharedString,
    pub driver: DriverKind,
}

pub enum TabEvent {
    /// The user wants to run this SQL; the workspace assigns a QueryId.
    Run(String),
    /// The user wants to save the editor's SQL as a bookmark.
    Bookmark(String),
}

enum Status {
    Idle,
    Running,
    Done {
        elapsed: Duration,
        error: Option<SharedString>,
        cancelled: bool,
    },
}

pub struct QueryTab {
    pub id: TabId,
    pub title: SharedString,
    pub conn: Option<TabConn>,
    /// Bookmark this tab was opened from or saved as; bookmarking again updates it.
    pub bookmark_id: Option<String>,
    engine: EngineHandle,
    editor: Entity<EditorState>,
    table: Entity<TableState<ResultsDelegate>>,
    status: Status,
    running: Option<QueryId>,
    /// Session pinned to this tab because a transaction is open.
    in_transaction: bool,
    has_result_set: bool,
    truncated: bool,
    last_rows: Option<u64>,
}

impl EventEmitter<TabEvent> for QueryTab {}

impl QueryTab {
    pub fn new(
        id: TabId,
        conn: Option<TabConn>,
        sql: &str,
        engine: EngineHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .default_value(sql.to_string())
                .placeholder("SELECT ...   (⌘↵ to run)")
        });
        let table = cx.new(|cx| {
            TableState::new(ResultsDelegate::default(), window, cx)
                .sortable(false)
                .col_movable(false)
        });
        Self {
            id,
            title: format!("Query {}", id.0).into(),
            conn,
            bookmark_id: None,
            engine,
            editor,
            table,
            status: Status::Idle,
            running: None,
            in_transaction: false,
            has_result_set: false,
            truncated: false,
            last_rows: None,
        }
    }

    pub fn focus_editor(tab: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let editor = tab.read(cx).editor.clone();
        editor.update(cx, |e, cx| e.focus(window, cx));
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub fn in_transaction(&self) -> bool {
        self.in_transaction
    }

    /// Switching connection releases a pinned session (the engine rolls it back).
    pub fn set_conn(&mut self, conn: TabConn, cx: &mut Context<Self>) {
        if let Some(old) = &self.conn
            && old.id != conn.id
            && self.in_transaction
        {
            self.engine.send(Command::ReleaseTab {
                tab: self.id,
                conn: old.id,
            });
            self.in_transaction = false;
        }
        self.conn = Some(conn);
        cx.notify();
    }

    fn run(&mut self, _: &RunQuery, _: &mut Window, cx: &mut Context<Self>) {
        if self.running.is_some() {
            return;
        }
        // Run the selection if there is one, otherwise the whole buffer.
        let editor = self.editor.read(cx);
        let selected = editor.selected_value();
        let sql = if selected.trim().is_empty() {
            editor.value()
        } else {
            selected
        };
        if sql.trim().is_empty() {
            return;
        }
        cx.emit(TabEvent::Run(sql.to_string()));
    }

    fn bookmark(&mut self, _: &BookmarkQuery, _: &mut Window, cx: &mut Context<Self>) {
        let sql = self.editor.read(cx).value().to_string();
        if !sql.trim().is_empty() {
            cx.emit(TabEvent::Bookmark(sql));
        }
    }

    /// Called by the workspace once the query was sent to the engine.
    pub fn started(&mut self, query: QueryId, cx: &mut Context<Self>) {
        self.running = Some(query);
        self.status = Status::Running;
        self.has_result_set = false;
        self.truncated = false;
        self.last_rows = None;
        self.table.update(cx, |t, cx| {
            t.delegate_mut().clear();
            t.refresh(cx);
            cx.notify();
        });
        cx.notify();
    }

    fn cancel(&mut self, _: &ClickEvent, _: &mut Window, _: &mut Context<Self>) {
        if let Some(query) = self.running {
            self.engine.send(Command::Cancel { query });
        }
    }

    pub fn on_query_event(&mut self, event: QueryEvent, cx: &mut Context<Self>) {
        match event {
            QueryEvent::Exec(ExecEvent::ResultStart(cols)) => {
                // Multiple statements: the last result set is shown.
                self.has_result_set = true;
                self.truncated = false;
                self.table.update(cx, |t, cx| {
                    t.delegate_mut().reset(cols);
                    t.refresh(cx);
                    cx.notify();
                });
            }
            QueryEvent::Exec(ExecEvent::Rows(batch)) => {
                self.table.update(cx, |t, cx| {
                    t.delegate_mut().result.append(batch);
                    cx.notify();
                });
            }
            QueryEvent::Exec(ExecEvent::StatementDone { rows }) => self.last_rows = Some(rows),
            QueryEvent::Exec(ExecEvent::Truncated) => self.truncated = true,
            QueryEvent::Finished {
                elapsed,
                error,
                in_transaction,
                cancelled,
            } => {
                self.running = None;
                self.in_transaction = in_transaction;
                self.status = Status::Done {
                    elapsed,
                    error: error.map(Into::into),
                    cancelled,
                };
            }
        }
        cx.notify();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let run_or_cancel = if self.running.is_some() {
            Button::new("cancel")
                .label("Cancel")
                .icon(IconName::CircleX)
                .small()
                .danger()
                .on_click(cx.listener(Self::cancel))
                .into_any_element()
        } else {
            Button::new("run")
                .label("Run")
                .icon(IconName::Play)
                .small()
                .primary()
                .disabled(self.conn.is_none())
                .on_click(cx.listener(|this, _, window, cx| this.run(&RunQuery, window, cx)))
                .into_any_element()
        };
        let conn_label: SharedString = match &self.conn {
            Some(c) => format!("{} · {}", c.name, c.driver.label()).into(),
            None => "No connection — pick one in the title bar".into(),
        };

        h_flex()
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(run_or_cancel)
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(conn_label),
            )
            .when(self.in_transaction, |this| {
                this.child(
                    div()
                        .px_1p5()
                        .rounded(theme.radius)
                        .text_xs()
                        .bg(theme.warning)
                        .text_color(theme.warning_foreground)
                        .child("in transaction"),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("bookmark")
                    .icon(Icon::new(Lucide::Bookmark))
                    .label(if self.bookmark_id.is_some() {
                        "Update bookmark"
                    } else {
                        "Bookmark"
                    })
                    .ghost()
                    .small()
                    .tooltip("Save query as bookmark (⌘D)")
                    .on_click(
                        cx.listener(|this, _, window, cx| {
                            this.bookmark(&BookmarkQuery, window, cx)
                        }),
                    ),
            )
    }

    fn render_results(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        match &self.status {
            Status::Done {
                error: Some(err), ..
            } => div()
                .size_full()
                .p_3()
                .font_family(theme.mono_font_family.clone())
                .text_sm()
                .text_color(theme.danger)
                .child(err.clone())
                .into_any_element(),
            Status::Done { .. } if !self.has_result_set => div()
                .size_full()
                .p_3()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(format!("OK, {} rows affected", self.last_rows.unwrap_or(0)))
                .into_any_element(),
            Status::Idle => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Run a query to see results")
                .into_any_element(),
            _ => DataTable::new(&self.table)
                .stripe(true)
                .xsmall()
                .into_any_element(),
        }
    }

    /// Timing and row count of the last run, shown in the workspace status bar.
    pub fn status_text(&self, cx: &App) -> SharedString {
        let rows = self.table.read(cx).delegate().result.row_count();
        match &self.status {
            Status::Idle => "".into(),
            Status::Running => format!("Running… {rows} rows").into(),
            Status::Done {
                elapsed, cancelled, ..
            } => {
                let mut s = format!("{} ms", elapsed.as_millis());
                if self.has_result_set {
                    let noun = if rows == 1 { "row" } else { "rows" };
                    s = format!("{rows} {noun} · {s}");
                }
                if *cancelled {
                    s.push_str(" · cancelled");
                }
                if self.truncated {
                    s.push_str(&format!(" · limited to {} rows", rusdb_engine::ROW_LIMIT));
                }
                s.into()
            }
        }
    }
}

impl Render for QueryTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .on_action(cx.listener(Self::run))
            .on_action(cx.listener(Self::bookmark))
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .key_context("QueryEditor")
                    .h(relative(0.4))
                    .min_h(px(80.))
                    .child(Editor::new(&self.editor).h_full().bordered(false)),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(div().flex_1().min_h_0().child(self.render_results(cx))),
            )
    }
}

impl Focusable for QueryTab {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}
