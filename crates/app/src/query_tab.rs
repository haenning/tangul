//! One query tab: SQL editor on top, results below.

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::base::input::{Diagnostic, DiagnosticSeverity};
use gpui_kit::component::input::RopeExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{CompletionProvider, Editor, EditorState, InputEvent},
    resizable::{ResizableState, resizable_panel, v_resizable},
    table::{DataTable, TableState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rusdb_assist::analyze::statements;
use rusdb_assist::chart::{ChartData, ChartStyle, chart_data};
use rusdb_assist::lexer::tokenize;
use rusdb_assist::related::RelatedLink;
use rusdb_core::{ConnId, DriverKind, QueryId, TabId};
use rusdb_engine::{Command, EngineHandle, ExecEvent, Problem, QueryEvent};

/// Pause in typing after which the editor is checked on the server.
const VALIDATE_AFTER: Duration = Duration::from_secs(1);
/// Limits per check; larger scripts are only checked in part.
const MAX_VALIDATE_STATEMENTS: usize = 50;
const MAX_VALIDATE_BYTES: usize = 100_000;
/// Editor height a new tab starts with.
const EDITOR_HEIGHT: f32 = 240.;
/// Dragging the editor below this height hides it completely.
const EDITOR_SNAP_HIDE: f32 = 60.;

use crate::{
    BookmarkQuery, RunQuery, assets::Lucide, chart::ResultChart, results::ResultsDelegate,
};

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
    OpenRelated { title: String, sql: String },
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
    /// Editor/results split. The editor is the first panel; height 0 means hidden.
    split: Entity<ResizableState>,
    /// Editor height to come back to when it is shown again after being hidden.
    editor_restore: Pixels,
    /// Times the last query was run successfully, when it is worth bookmarking.
    pub bookmark_hint: Option<u32>,
    /// Tab is pinned: no close button, and middle-click doesn't close it. Closing it
    /// from the context menu or with the shortcut still works.
    pub pinned: bool,
    engine: EngineHandle,
    editor: Entity<EditorState>,
    table: Entity<TableState<ResultsDelegate>>,
    status: Status,
    running: Option<QueryId>,
    /// Where the SQL of the next/current run starts in the editor (errors are
    /// reported relative to the executed text), and that text.
    pending_origin: Option<usize>,
    run_origin: Option<(usize, String)>,
    /// Bumped on every edit; validation results for older text are dropped.
    edit_generation: u64,
    /// The statements sent for the current generation's validation.
    validating: Vec<(usize, String)>,
    _validation: Task<()>,
    _editor_changes: Subscription,
    /// Session pinned to this tab because a transaction is open.
    in_transaction: bool,
    has_result_set: bool,
    truncated: bool,
    last_rows: Option<u64>,
    /// The last result as a chart, or why it cannot be one.
    chart: Result<Rc<ChartData>, SharedString>,
    /// The chart style picked in the status bar, shown instead of the grid when the
    /// result can be charted; `None` shows the grid. Kept across runs.
    chart_style: Option<ChartStyle>,
}

