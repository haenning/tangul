//! One query tab: SQL editor on top, results below.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::base::input::{Diagnostic, DiagnosticSeverity};
use gpui_kit::component::input::RopeExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{CompletionProvider, Editor, EditorState, InputEvent},
    notification::Notification,
    resizable::{ResizableState, resizable_panel, v_resizable},
    table::{DataTable, TableState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_assist::analyze::statements;
use tangul_assist::chart::{ChartData, ChartView, chart_data};
use tangul_assist::columns::ColumnFacts;
use tangul_assist::lexer::tokenize;
use tangul_assist::refine::{ColumnFilter, Refine, Sort, refinable, refined_sql};
use tangul_assist::{edit::RowEdit, related::RelatedLink};
use tangul_core::{ConnId, DriverKind, QueryId, TabId};
use tangul_engine::{Command, EngineHandle, ExecEvent, Problem, QueryEvent};

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
    BookmarkQuery, RunQuery, SaveFile,
    assets::Lucide,
    chart::{self, ResultChart},
    column_filter::FilterPanel,
    results::ResultsDelegate,
    settings,
};

/// Connection a tab runs against.
#[derive(Clone)]
pub struct TabConn {
    pub id: ConnId,
    pub name: SharedString,
    pub driver: DriverKind,
}

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
    OpenRelated { title: String, sql: String },
    /// Open SQL for the selected rows in a new tab on this tab's connection, without
    /// running it (UPDATE / DELETE statements).
    OpenSql { title: String, sql: String },
    /// Run a filtered / sorted version of the last result (`append: false`) or its
    /// next page (`append: true`). Not a run of its own.
    Refine { sql: String, append: bool },
    /// Open a table (the target of a foreign key) in a new tab, like clicking it in
    /// the schema tree.
    OpenTable { schema: String, table: String },
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
    /// The chart picked in the status bar, shown instead of the grid when the result
    /// can be charted; `None` shows the grid. Kept across runs.
    chart_view: Option<ChartView>,
    /// Curved instead of straight lines; starts straight, which is exact.
    smooth_lines: bool,
    /// Set for a file tab (no SQL, no results).
    file: Option<FileTab>,
    /// The last run's statement, when its result can be filtered, sorted and paged.
    base_sql: Option<String>,
    /// While a derived query runs (see [`TabEvent::Refine`]): whether it appends a
    /// page.
    derived: Option<bool>,
    /// Loading a further page failed (the loaded rows stay).
    page_error: Option<SharedString>,
}

/// Why there is no chart before a result is complete.
const NO_RESULT: &str = "Run a query that returns rows to chart them";

