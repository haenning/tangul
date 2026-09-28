//! One query tab: the SQL editor on top, the result below, on a connection. The
//! editor and the result are their own parts ([`SqlEditor`], [`ResultView`]); the tab
//! puts them together and passes what they ask for on to the workspace.

mod editor;

use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{CompletionProvider, Editor},
    notification::Notification,
    resizable::{ResizableState, resizable_panel, v_resizable},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_assist::chart::{ChartData, ChartView};
use tangul_assist::columns::ColumnFacts;
use tangul_assist::{edit::RowEdit, related::RelatedLink};
use tangul_core::{ConnId, QueryId, TabId};
use tangul_engine::{Command, EngineHandle, Problem, QueryEvent};

use crate::{
    BookmarkQuery, RunAll, RunQuery,
    assets::Lucide,
    connection::{ConnStatus, Connection},
    result_view::{CellValue, ResultEvent, ResultView},
};
use editor::{EditorEvent, RunTake, SqlEditor};

/// Editor height a new tab starts with.
const EDITOR_HEIGHT: f32 = 240.;
/// Dragging the editor below this height hides it completely.
const EDITOR_SNAP_HIDE: f32 = 60.;

#[derive(Clone)]
pub enum TabEvent {
    /// The user wants to run this SQL; the workspace assigns a QueryId.
    Run(String),
    /// Check these statements (byte offset in the editor, text) on the server
    /// without running them; the answer goes to `apply_validation(generation, …)`.
    Validate {
        generation: u64,
        statements: Vec<(usize, String)>,
    },
    /// The user wants to save the editor's SQL as a bookmark.
    Bookmark(String),
    /// Open (and run) a query for rows related to the selected ones, on this tab's
    /// connection.
    OpenRelated {
        title: String,
        sql: String,
    },
    /// Open SQL for the selected rows in a new tab on this tab's connection, without
    /// running it (UPDATE / DELETE statements).
    OpenSql {
        title: String,
        sql: String,
    },
    /// Run a filtered / sorted version of the last result (`append: false`) or its
    /// next page (`append: true`). Not a run of its own.
    Refine {
        sql: String,
        append: bool,
    },
    /// Open a table (the target of a foreign key) in a new tab, like clicking it in
    /// the schema tree.
    OpenTable {
        schema: String,
        table: String,
    },
    /// The value viewer's cells: see [`ResultEvent::Inspect`] and
    /// [`ResultEvent::Compare`].
    Inspect {
        cell: CellValue,
        open: bool,
    },
    Compare(CellValue),
}

pub struct QueryTab {
    pub id: TabId,
    pub title: SharedString,
    /// The connection it runs against, shared with the workspace and other tabs.
    pub conn: Option<Entity<Connection>>,
    /// Re-renders the tab when its connection changes (status, name).
    _conn_changes: Option<Subscription>,
    /// Bookmark this tab was opened from or saved as; bookmarking again updates it.
    pub bookmark_id: Option<String>,
    /// Times the last query was run successfully, when it is worth bookmarking.
    pub bookmark_hint: Option<u32>,
    /// Editor/results split. The editor is the first panel; height 0 means hidden.
    split: Entity<ResizableState>,
    /// Editor height to come back to when it is shown again after being hidden.
    editor_restore: Pixels,
    engine: EngineHandle,
    editor: Entity<SqlEditor>,
    results: Entity<ResultView>,
    /// Session pinned to this tab because a transaction is open.
    in_transaction: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TabEvent> for QueryTab {}

impl QueryTab {
    pub fn new(
        id: TabId,
        conn: Option<Entity<Connection>>,
        sql: &str,
        engine: EngineHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| SqlEditor::new(sql, window, cx));
        let results = cx.new(|cx| ResultView::new(window, cx));
        if let Some(conn) = &conn {
            let dialect = conn.read(cx).dialect();
            results.update(cx, |r, _| r.set_dialect(dialect));
            editor.update(cx, |e, cx| e.set_lexing(dialect.lexing(), cx));
        }
        let subscriptions = vec![
            cx.subscribe(&editor, |_, _, event: &EditorEvent, cx| match event {
                EditorEvent::Validate {
                    generation,
                    statements,
                } => cx.emit(TabEvent::Validate {
                    generation: *generation,
                    statements: statements.clone(),
                }),
            }),
            cx.subscribe(&results, Self::on_result_event),
        ];
        Self {
            id,
            title: format!("Query {}", id.0).into(),
            _conn_changes: conn.as_ref().map(|c| cx.observe(c, |_, _, cx| cx.notify())),
            conn,
            bookmark_id: None,
            bookmark_hint: None,
            split: cx.new(|_| ResizableState::default()),
            editor_restore: px(EDITOR_HEIGHT),
            engine,
            editor,
            results,
            in_transaction: false,
            _subscriptions: subscriptions,
        }
    }