/// Why there is no chart before a result is complete.
const NO_RESULT: &str = "Run a query that returns rows to chart them";

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
        let editor_changes = cx.subscribe(&editor, |this: &mut Self, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                this.schedule_validation(cx);
            }
        });
        let delegate = ResultsDelegate::new(cx.weak_entity());
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .sortable(false)
                .col_movable(false)
                // Rows are selected by the delegate (multi-selection).
                .row_selectable(false)
        });
        Self {
            id,
            title: format!("Query {}", id.0).into(),
            conn,
            bookmark_id: None,
            split: cx.new(|_| ResizableState::default()),
            editor_restore: px(EDITOR_HEIGHT),
            bookmark_hint: None,
            pinned: false,
            pending_origin: None,
            run_origin: None,
            edit_generation: 0,
            validating: Vec::new(),
            _validation: Task::ready(()),
            _editor_changes: editor_changes,
            engine,
            editor,
            table,
            status: Status::Idle,
            running: None,
            in_transaction: false,
            has_result_set: false,
            truncated: false,
            last_rows: None,
            chart: Err(NO_RESULT.into()),
            chart_style: None,
        }
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
            self.editor.update(cx, |e, cx| e.focus(window, cx));
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
        editor.update(cx, |e, cx| e.focus(window, cx));
    }

    pub fn set_completion(&self, provider: Rc<dyn CompletionProvider>, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |e, _| e.lsp_mut().completion_provider = Some(provider));
    }

    pub fn sql(&self, cx: &App) -> String {
        self.editor.read(cx).value().to_string()
    }

    pub fn result_columns(&self, cx: &App) -> Vec<String> {
        self.table.read(cx).delegate().column_names()
    }

    /// Foreign-key links offered in the result rows' context menu.
    pub fn set_related(&mut self, links: Vec<RelatedLink>, cx: &mut Context<Self>) {
        self.table
            .update(cx, |t, _| t.delegate_mut().related = links);
    }

    /// Rows returned by the last result set, or affected by the last statement.
    pub fn row_count(&self, cx: &App) -> Option<u64> {
        if self.has_result_set {
            Some(self.table.read(cx).delegate().result.row_count() as u64)
        } else {
            self.last_rows
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub fn in_transaction(&self) -> bool {
        self.in_transaction
    }

    /// Switching connection releases a pinned session (the engine rolls it back).
    pub fn set_conn(&mut self, conn: TabConn, cx: &mut Context<Self>) {
        // The same SQL can be valid on one connection and not on another.
        self.schedule_validation(cx);
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
        let (sql, origin) = if selected.trim().is_empty() {
            (editor.value(), 0)
        } else {
            (selected, editor.selected_range().start)
        };
        if sql.trim().is_empty() {
            return;
        }
        self.pending_origin = Some(origin);
        cx.emit(TabEvent::Run(sql.to_string()));
    }

    fn bookmark(&mut self, _: &BookmarkQuery, _: &mut Window, cx: &mut Context<Self>) {
        let sql = self.sql(cx);
        if !sql.trim().is_empty() {
            cx.emit(TabEvent::Bookmark(sql));
        }
    }

    pub fn toggle_pin(&mut self) {
        self.pinned = !self.pinned;
    }

    /// Called by the workspace once the query was sent to the engine.
    pub fn started(&mut self, query: QueryId, sql: &str, cx: &mut Context<Self>) {
        // Locate the executed text in the editor, to put error markers in place.
        let text = self.sql(cx);
        let origin = self
            .pending_origin
            .take()
            .filter(|&o| text.get(o..).is_some_and(|t| t.starts_with(sql)))
            .or_else(|| text.find(sql));
        self.run_origin = origin.map(|o| (o, sql.to_string()));
        self.set_error_marker(None, cx);
        self.running = Some(query);
        self.status = Status::Running;
        self.has_result_set = false;
        self.truncated = false;
        self.last_rows = None;
        self.chart = Err(NO_RESULT.into());
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
                error_position,
                in_transaction,
                cancelled,
            } => {
                if let (Some(message), Some(position)) = (&error, error_position) {
                    self.set_error_marker(Some((position, message.clone())), cx);
                }
                self.running = None;
                self.in_transaction = in_transaction;
                if error.is_none() && self.has_result_set {
                    let sql = self.run_origin.as_ref().map_or("", |(_, sql)| sql.as_str());
                    let result = &self.table.read(cx).delegate().result;
                    self.chart = chart_data(sql, result).map(Rc::new).map_err(Into::into);
                }
                self.status = Status::Done {
                    elapsed,
                    error: error.map(Into::into),
                    cancelled,
                };
            }
        }
        cx.notify();
    }

    /// Validates the editor once typing paused for [`VALIDATE_AFTER`].
    fn schedule_validation(&mut self, cx: &mut Context<Self>) {
        self.edit_generation += 1;
        let generation = self.edit_generation;
        self._validation = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(VALIDATE_AFTER).await;
            let _ = this.update(cx, |tab, cx| tab.request_validation(generation, cx));
        });
    }

    fn request_validation(&mut self, generation: u64, cx: &mut Context<Self>) {
        if generation != self.edit_generation || self.conn.is_none() {
            return;
        }
        let text = self.sql(cx);
        self.validating = statements(&text)
            .into_iter()
            .filter(|&(s, e)| !text[s..e].trim().is_empty() && e - s <= MAX_VALIDATE_BYTES)
            .take(MAX_VALIDATE_STATEMENTS)
            .map(|(s, e)| (s, text[s..e].to_string()))
            .collect();
        if self.validating.is_empty() {
            self.set_markers(Vec::new(), cx);
            return;
        }
        cx.emit(TabEvent::Validate {
            generation,
            statements: self.validating.clone(),
        });
    }

    /// Marks the problems the server found, unless the text changed meanwhile.
    /// Errors that only say the statement is unfinished are left out: that is the
    /// normal state while typing.
    pub fn apply_validation(
        &mut self,
        generation: u64,
        problems: Vec<Problem>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.edit_generation {
            return;
        }
        let markers = problems
            .into_iter()
            .filter_map(|p| {
                let (offset, sql) = self.validating.get(p.statement)?;
                if is_incomplete(sql, &p) {
                    return None;
                }
                let span = error_span(sql, p.position.unwrap_or(0))?;
                Some((offset + span.start..offset + span.end, p.message))
            })
            .collect();
        self.set_markers(markers, cx);
    }

    /// Underlines the token at `position` (byte offset in the executed SQL) with the
    /// error as hover text; `None` clears. Editing clears it too (the editor resets
    /// diagnostics on every change).
    fn set_error_marker(&mut self, error: Option<(usize, String)>, cx: &mut Context<Self>) {
        let marker = error.and_then(|(position, message)| {
            let (origin, sql) = self.run_origin.as_ref()?;
            let span = error_span(sql, position)?;
            Some((origin + span.start..origin + span.end, message))
        });
        self.set_markers(marker.into_iter().collect(), cx);
    }

    /// Replaces the editor's error underlines (byte ranges in the editor text).
    fn set_markers(
        &mut self,
        markers: Vec<(std::ops::Range<usize>, String)>,
        cx: &mut Context<Self>,
    ) {
        self.editor.update(cx, |editor, cx| {
            let text = editor.text().clone();
            let Some(diagnostics) = editor.diagnostics_mut() else {
                return;
            };
            diagnostics.reset(&text);
            for (range, message) in markers {
                if range.end > text.len() {
                    continue;
                }
                diagnostics.push(Diagnostic {
                    range: text.offset_to_position(range.start)..text.offset_to_position(range.end),
                    severity: DiagnosticSeverity::Error,
                    message: message.into(),
                    ..Default::default()
                });
            }
            cx.notify();
        });
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

    /// Chart menu state for the status bar: the styles this result allows and
    /// whether a chart is suggested (grouped query), or why it can't be charted.
    pub fn chart_options(&self) -> Result<(Vec<ChartStyle>, bool), SharedString> {
        self.chart
            .as_ref()
            .map(|c| (c.styles(), c.grouped))
            .map_err(Clone::clone)
    }

    /// The style the result is shown in, if it is shown as a chart.
    pub fn chart_shown(&self) -> Option<ChartStyle> {
        let data = self.chart.as_ref().ok()?;
        self.chart_style.map(|s| data.style_or_default(Some(s)))
    }

    /// Shows the result as a chart in `style`, or as the grid (`None`).
    pub fn set_chart_style(&mut self, style: Option<ChartStyle>, cx: &mut Context<Self>) {
        self.chart_style = style;
        cx.notify();
    }

    /// All series in one chart, with a legend when there are several.
    fn render_chart(&self, data: &Rc<ChartData>, style: ChartStyle, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let colors = vec![
            theme.chart_1,
            theme.chart_2,
            theme.chart_3,
            theme.chart_4,
            theme.chart_5,
        ];
        let legend = (data.series.len() > 1).then(|| {
            h_flex()
                .gap_3()
                .flex_wrap()
                .children(data.series.iter().enumerate().map(|(i, s)| {
                    h_flex()
                        .gap_1()
                        .items_center()
                        .child(div().size_2().rounded_sm().bg(colors[i % colors.len()]))
                        .child(s.name.clone())
                }))
        });
        v_flex()
            .size_full()
            .p_3()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .children(legend)
            .when(data.unsorted && !style.is_bars(), |this| {
                this.child(format!(
                    "{} is not in ascending order; add ORDER BY to plot it over time",
                    data.x_name
                ))
            })
            .child(div().flex_1().min_h_0().child(ResultChart::new(
                "result-chart",
                data.clone(),
                style,
                colors,
            )))
            .into_any_element()
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
            Status::Done { .. }
                if let (Some(style), Ok(data)) = (self.chart_shown(), &self.chart) =>
            {
                self.render_chart(data, style, cx)
            }
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
        let this = cx.entity().downgrade();
        v_flex()
            .size_full()
            .on_action(cx.listener(Self::run))
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
                                        .child(Editor::new(&self.editor).h_full().bordered(false)),
                                ),
                        )
                        .child(
                            resizable_panel().child(
                                div()
                                    .size_full()
                                    .overflow_hidden()
                                    .child(self.render_results(cx)),
                            ),
                        ),
                ),
            )
    }
}