/// A file edited in a tab instead of SQL (`settings.json`): only the editor, `⌘S`
/// saves.
struct FileTab {
    path: PathBuf,
    /// The text as last read or saved, to show unsaved changes.
    saved: String,
    /// Why the saved text isn't applied (invalid settings), if it isn't.
    problem: Option<SharedString>,
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
        let editor_changes = cx.subscribe(&editor, |this: &mut Self, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                this.schedule_validation(cx);
            }
        });
        let tab = cx.weak_entity();
        let filter_panel = cx.new(|cx| FilterPanel::new(tab, window, cx));
        let delegate = ResultsDelegate::new(cx.weak_entity(), filter_panel);
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .sortable(false)
                .col_movable(false)
                .col_selectable(false)
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
            chart_view: None,
            smooth_lines: false,
            file: None,
            base_sql: None,
            derived: None,
            page_error: None,
        }
    }

    /// A tab editing the JSON file at `path` (settings): no connection, no results.
    pub fn new_file(
        id: TabId,
        path: PathBuf,
        engine: EngineHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut this = Self::new(id, None, "", engine, window, cx);
        this.editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("json")
                .line_number(true)
                .default_value(text.clone())
        });
        this._editor_changes = cx.subscribe(&this.editor, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });
        this.title = path
            .file_name()
            .map_or("settings.json".into(), |n| n.to_string_lossy().to_string())
            .into();
        this.file = Some(FileTab {
            path,
            saved: text,
            problem: None,
        });
        this
    }

    /// The file this tab edits, if it is a file tab.
    pub fn file_path(&self) -> Option<&Path> {
        self.file.as_ref().map(|f| f.path.as_path())
    }

    /// A file tab with changes that aren't saved yet.
    pub fn has_unsaved_changes(&self, cx: &App) -> bool {
        self.file
            .as_ref()
            .is_some_and(|f| f.saved != self.editor.read(cx).value().as_ref())
    }

    /// Writes a file tab's text. Settings that don't parse are saved anyway (it is
    /// the user's file) but not applied; the tab says why.
    fn save(&mut self, _: &SaveFile, window: &mut Window, cx: &mut Context<Self>) {
        self.save_file(window, cx);
    }

    /// Writes a file tab's text; `false` (with a notification) if that failed.
    pub fn save_file(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let text = self.editor.read(cx).value().to_string();
        let Some(file) = &mut self.file else {
            return false;
        };
        if let Err(e) = std::fs::write(&file.path, &text) {
            window.push_notification(
                Notification::error(format!("Could not save {}: {e}", file.path.display())),
                cx,
            );
            return false;
        }
        file.problem = settings::problem(&text).map(Into::into);
        file.saved = text;
        cx.notify();
        true
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

    /// What the result rows' context menu offers besides copying: foreign-key links
    /// (most used first) and UPDATE / DELETE statements.
    /// Also takes what the schema says about the result's columns (keys, types).
    pub fn set_row_actions(
        &mut self,
        links: Vec<RelatedLink>,
        edit: Option<RowEdit>,
        facts: Vec<Option<ColumnFacts>>,
        cx: &mut Context<Self>,
    ) {
        self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            d.related = links;
            d.edit = edit;
            d.set_facts(facts);
            cx.notify();
        });
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
        if self.file.is_some() {
            return;
        }
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
        if self.running.is_some() || self.file.is_some() {
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
        if self.file.is_some() {
            return;
        }
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
        self.base_sql = refinable(sql);
        self.derived = None;
        self.page_error = None;
        let refinable = self.base_sql.is_some();
        self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            d.clear();
            d.refine = Refine::default();
            d.refinable = refinable;
            t.refresh(cx);
            cx.notify();
        });
        cx.notify();
    }

    /// Called by the workspace once a derived query (filter, sort, next page) was
    /// sent. A new filter or sort replaces the rows; a page adds to them.
    pub fn started_derived(&mut self, query: QueryId, append: bool, cx: &mut Context<Self>) {
        self.running = Some(query);
        self.derived = Some(append);
        self.page_error = None;
        self.truncated = false;
        if !append {
            self.status = Status::Running;
            self.table.update(cx, |t, cx| {
                t.delegate_mut().clear_rows();
                cx.notify();
            });
        }
        cx.notify();
    }

    /// Filters `column` (or removes its filter) and runs the result again.
    pub fn set_filter(
        &mut self,
        column: usize,
        filter: Option<ColumnFilter>,
        cx: &mut Context<Self>,
    ) {
        if self.running.is_some() {
            return;
        }
        self.table.update(cx, |t, _| {
            let d = t.delegate_mut();
            match filter {
                Some(filter) => d.refine.set_filter(filter),
                None => d.refine.remove_filter(column),
            }
            d.close_filter();
        });
        self.run_refined(cx);
    }

    pub fn set_sort(&mut self, sort: Option<Sort>, cx: &mut Context<Self>) {
        if self.running.is_some() {
            return;
        }
        self.table.update(cx, |t, _| {
            let d = t.delegate_mut();
            d.refine.sort = sort;
            d.close_filter();
        });
        self.run_refined(cx);
    }

    fn clear_refine(&mut self, cx: &mut Context<Self>) {
        if self.running.is_some() {
            return;
        }
        self.table
            .update(cx, |t, _| t.delegate_mut().refine = Refine::default());
        self.run_refined(cx);
    }

    /// Runs the last statement again with the current filters and sort, from the
    /// first row.
    fn run_refined(&mut self, cx: &mut Context<Self>) {
        if let Some(sql) = self.refined_sql(0, cx) {
            cx.emit(TabEvent::Refine { sql, append: false });
        }
        cx.notify();
    }

    /// The next page, when the table is scrolled to the end of the loaded rows.
    pub fn load_more(&mut self, cx: &mut Context<Self>) {
        let d = self.table.read(cx).delegate();
        if !d.more || self.page_error.is_some() {
            return;
        }
        let offset = d.result.row_count();
        if let Some(sql) = self.refined_sql(offset, cx) {
            cx.emit(TabEvent::Refine { sql, append: true });
        }
    }

    /// The last statement filtered, sorted and cut to a page from `offset`. One row
    /// more than a page is asked for; the engine's row limit drops it and reports
    /// the result truncated, which says there are more.
    fn refined_sql(&self, offset: usize, cx: &App) -> Option<String> {
        if self.running.is_some() {
            return None;
        }
        let base = self.base_sql.as_ref()?;
        let driver = self.conn.as_ref()?.driver;
        let d = self.table.read(cx).delegate();
        let columns = d.column_names();
        let kinds: Vec<_> = (0..columns.len()).map(|c| d.kind(c)).collect();
        Some(refined_sql(
            base,
            &columns,
            &kinds,
            &d.refine,
            driver,
            offset,
            tangul_engine::ROW_LIMIT + 1,
        ))
    }

    fn cancel(&mut self, _: &ClickEvent, _: &mut Window, _: &mut Context<Self>) {
        if let Some(query) = self.running {
            self.engine.send(Command::Cancel { query });
        }
    }

    pub fn on_query_event(&mut self, event: QueryEvent, cx: &mut Context<Self>) {
        match event {
            // A derived query returns the same columns: keep them (widths, keys).
            QueryEvent::Exec(ExecEvent::ResultStart(cols))
                if self.derived.is_some()
                    && cols.len() == self.table.read(cx).delegate().result.columns().len() => {}
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
                let derived = self.derived.take();
                // Positions of a derived query are in the wrapped SQL, not the editor.
                if derived.is_none()
                    && let (Some(message), Some(position)) = (&error, error_position)
                {
                    self.set_error_marker(Some((position, message.clone())), cx);
                }
                self.running = None;
                self.in_transaction = in_transaction;
                let more = self.truncated && self.base_sql.is_some() && error.is_none();
                self.table.update(cx, |t, cx| {
                    t.delegate_mut().more = more;
                    cx.notify();
                });
                if derived == Some(true) {
                    // A failed page keeps the rows already loaded.
                    self.page_error = error.map(Into::into);
                    if let Status::Done { elapsed: e, .. } = &mut self.status {
                        *e = elapsed;
                    }
                    cx.notify();
                    return;
                }
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

    /// The last result as chart data (for the status-bar menu), or why it can't be
    /// charted.
    pub fn chart(&self) -> Result<Rc<ChartData>, SharedString> {
        self.chart.clone()
    }

    /// What the result is shown as, if it is shown as a chart.
    pub fn chart_shown(&self) -> Option<ChartView> {
        let data = self.chart.as_ref().ok()?;
        self.chart_view.map(|v| data.view_or_default(Some(v)))
    }

    /// Shows the result as `view`, or as the grid (`None`).
    pub fn set_chart_view(&mut self, view: Option<ChartView>, cx: &mut Context<Self>) {
        self.chart_view = view;
        cx.notify();
    }

    /// All series in one chart, with a legend when there are several.
    fn render_chart(
        &self,
        data: &ChartData,
        view: ChartView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let mut colors = vec![
            theme.chart_1,
            theme.chart_2,
            theme.chart_3,
            theme.chart_4,
            theme.chart_5,
        ];
        // A single column keeps the color it has among all of them.
        if let Some(i) = view.series {
            let n = colors.len();
            colors.rotate_left(i % n);
        }
        let style = view.style;
        let data = Rc::new(data.for_view(view));
        // Straight / smooth, for lines and areas only.
        let curve_switch = (!style.is_bars()).then(|| {
            let option = |id: &'static str, label: &'static str, smooth: bool| {
                Button::new(id)
                    .label(label)
                    .xsmall()
                    .ghost()
                    .selected(self.smooth_lines == smooth)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.smooth_lines = smooth;
                        cx.notify();
                    }))
            };
            h_flex()
                .gap_0p5()
                .child(option("straight-lines", "Straight", false))
                .child(option("smooth-lines", "Smooth", true))
        });
        let theme = cx.theme();
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
        let settings = settings::current(cx);
        let label_size = settings.chart_font_size();
        v_flex()
            .size_full()
            .p_3()
            .gap_2()
            .text_xs()
            .when_some(label_size, |this, size| this.text_size(px(size)))
            .when_some(settings.chart.font.family(), |this, family| {
                this.font_family(SharedString::from(family.to_string()))
            })
            .text_color(theme.muted_foreground)
            .when(curve_switch.is_some() || legend.is_some(), |this| {
                this.child(
                    h_flex()
                        .gap_4()
                        .items_center()
                        .children(curve_switch)
                        .children(legend),
                )
            })
            .when(data.unsorted && !style.is_bars(), |this| {
                this.child(format!(
                    "{} is not in ascending order; add ORDER BY to plot it over time",
                    data.x_name
                ))
            })
            .child(div().flex_1().min_h_0().child(ResultChart::new(
                "result-chart",
                data,
                style,
                colors,
                self.smooth_lines,
                label_size.unwrap_or(chart::LABEL_SIZE),
            )))
            .into_any_element()
    }

    /// The results, with the active filters and sort above them.
    fn render_results(&self, cx: &mut Context<Self>) -> AnyElement {
        let chips = self.render_refine_bar(cx);
        let results = self.render_result_body(cx);
        match chips {
            Some(chips) => v_flex()
                .size_full()
                .child(chips)
                .child(div().flex_1().min_h_0().child(results))
                .into_any_element(),
            None => results,
        }
    }

    /// One chip per filter and for the sort; clicking a chip's ✕ removes it.
    fn render_refine_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.table.read(cx).delegate();
        if d.refine.is_empty() || matches!(self.status, Status::Idle) {
            return None;
        }
        let names = d.column_names();
        let name = |c: usize| names.get(c).cloned().unwrap_or_default();
        let sort = d.refine.sort.map(|sort| {
            let dir = if sort.descending { "↓" } else { "↑" };
            let label = format!("sorted by {} {dir}", name(sort.column));
            (SharedString::from(label), None)
        });
        let chips = d
            .refine
            .filters
            .iter()
            .map(|f| (f.describe(&name(f.column)).into(), Some(f.column)))
            .chain(sort);
        let theme = cx.theme();
        Some(
            h_flex()
                .px_2()
                .py_1()
                .gap_1()
                .flex_wrap()
                .items_center()
                .border_b_1()
                .border_color(theme.border)
                .text_xs()
                .child(
                    Icon::new(Lucide::Funnel)
                        .xsmall()
                        .text_color(theme.muted_foreground),
                )
                .children(chips.enumerate().map(
                    |(i, (label, filter)): (usize, (SharedString, Option<usize>))| {
                        h_flex()
                            .pl_2()
                            .gap_0p5()
                            .items_center()
                            .rounded(theme.radius)
                            .bg(theme.secondary)
                            .child(label)
                            .child(
                                Button::new(("refine-chip", i))
                                    .icon(IconName::Close)
                                    .xsmall()
                                    .ghost()
                                    .tooltip(if filter.is_some() {
                                        "Remove filter"
                                    } else {
                                        "Remove sort"
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| match filter {
                                        Some(column) => this.set_filter(column, None, cx),
                                        None => this.set_sort(None, cx),
                                    })),
                            )
                    },
                ))
                .child(
                    Button::new("refine-clear")
                        .label("Clear all")
                        .xsmall()
                        .ghost()
                        .on_click(cx.listener(|this, _, _, cx| this.clear_refine(cx))),
                )
                .into_any_element(),
        )
    }

    fn render_result_body(&self, cx: &mut Context<Self>) -> AnyElement {
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
                if let (Some(view), Ok(data)) = (self.chart_shown(), &self.chart) =>
            {
                self.render_chart(data, view, cx)
            }
            _ => DataTable::new(&self.table)
                .stripe(true)
                .xsmall()
                .into_any_element(),
        }
    }

    /// Timing and row count of the last run, shown in the workspace status bar.
    pub fn status_text(&self, cx: &App) -> SharedString {
        let d = self.table.read(cx).delegate();
        let rows = d.result.row_count();
        let (more, filtered) = (d.more, !d.refine.is_empty());
        if self.derived == Some(true) {
            return format!("{rows} rows · loading more…").into();
        }
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
                if filtered {
                    s.push_str(" · filtered");
                }
                if *cancelled {
                    s.push_str(" · cancelled");
                }
                if let Some(e) = &self.page_error {
                    s.push_str(&format!(" · loading more failed: {e}"));
                } else if more {
                    s.push_str(" · scroll for more");
                } else if self.truncated {
                    s.push_str(&format!(" · limited to {} rows", tangul_engine::ROW_LIMIT));
                }
                s.into()
            }
        }
    }
}

impl QueryTab {
    /// A file tab: path, save state and the editor over the whole tab.
    fn render_file(&self, file: &FileTab, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let unsaved = self.has_unsaved_changes(cx);
        let (status, color) = match (&file.problem, unsaved) {
            (_, true) => ("Unsaved changes".into(), theme.muted_foreground),
            (Some(problem), false) => (format!("Saved, not applied: {problem}"), theme.danger),
            (None, false) => (
                "Saved; changes apply right away".into(),
                theme.muted_foreground,
            ),
        };
        v_flex()
            .size_full()
            .on_action(cx.listener(Self::save))
            .child(
                h_flex()
                    .h_9()
                    .px_2()
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_xs()
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(file.path.display().to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(color)
                            .child(status),
                    )
                    .child(
                        Button::new("save-file")
                            .label("Save")
                            .small()
                            .when(unsaved, |b| b.primary())
                            .when(!unsaved, |b| b.ghost())
                            .tooltip("Save (⌘S)")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.save(&SaveFile, window, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .key_context("QueryEditor")
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(Editor::new(&self.editor).h_full().bordered(false)),
            )
            .into_any_element()
    }
}

impl Render for QueryTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(file) = &self.file {
            return self.render_file(file, cx);
        }
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
            .into_any_element()
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