    fn on_result_event(
        &mut self,
        _: Entity<ResultView>,
        event: &ResultEvent,
        cx: &mut Context<Self>,
    ) {
        let event = match event.clone() {
            ResultEvent::Failed { message, position } => {
                self.editor
                    .update(cx, |e, cx| e.mark_error(position, message, cx));
                return;
            }
            ResultEvent::OpenRelated { title, sql } => TabEvent::OpenRelated { title, sql },
            ResultEvent::OpenSql { title, sql } => TabEvent::OpenSql { title, sql },
            ResultEvent::OpenTable { schema, table } => TabEvent::OpenTable { schema, table },
            ResultEvent::Refine { sql, append } => TabEvent::Refine { sql, append },
            ResultEvent::Inspect { cell, open } => TabEvent::Inspect { cell, open },
            ResultEvent::Compare(cell) => TabEvent::Compare(cell),
        };
        cx.emit(event);
    }

    fn editor_hidden(&self, cx: &App) -> bool {
        self.split
            .read(cx)
            .sizes()
            .first()
            .is_some_and(|height| *height < px(1.))
    }

    /// Hides the editor so only the toolbar and the results remain, or brings it back.
    fn toggle_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let hidden = self.editor_hidden(cx);
        let height = if hidden { self.editor_restore } else { px(0.) };
        self.split
            .update(cx, |split, cx| split.resize_panel(0, height, window, cx));
        if hidden {
            SqlEditor::focus(&self.editor, window, cx);
        }
        cx.notify();
    }

    /// After a drag on the divider: remembers the editor height, or hides the
    /// editor when it was dragged (almost) shut.
    fn split_resized(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(height) = self.split.read(cx).sizes().first().copied() else {
            return;
        };
        if height >= px(EDITOR_SNAP_HIDE) {
            self.editor_restore = height;
        } else if height > px(0.) {
            self.split
                .update(cx, |split, cx| split.resize_panel(0, px(0.), window, cx));
        }
        cx.notify();
    }

    pub fn focus_editor(tab: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let editor = tab.read(cx).editor.clone();
        SqlEditor::focus(&editor, window, cx);
    }

    pub fn set_completion(&self, provider: Rc<dyn CompletionProvider>, cx: &mut App) {
        SqlEditor::set_completion(&self.editor, provider, cx);
    }

    pub fn sql(&self, cx: &App) -> String {
        self.editor.read(cx).text(cx)
    }

    pub fn result_columns(&self, cx: &App) -> Vec<String> {
        self.results.read(cx).columns(cx)
    }

    /// See [`ResultView::set_row_actions`].
    pub fn set_row_actions(
        &mut self,
        links: Vec<RelatedLink>,
        edit: Option<RowEdit>,
        facts: Vec<Option<ColumnFacts>>,
        cx: &mut Context<Self>,
    ) {
        self.results
            .update(cx, |r, cx| r.set_row_actions(links, edit, facts, cx));
    }

    /// Rows returned by the last result set, or affected by the last statement.
    pub fn row_count(&self, cx: &App) -> Option<u64> {
        self.results.read(cx).row_count(cx)
    }

    pub fn is_running(&self, cx: &App) -> bool {
        self.results.read(cx).running().is_some()
    }

    /// The engine's id of the tab's connection.
    pub fn conn_id(&self, cx: &App) -> Option<ConnId> {
        Some(self.conn.as_ref()?.read(cx).id)
    }

    pub fn in_transaction(&self) -> bool {
        self.in_transaction
    }

    /// Switching connection releases a pinned session (the engine rolls it back).
    pub fn set_conn(&mut self, conn: Entity<Connection>, cx: &mut Context<Self>) {
        // The same SQL can be valid on one connection and not on another.
        self.editor.update(cx, |e, cx| e.revalidate(cx));
        if let Some(old) = &self.conn
            && *old != conn
            && self.in_transaction
        {
            self.engine.send(Command::ReleaseTab {
                tab: self.id,
                conn: old.read(cx).id,
            });
            self.in_transaction = false;
        }
        let dialect = conn.read(cx).dialect();
        self.results.update(cx, |r, _| r.set_dialect(dialect));
        self.editor
            .update(cx, |e, cx| e.set_lexing(dialect.lexing(), cx));
        self._conn_changes = Some(cx.observe(&conn, |_, _, cx| cx.notify()));
        self.conn = Some(conn);
        cx.notify();
    }

    /// ⌘↵: the selection, or the statement under the cursor.
    fn run(&mut self, _: &RunQuery, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_running(cx) {
            return;
        }
        // Without a connection nothing runs; the workspace says so.
        let dialect = self.conn.as_ref().map(|c| c.read(cx).dialect());
        let take = self.editor.update(cx, |e, cx| match dialect {
            Some(d) => e.take_run(d, cx),
            None => e.take_run_all(cx),
        });
        self.emit_run(take, window, cx);
    }

    /// ⌘⇧↵: the selection, or everything in the editor.
    pub fn run_all(&mut self, _: &RunAll, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_running(cx) {
            return;
        }
        let take = self.editor.update(cx, |e, cx| e.take_run_all(cx));
        self.emit_run(take, window, cx);
    }

    fn emit_run(&mut self, take: RunTake, window: &mut Window, cx: &mut Context<Self>) {
        match take {
            RunTake::Sql(sql) => cx.emit(TabEvent::Run(sql)),
            RunTake::NoStatement => window.push_notification(
                Notification::info(
                    "No statement at cursor. Put the cursor in one, select what to run, \
                     or run all (⌘⇧↵).",
                ),
                cx,
            ),
            RunTake::Empty => {}
        }
    }

    fn bookmark(&mut self, _: &BookmarkQuery, _: &mut Window, cx: &mut Context<Self>) {
        let sql = self.sql(cx);
        if !sql.trim().is_empty() {
            cx.emit(TabEvent::Bookmark(sql));
        }
    }

    /// Called by the workspace once the query was sent to the engine.
    pub fn started(&mut self, query: QueryId, sql: &str, cx: &mut Context<Self>) {
        self.editor.update(cx, |e, cx| e.started(sql, cx));
        self.results.update(cx, |r, cx| r.started(query, sql, cx));
        cx.notify();
    }

    /// Called by the workspace once a derived query (filter, sort, next page) was
    /// sent.
    pub fn started_derived(&mut self, query: QueryId, append: bool, cx: &mut Context<Self>) {
        self.results
            .update(cx, |r, cx| r.started_derived(query, append, cx));
        cx.notify();
    }

    fn cancel(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(query) = self.results.read(cx).running() {
            self.engine.send(Command::Cancel { query });
        }
    }

    pub fn on_query_event(&mut self, event: QueryEvent, cx: &mut Context<Self>) {
        if let QueryEvent::Finished { in_transaction, .. } = &event {
            self.in_transaction = *in_transaction;
        }
        self.results.update(cx, |r, cx| r.on_query_event(event, cx));
        cx.notify();
    }

    /// See [`SqlEditor::apply_validation`].
    pub fn apply_validation(
        &mut self,
        generation: u64,
        problems: Vec<Problem>,
        cx: &mut Context<Self>,
    ) {
        self.editor
            .update(cx, |e, cx| e.apply_validation(generation, problems, cx));
    }

    /// See [`ResultView::clear_mark`].
    pub fn clear_value_mark(&mut self, compared: bool, cx: &mut Context<Self>) {
        self.results.update(cx, |r, cx| r.clear_mark(compared, cx));
    }

    /// The last result as chart data, or why it can't be charted.
    pub fn chart(&self, cx: &App) -> Result<Rc<ChartData>, SharedString> {
        self.results.read(cx).chart()
    }

    /// What the result is shown as, if it is shown as a chart.
    pub fn chart_shown(&self, cx: &App) -> Option<ChartView> {
        self.results.read(cx).chart_shown()
    }

    /// The editor's ranges shown as comments (tests).
    #[cfg(test)]
    pub fn comments(&self, cx: &App) -> Vec<std::ops::Range<usize>> {
        self.editor.read(cx).comments(cx)
    }

    /// The view the result is set to, even before a run (tests).
    #[cfg(test)]
    pub fn chart_view(&self, cx: &App) -> Option<ChartView> {
        self.results.read(cx).chart_view()
    }

    /// Shows the result as `view`, or as the grid (`None`).
    pub fn set_chart_view(&mut self, view: Option<ChartView>, cx: &mut Context<Self>) {
        self.results.update(cx, |r, cx| r.set_chart_view(view, cx));
    }

    /// Timing and row count of the last run, shown in the workspace status bar.
    pub fn status_text(&self, cx: &App) -> SharedString {
        self.results.read(cx).status_text(cx)
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let run_or_cancel = if self.is_running(cx) {
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
                .tooltip("Run the selection or the statement at the cursor (⌘↵); run all: ⌘⇧↵")
                .on_click(cx.listener(|this, _, window, cx| this.run(&RunQuery, window, cx)))
                .into_any_element()
        };
        let conn_label: SharedString = match self.conn.as_ref().map(|c| c.read(cx)) {
            Some(c) => {
                let status = match &c.status {
                    ConnStatus::Connected => String::new(),
                    ConnStatus::Connecting => " · connecting…".into(),
                    ConnStatus::Failed(_) => " · not connected".into(),
                };
                format!("{} · {}{status}", c.name, c.driver.label()).into()
            }
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
            .child({
                let hidden = self.editor_hidden(cx);
                Button::new("toggle-editor")
                    .icon(if hidden {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronUp
                    })
                    .ghost()
                    .small()
                    .tooltip(if hidden { "Show editor" } else { "Hide editor" })
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_editor(window, cx)))
            })
            .child({
                let hint = self.bookmark_hint.filter(|_| self.bookmark_id.is_none());
                let button = Button::new("bookmark")
                    .icon(Icon::new(Lucide::Bookmark))
                    .label(match (&self.bookmark_id, hint) {
                        (Some(_), _) => "Update bookmark".to_string(),
                        (None, Some(n)) => format!("Bookmark · run {n}×"),
                        (None, None) => "Bookmark".to_string(),
                    })
                    .small()
                    .tooltip(if hint.is_some() {
                        "You run this query often — save it as a bookmark (⌘D)"
                    } else {
                        "Save query as bookmark (⌘D)"
                    });
                let button = if hint.is_some() {
                    button.outline()
                } else {
                    button.ghost()
                };
                button.on_click(
                    cx.listener(|this, _, window, cx| this.bookmark(&BookmarkQuery, window, cx)),
                )
            })
    }
}

impl Render for QueryTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let editor = self.editor.read(cx).state().clone();
        v_flex()
            .size_full()
            .on_action(cx.listener(Self::run))
            .on_action(cx.listener(Self::run_all))
            .on_action(cx.listener(Self::bookmark))
            .child(self.render_toolbar(cx))
            .child(
                div().flex_1().min_h_0().child(
                    v_resizable(("split", self.id.0 as usize))
                        .with_state(&self.split)
                        .on_resize(move |_, window, cx| {
                            let _ = this.update(cx, |tab, cx| tab.split_resized(window, cx));
                        })
                        .child(
                            resizable_panel()
                                .size(px(EDITOR_HEIGHT))
                                .size_range(px(0.)..Pixels::MAX)
                                .flex_none()
                                .child(
                                    div()
                                        .key_context("QueryEditor")
                                        .size_full()
                                        .overflow_hidden()
                                        .child(Editor::new(&editor).h_full().bordered(false)),
                                ),
                        )
                        .child(
                            resizable_panel().child(
                                div()
                                    .size_full()
                                    .overflow_hidden()
                                    .child(self.results.clone()),
                            ),
                        ),
                ),
            )
    }
}

impl Focusable for QueryTab {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).state().read(cx).focus_handle(cx)
    }
}