impl Focusable for QueryTab {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}

/// Whether an error just means the statement isn't finished yet (the normal state
/// while typing): errors at its very end, or about unterminated quotes.
fn is_incomplete(sql: &str, problem: &Problem) -> bool {
    let end = sql.trim_end().trim_end_matches(';').trim_end().len();
    let message = problem.message.to_lowercase();
    problem.position.is_some_and(|p| p >= end)
        || message.contains("at end of input")
        || message.contains("near '' at line")
        || message.contains("unterminated")
}

/// The token an error position points at (or the last token if the error is at the
/// end, e.g. an unfinished statement).
fn error_span(sql: &str, position: usize) -> Option<std::ops::Range<usize>> {
    let tokens: Vec<_> = tokenize(sql)
        .into_iter()
        .filter(|t| !t.is_trivia())
        .collect();
    tokens
        .iter()
        .find(|t| t.end > position)
        .or(tokens.last())
        .map(|t| t.start..t.end)
}

#[cfg(test)]
mod tests {
    use super::{Problem, error_span, is_incomplete};

    #[test]
    fn incomplete_statements_are_not_errors() {
        let p = |position, message: &str| Problem {
            statement: 0,
            position,
            message: message.into(),
        };
        let sql = "select * from ";
        assert!(is_incomplete(
            sql,
            &p(Some(14), "syntax error at end of input")
        ));
        // Position at the end, without the "end of input" wording.
        assert!(is_incomplete(
            "select * from users where;",
            &p(Some(25), "syntax error")
        ));
        assert!(is_incomplete(
            "select 'abc",
            &p(Some(7), "unterminated quoted string")
        ));
        assert!(!is_incomplete(
            "select * form users",
            &p(Some(9), "syntax error at or near \"form\"")
        ));
        assert!(!is_incomplete(
            "select nme from users",
            &p(None, "Unknown column 'nme'")
        ));
    }

    #[test]
    fn spans() {
        let sql = "select * form users";
        assert_eq!(error_span(sql, 9), Some(9..13));
        // Position in whitespace: the next token.
        assert_eq!(error_span(sql, 8), Some(9..13));
        // At the end: the last token.
        assert_eq!(error_span("select * from", 13), Some(9..13));
        assert_eq!(error_span("", 0), None);
    }
}
