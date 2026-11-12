//! A query's result, shown as a grid or a chart, with its run state: running,
//! finished, filtered, sorted and paged. It knows nothing about tabs: whatever runs
//! queries (a tab now; a board tile or the assistant later) feeds it the engine's
//! events and acts on its [`ResultEvent`]s.

mod filter;
mod grid;
mod xlsx;

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt as _,
    WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{PopupMenu, PopupMenuItem},
    notification::Notification,
    table::{DataTable, TableEvent, TableState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_assist::chart::{
    ChartData, ChartStyle, ChartView, chart_data, old_layout, old_layout_message,
};
use tangul_assist::columns::ColumnFacts;
use tangul_assist::dialect::Dialect;
use tangul_assist::refine::{ColumnFilter, ColumnKind, Refine, Sort, refinable, refined_sql};
use tangul_assist::{edit::RowEdit, related::RelatedLink};
use tangul_core::QueryId;
use tangul_engine::{ExecEvent, QueryEvent, QueryOutput};

use crate::assets::{Lucide, style_icon};
use crate::chart::{self, Pin, ResultChart};
use crate::settings;
use crate::ui::{ErrorText, Severity, alert_card};
use crate::{
    ClearMark, CopyMark, ExtendMarkDown, ExtendMarkLeft, ExtendMarkRight, ExtendMarkUp,
    InspectValue, MarkAll,
};
use filter::FilterPanel;
use grid::ResultsDelegate;

/// Shows a result as the view picked (`None`: the table).
pub type SetView = Rc<dyn Fn(Option<ChartView>, &mut App)>;

/// The table and every chart style `data` fits, `shown` checked. `set` applies
/// the pick. The one menu of views, for the status bar and board tiles alike.
pub fn chart_menu(
    mut menu: PopupMenu,
    data: &ChartData,
    shown: Option<ChartView>,
    set: SetView,
    _: &mut Window,
    _: &mut Context<PopupMenu>,
) -> PopupMenu {
    let item = |label: String, view: Option<ChartView>| {
        let set = set.clone();
        PopupMenuItem::new(label)
            .icon(Icon::new(style_icon(view.map(|v| v.style))))
            .checked(shown.map(|v| v.style) == view.map(|v| v.style))
            .on_click(move |_, _, cx| set(view, cx))
    };
    menu = menu.item(item("Table".into(), None)).separator();
    for style in data.styles() {
        let view = ChartView::new(style);
        menu = menu.item(item(data.view_label(view), Some(view)));
    }
    menu
}

/// What the result asks of whoever runs its queries.
#[derive(Clone)]
pub enum ResultEvent {
    /// Open (and run) a query for rows related to the selected ones, on the same
    /// connection.
    OpenRelated { title: String, sql: String },
    /// Open SQL for the selected rows without running it (UPDATE / DELETE).
    OpenSql { title: String, sql: String },
    /// Open a table (the target of a foreign key), like clicking it in the schema.
    OpenTable { schema: String, table: String },
    /// Run a filtered / sorted version of the last result (`append: false`) or its
    /// next page (`append: true`). Not a run of its own.
    Refine { sql: String, append: bool },
    /// A run (not a refinement) failed at `position` in its SQL.
    Failed { message: String, position: usize },
    /// The inspected cell (A) changed; `open` asks to show the value viewer.
    Inspect { cell: CellValue, open: bool },
    /// Space: open or close the viewer (opening shows the selected cell, see
    /// [`ResultView::inspect_selected`]).
    ToggleViewer,
    /// A cell to compare with the inspected one (B).
    Compare(CellValue),
}

/// A cell's value as it was when picked: it stays shown after the result changes.
#[derive(Clone)]
pub struct CellValue {
    pub column: String,
    /// Row number, from 1.
    pub row: usize,
    pub value: Option<String>,
    /// What the schema says the column is, if it is a table column.
    pub data_type: Option<String>,
    /// For querying rows by this value; `None` when the result can't be queried
    /// again (not a single SELECT).
    pub rows: Option<RowsSource>,
}

/// The result a value came from, to query its rows by that value.
#[derive(Clone)]
pub struct RowsSource {
    /// The result's statement (see `refinable`).
    pub base: String,
    pub columns: Vec<String>,
    pub kinds: Vec<ColumnKind>,
    pub column: usize,
    pub dialect: &'static dyn Dialect,
}

/// A file "Save as" writes: the name the save dialog suggests, and what goes in.
struct SaveFile {
    name: String,
    contents: Contents,
}

enum Contents {
    Bytes(Vec<u8>),
    /// Made into a workbook off the UI thread.
    Excel(xlsx::Sheet),
}

enum Status {
    Idle,
    /// Since then.
    Running(Instant),
    Done {
        elapsed: Duration,
        error: Option<SharedString>,
        cancelled: bool,
    },
}

/// Why there is no chart before a result is complete.
const NO_RESULT: &str = "Run a query that returns rows to chart them";

pub struct ResultView {
    table: Entity<TableState<ResultsDelegate>>,
    status: Status,
    running: Option<QueryId>,
    /// The statement of the current run (charts read its ORDER BY).
    sql: String,
    has_result_set: bool,
    truncated: bool,
    last_rows: Option<u64>,
    /// The last result as a chart, or why it cannot be one.
    chart: Result<Rc<ChartData>, SharedString>,
    /// The chart as drawn (`ChartData::drawn`), kept while the data, style and
    /// `chart.max_series` stay: not made again on every frame.
    drawn: RefCell<Option<DrawnChart>>,
    /// The chart picked in the status bar, shown instead of the grid when the result
    /// can be charted; `None` shows the grid. Kept across runs.
    chart_view: Option<ChartView>,
    /// A chart asked for before the result is known (a proposal from the
    /// assistant): resolved against the next result (the grid when it doesn't
    /// fit), then dropped.
    chart_request: Option<ChartStyle>,
    /// The last result has the old chart layout (a label column and several
    /// number columns): a stored chart from then shows why it isn't drawn.
    old_result: bool,
    /// Curved instead of straight lines; starts straight, which is exact.
    smooth_lines: bool,
    /// The chart row a click pinned: its values listed to scroll and copy, until
    /// Esc or another click.
    chart_pin: Option<ChartPin>,
    /// Where the chart was drawn last (clicks are mapped to its rows).
    chart_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Takes Esc while a row is pinned.
    pin_focus: FocusHandle,
    /// The pointer is on the legend's "+N more": every series is listed.
    legend_more: bool,
    /// How many series the legend's one line has room for (measured as it is
    /// drawn; the rest behind "+N more").
    legend_fit: Rc<Cell<usize>>,
    /// The last run's statement, when its result can be filtered, sorted and paged.
    base_sql: Option<String>,
    /// While a derived query runs (see [`ResultEvent::Refine`]): whether it appends
    /// a page.
    derived: Option<bool>,
    /// Loading a further page failed (the loaded rows stay).
    page_error: Option<SharedString>,
    /// The run's or the page's error, to select and copy (only one at a time).
    error_text: Entity<ErrorText>,
    /// SQL of the connection the results come from, for filtering, sorting, paging.
    dialect: Option<&'static dyn Dialect>,
    /// Redraws the running time while it counts up.
    _count_up: Task<()>,
    _table_events: Subscription,
}

impl EventEmitter<ResultEvent> for ResultView {}

/// A pinned chart row, for the chart it was pinned on only.
struct ChartPin {
    pin: Pin,
    view: ChartView,
    source: Weak<ChartData>,
}

impl ResultView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let view = cx.weak_entity();
        let filter_panel = cx.new(|cx| FilterPanel::new(view.clone(), window, cx));
        let delegate = ResultsDelegate::new(view, filter_panel);
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .sortable(false)
                .col_movable(false)
                .col_selectable(false)
                // Rows are selected by the delegate (multi-selection); the table's
                // own selection is a cell (the inspected one, A, is a mark of its
                // own: see `inspect`).
                .row_selectable(false)
                .cell_selectable(true)
                .row_header(false)
        });
        let table_events = cx.subscribe(&table, Self::on_table_event);
        Self {
            table,
            status: Status::Idle,
            running: None,
            sql: String::new(),
            has_result_set: false,
            truncated: false,
            last_rows: None,
            chart: Err(NO_RESULT.into()),
            drawn: RefCell::default(),
            chart_view: None,
            chart_request: None,
            old_result: false,
            smooth_lines: false,
            chart_pin: None,
            chart_bounds: Rc::default(),
            pin_focus: cx.focus_handle(),
            legend_more: false,
            legend_fit: Rc::new(Cell::new(usize::MAX)),
            base_sql: None,
            derived: None,
            page_error: None,
            error_text: cx.new(|cx| ErrorText::new(window, cx)),
            dialect: None,
            _count_up: Task::ready(()),
            _table_events: table_events,
        }
    }

    /// Asks where to save `file` (starting in Downloads) and writes it there, off
    /// the UI thread; a failure is a notification.
    fn save(&mut self, file: SaveFile, window: &mut Window, cx: &mut Context<Self>) {
        let path = cx.prompt_for_new_path(&tangul_data::config::save_dir(), Some(&file.name));
        cx.spawn_in(window, async move |_, cx| {
            let path = match path.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(e)) => {
                    let message = format!("Could not open the save dialog: {e}");
                    let _ = cx.update(|window, cx| {
                        window.push_notification(Notification::error(message), cx)
                    });
                    return;
                }
            };
            let written = cx
                .background_spawn(async move {
                    let bytes = match file.contents {
                        Contents::Bytes(bytes) => bytes,
                        Contents::Excel(sheet) => xlsx::xlsx(&sheet)?,
                    };
                    std::fs::write(&path, bytes)
                        .map_err(|e| format!("Could not save {}: {e}", path.display()))
                })
                .await;
            if let Err(message) = written {
                let _ = cx.update(|window, cx| {
                    window.push_notification(Notification::error(message), cx)
                });
            }
        })
        .detach();
    }

    fn on_table_event(
        &mut self,
        _: Entity<TableState<ResultsDelegate>>,
        event: &TableEvent,
        cx: &mut Context<Self>,
    ) {
        // A click or the arrow keys only select (the keys move the mark); the
        // viewer changes on a double click, Space or "Inspect value".
        match *event {
            TableEvent::DoubleClickedCell(row, col) => self.inspect(row, col, true, cx),
            TableEvent::SelectCell(row, col) => self.table.update(cx, |t, cx| {
                if t.delegate_mut().cursor_moved(row, col) {
                    cx.notify();
                }
            }),
            _ => {}
        }
    }

    /// Makes `row`, `col` the inspected cell (A) and says so.
    fn inspect(&mut self, row: usize, col: usize, open: bool, cx: &mut Context<Self>) {
        self.table.update(cx, |t, cx| {
            t.delegate_mut().inspected = Some((row, col));
            cx.notify();
        });
        if let Some(cell) = self.cell_value(row, col, cx) {
            cx.emit(ResultEvent::Inspect { cell, open });
        }
    }

    /// Space: opens or closes the value viewer (like Quick Look).
    fn inspect_action(&mut self, _: &InspectValue, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(ResultEvent::ToggleViewer);
    }

    /// ⌘/Ctrl+C: the mark (else the selected cell) to the clipboard, without headers.
    fn copy_mark_action(&mut self, _: &CopyMark, _: &mut Window, cx: &mut Context<Self>) {
        let table = self.table.read(cx);
        match table.delegate().copy_text(table.selected_cell()) {
            Some(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            None => cx.propagate(),
        }
    }

    /// ⌘/Ctrl+A: marks every column and row.
    fn mark_all_action(&mut self, _: &MarkAll, _: &mut Window, cx: &mut Context<Self>) {
        self.table.update(cx, |t, cx| {
            t.delegate_mut().mark_all();
            cx.notify();
        });
    }

    /// Esc: clears the mark; without one the table's own Esc runs.
    /// Esc (after the viewer's): clears the mark and the table's selected cell
    /// with it (that one is never drawn); with neither the table's own Esc runs.
    fn clear_mark_action(&mut self, _: &ClearMark, _: &mut Window, cx: &mut Context<Self>) {
        let selected = self.table.read(cx).selected_cell().is_some();
        if self.clear_grid_mark(cx) || selected {
            self.table.update(cx, |t, cx| t.clear_selection(cx));
            cx.notify();
        } else {
            cx.propagate();
        }
    }

    fn clear_grid_mark(&mut self, cx: &mut Context<Self>) -> bool {
        self.table.update(cx, |t, cx| {
            let cleared = t.delegate_mut().clear_mark();
            cx.notify();
            cleared
        })
    }

    /// ⇧+arrows: moves the mark's end, scrolling only when it leaves the view.
    fn extend_mark(&mut self, d_row: isize, d_col: isize, cx: &mut Context<Self>) {
        self.table.update(cx, |t, cx| {
            let from = t.selected_cell();
            if let Some((row, col)) = t.delegate_mut().extend_mark(from, d_row, d_col) {
                let visible = t.visible_range();
                let (show_row, show_col) = (
                    !visible.rows().contains(&row),
                    !visible.cols().contains(&col),
                );
                if d_row != 0 && show_row {
                    t.scroll_to_row(row, cx);
                }
                if d_col != 0 && show_col {
                    t.scroll_to_col(col, cx);
                }
            }
            cx.notify();
        });
    }

    /// The viewer opens on Space: the selected cell becomes A. False without one.
    pub fn inspect_selected(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((row, col)) = self.table.read(cx).selected_cell() else {
            return false;
        };
        self.inspect(row, col, true, cx);
        true
    }

    /// The grid's focus (Esc in the value viewer returns to it).
    pub fn grid_focus(&self, cx: &App) -> FocusHandle {
        self.table.read(cx).focus_handle(cx)
    }

    /// "Inspect value" in the menu: selects the cell (A) and shows it.
    pub(super) fn inspect_cell(&mut self, row: usize, col: usize, cx: &mut Context<Self>) {
        self.table.update(cx, |t, cx| {
            t.delegate_mut().selecting(row, col);
            t.set_selected_cell(row, col, cx);
        });
        self.inspect(row, col, true, cx);
    }

    /// ⌥-click or "Compare with A": `row`, `col` is to be compared with the
    /// inspected cell (B).
    pub(super) fn compare(&mut self, row: usize, col: usize, cx: &mut Context<Self>) {
        self.table.update(cx, |t, cx| {
            t.delegate_mut().compared = Some((row, col));
            cx.notify();
        });
        if let Some(cell) = self.cell_value(row, col, cx) {
            cx.emit(ResultEvent::Compare(cell));
        }
    }

    /// The value viewer shows another cell as A or B now: drops this grid's mark.
    pub fn clear_mark(&mut self, compared: bool, cx: &mut Context<Self>) {
        self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            if compared {
                d.compared = None;
            } else {
                d.inspected = None;
            }
            cx.notify();
        });
    }

    fn cell_value(&self, row: usize, col: usize, cx: &App) -> Option<CellValue> {
        let d = self.table.read(cx).delegate();
        let column = d.result.columns().get(col)?.name.clone();
        let columns = d.column_names();
        let rows = self
            .base_sql
            .clone()
            .zip(self.dialect)
            .map(|(base, dialect)| RowsSource {
                base,
                kinds: (0..columns.len()).map(|c| d.kind(c)).collect(),
                columns,
                column: col,
                dialect,
            });
        Some(CellValue {
            column,
            row: row + 1,
            value: d.result.cell(row, col).map(str::to_string),
            data_type: d.facts.get(col).cloned().flatten().map(|f| f.data_type),
            rows,
        })
    }

    pub fn set_dialect(&mut self, dialect: &'static dyn Dialect) {
        self.dialect = Some(dialect);
    }

    /// The query running now, if any.
    pub fn running(&self) -> Option<QueryId> {
        self.running
    }

    /// Whether a run is over and its result (or why it has none) is shown: not
    /// before the first run, nor while one runs.
    pub fn finished(&self) -> bool {
        matches!(self.status, Status::Done { .. })
    }

    /// The cell at `row`, `col` of the result shown (tests).
    #[cfg(test)]
    pub fn cell(&self, row: usize, col: usize, cx: &App) -> Option<String> {
        self.table.read(cx).delegate().cell(row, col)
    }

    /// What is marked in the grid (tests).
    #[cfg(test)]
    pub fn mark(&self, cx: &App) -> tangul_assist::mark::Mark {
        self.table.read(cx).delegate().mark()
    }

    pub fn columns(&self, cx: &App) -> Vec<String> {
        self.table.read(cx).delegate().column_names()
    }

    /// Rows returned by the last result set, or affected by the last statement.
    pub fn row_count(&self, cx: &App) -> Option<u64> {
        if self.has_result_set {
            Some(self.table.read(cx).delegate().result.row_count() as u64)
        } else {
            self.last_rows
        }
    }

    /// What the rows' context menu offers besides copying: foreign-key links (most
    /// used first) and UPDATE / DELETE statements. Also takes what the schema says
    /// about the result's columns (keys, types).
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

    /// `sql` was sent to the engine as `query`: a run of its own.
    pub fn started(&mut self, query: QueryId, sql: &str, cx: &mut Context<Self>) {
        self.running = Some(query);
        self.sql = sql.to_string();
        self.set_running(cx);
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

    /// Running from now: the status bar counts the time up.
    fn set_running(&mut self, cx: &mut Context<Self>) {
        self.status = Status::Running(Instant::now());
        self._count_up = crate::ui::count_up(cx, |v| matches!(v.status, Status::Running(_)));
    }

    /// The result can't be sorted, filtered or paged by queries around it.
    pub fn not_refinable(&mut self, cx: &mut Context<Self>) {
        self.base_sql = None;
        self.table.update(cx, |t, cx| {
            t.delegate_mut().refinable = false;
            cx.notify();
        });
    }

    /// A derived query (filter, sort, next page) was sent. A new filter or sort
    /// replaces the rows; a page adds to them.
    pub fn started_derived(&mut self, query: QueryId, append: bool, cx: &mut Context<Self>) {
        self.running = Some(query);
        self.derived = Some(append);
        self.page_error = None;
        self.truncated = false;
        if !append {
            self.set_running(cx);
            self.table.update(cx, |t, cx| {
                t.delegate_mut().clear_rows();
                cx.notify();
            });
        }
        cx.notify();
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
                cancelled,
                ..
            } => {
                let derived = self.derived.take();
                // Positions of a derived query are in the wrapped SQL, not the run's.
                if derived.is_none()
                    && let (Some(message), Some(position)) = (&error, error_position)
                {
                    cx.emit(ResultEvent::Failed {
                        message: message.clone(),
                        position,
                    });
                }
                self.running = None;
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
                    let result = &self.table.read(cx).delegate().result;
                    self.chart = chart_data(&self.sql, result, self.truncated)
                        .map(Rc::new)
                        .map_err(Into::into);
                    self.old_result = old_layout(result);
                    if let Some(style) = self.chart_request.take() {
                        // A result that can't be a chart shows the grid.
                        self.chart_view = self
                            .chart
                            .as_ref()
                            .ok()
                            .map(|data| data.view_or_default(Some(ChartView::new(style))));
                    }
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

    /// Shows the whole result of what [`ResultView::started`] (or
    /// [`ResultView::started_derived`]) began, which ran elsewhere: as if the engine
    /// had streamed it. The last result set is shown.
    pub fn replay(&mut self, output: Result<QueryOutput, String>, cx: &mut Context<Self>) {
        let (elapsed, error) = match output {
            Ok(mut out) => {
                if let Some(result) = out.results.pop()
                    && !result.columns().is_empty()
                {
                    let (columns, batch) = result.into_batch();
                    self.on_query_event(QueryEvent::Exec(ExecEvent::ResultStart(columns)), cx);
                    self.on_query_event(QueryEvent::Exec(ExecEvent::Rows(batch)), cx);
                }
                if out.truncated {
                    self.on_query_event(QueryEvent::Exec(ExecEvent::Truncated), cx);
                }
                if let Some(rows) = out.rows {
                    let done = ExecEvent::StatementDone { rows };
                    self.on_query_event(QueryEvent::Exec(done), cx);
                }
                (out.elapsed, None)
            }
            Err(e) => (Duration::ZERO, Some(e)),
        };
        self.on_query_event(
            QueryEvent::Finished {
                elapsed,
                error,
                error_position: None,
                in_transaction: false,
                cancelled: false,
            },
            cx,
        );
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
            cx.emit(ResultEvent::Refine { sql, append: false });
        }
        cx.notify();
    }

    /// The next page, when the grid is scrolled to the end of the loaded rows.
    pub fn load_more(&mut self, cx: &mut Context<Self>) {
        let d = self.table.read(cx).delegate();
        if !d.more || self.page_error.is_some() {
            return;
        }
        let offset = d.result.row_count();
        if let Some(sql) = self.refined_sql(offset, cx) {
            cx.emit(ResultEvent::Refine { sql, append: true });
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
        let d = self.table.read(cx).delegate();
        let columns = d.column_names();
        let kinds: Vec<_> = (0..columns.len()).map(|c| d.kind(c)).collect();
        Some(refined_sql(
            base,
            &columns,
            &kinds,
            &d.refine,
            self.dialect?,
            offset,
            tangul_engine::ROW_LIMIT + 1,
        ))
    }

    /// The last result as chart data (for the status-bar menu), or why it can't be
    /// charted.
    /// Whether it shows the table and that has more rows than fit (it scrolls).
    pub fn table_scrolls(&self, cx: &App) -> bool {
        matches!(self.status, Status::Done { .. })
            && self.chart_shown().is_none()
            && self.table.read(cx).vertical_scroll_handle.is_scrollable()
    }

    /// How far its table is scrolled down (tests).
    #[cfg(test)]
    pub fn table_scroll_y(&self, cx: &App) -> Pixels {
        let handle = &self.table.read(cx).vertical_scroll_handle;
        handle.0.borrow().base_handle.offset().y
    }

    pub fn chart(&self) -> Result<Rc<ChartData>, SharedString> {
        self.chart.clone()
    }

    /// What the result is shown as, if it is shown as a chart.
    pub fn chart_shown(&self) -> Option<ChartView> {
        let data = self.chart.as_ref().ok()?;
        self.chart_view.map(|v| data.view_or_default(Some(v)))
    }

    /// The chart as shown, at the default label size, and where it was drawn.
    #[cfg(test)]
    pub fn chart_for_test(&self) -> Option<(ResultChart, Bounds<Pixels>)> {
        let view = self.chart_shown()?;
        let data = self.chart.as_ref().ok()?.clone();
        let chart = ResultChart::new("t", data, view.style, Vec::new(), false, chart::LABEL_SIZE);
        Some((chart, self.chart_bounds.get()))
    }

    /// The view it is set to, with or without a result yet.
    pub fn chart_view(&self) -> Option<ChartView> {
        self.chart_view
    }

    /// Shows the result as `view`, or as the grid (`None`).
    pub fn set_chart_view(&mut self, view: Option<ChartView>, cx: &mut Context<Self>) {
        self.chart_view = view;
        self.chart_request = None;
        cx.notify();
    }

    /// Shows the next result as `style` (the nearest one that fits, or the grid).
    pub fn request_chart(&mut self, style: ChartStyle, cx: &mut Context<Self>) {
        // Set already, so the status bar says a chart is coming.
        self.chart_view = Some(ChartView::new(style));
        self.chart_request = Some(style);
        cx.notify();
    }

    /// A stored chart from before charts took one value column, over a result of
    /// that layout: it isn't drawn, the reason is shown instead.
    pub fn old_chart(&self) -> bool {
        self.old_result && self.chart_view.is_some_and(|v| v.old_layout)
    }

    /// What the column filter's value and upper end hold (tests).
    #[cfg(test)]
    pub fn filter_inputs(&self, cx: &App) -> (String, String) {
        let table = self.table.read(cx);
        let panel = table.delegate().filter_panel().expect("a filter panel");
        panel.read(cx).inputs(cx)
    }

    /// The table's selected cell (tests).
    #[cfg(test)]
    pub fn selected_cell(&self, cx: &App) -> Option<(usize, usize)> {
        self.table.read(cx).selected_cell()
    }

    /// Scrolls the grid sideways to column `col` (tests).
    #[cfg(test)]
    pub fn scroll_to_col(&self, col: usize, cx: &mut App) {
        self.table.update(cx, |t, cx| t.scroll_to_col(col, cx));
    }

    /// Why the last run failed, if it did (tests).
    #[cfg(test)]
    pub fn error(&self) -> Option<String> {
        match &self.status {
            Status::Done { error, .. } => error.as_ref().map(ToString::to_string),
            _ => None,
        }
    }

    /// The part of the error message the user selected (tests).
    #[cfg(test)]
    pub fn error_selection(&self, cx: &App) -> String {
        self.error_text.read(cx).selected(cx).to_string()
    }

    /// Whether queries around the result may sort, filter and page it (tests).
    #[cfg(test)]
    pub fn is_refinable(&self) -> bool {
        self.base_sql.is_some()
    }

    /// Timing and row count of the last run, for the status bar.
    pub fn status_text(&self, cx: &App) -> SharedString {
        let d = self.table.read(cx).delegate();
        let rows = d.result.row_count();
        let (more, filtered) = (d.more, !d.refine.is_empty());
        if self.derived == Some(true) {
            return format!("{rows} rows · loading more…").into();
        }
        match &self.status {
            Status::Idle => "".into(),
            Status::Running(since) => crate::ui::millis(since.elapsed()),
            Status::Done {
                elapsed, cancelled, ..
            } => {
                let mut s = crate::ui::millis(*elapsed).to_string();
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
                // The message itself is below the rows, to select and copy.
                if self.page_error.is_some() {
                    s.push_str(" · loading more failed");
                } else if more {
                    s.push_str(" · scroll for more");
                } else if self.truncated {
                    s.push_str(&format!(" · limited to {} rows", tangul_engine::ROW_LIMIT));
                }
                s.into()
            }
        }
    }

    /// `data` as drawn in `style`, made once while they and `max_series` stay.
    fn drawn_chart(&self, data: &ChartData, style: ChartStyle, max_series: usize) -> Rc<ChartData> {
        let mut drawn = self.drawn.borrow_mut();
        let source = std::ptr::from_ref(data);
        if let Some(d) = drawn
            .as_ref()
            .filter(|d| std::ptr::eq(Rc::as_ptr(&d.source), source))
            .filter(|d| (d.style, d.max_series) == (style, max_series))
        {
            return d.data.clone();
        }
        let source = match &self.chart {
            Ok(chart) if std::ptr::eq(Rc::as_ptr(chart), source) => chart.clone(),
            // Not this view's own data: made each time.
            _ => return Rc::new(data.drawn(style, max_series)),
        };
        let data = Rc::new(crate::perf::span("result.drawn", || {
            data.drawn(style, max_series)
        }));
        *drawn = Some(DrawnChart {
            source,
            style,
            max_series,
            data: data.clone(),
        });
        data
    }

    /// The legend: one line of series in the order the tooltip lists them (the
    /// top of a stack first), as many as fit; the pointer on "+N more" lists them
    /// all. Every entry is laid out to be measured; those past the room are
    /// invisible, and the next frame knows how many fit.
    fn render_legend(
        &self,
        data: &ChartData,
        style: ChartStyle,
        colors: &[Hsla],
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        // The colored series first, then the grey ones.
        let (mut order, grey): (Vec<usize>, Vec<usize>) = data
            .legend_order(style)
            .into_iter()
            .partition(|&s| !data.series[s].grey);
        order.extend(grey);
        let count = order.len();
        let fit = self.legend_fit.get().min(count);
        let hidden = count - fit;
        let left_out = data.left_out;
        let entry = |s: usize| {
            h_flex()
                .flex_none()
                .whitespace_nowrap()
                .gap_1()
                .items_center()
                .child(
                    div()
                        .size_2()
                        .flex_none()
                        .rounded_sm()
                        .bg(colors[s % colors.len()]),
                )
                .child(chart::series_label(&data.series[s]))
        };
        let all = (hidden > 0 && self.legend_more).then(|| {
            let theme = cx.theme();
            // Wrapped into columns when there are many.
            let per_column = 16;
            div().absolute().top_full().left_0().child(
                deferred(
                    anchored().snap_to_window_with_margin(px(8.)).child(
                        h_flex()
                            .id("chart-legend-all")
                            .test_support()
                            .items_start()
                            .gap_4()
                            .p_2()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.popover)
                            .text_color(theme.popover_foreground)
                            .shadow_md()
                            .children(order.chunks(per_column).map(|chunk| {
                                v_flex().gap_0p5().children(chunk.iter().map(|&s| entry(s)))
                            })),
                    ),
                )
                .with_priority(1),
            )
        });
        // Shown only with something behind it; always laid out, for its width.
        let more = div()
            .id("chart-legend-more")
            .test_support()
            .flex_none()
            .whitespace_nowrap()
            .relative()
            .cursor_default()
            .when(hidden == 0, |this| this.invisible())
            .when(hidden > 0, |this| {
                this.on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    this.legend_more = *hovered;
                    cx.notify();
                }))
            })
            .child(format!("+{} more", hidden.max(1)))
            .children(all);
        // Widths of the entries and "+N more", in the order laid out below.
        let widths: Rc<RefCell<Vec<f32>>> = Rc::default();
        let line = h_flex()
            .gap_3()
            .on_children_prepainted({
                let widths = widths.clone();
                move |bounds, _, _| {
                    *widths.borrow_mut() = bounds.iter().map(|b| b.size.width.as_f32()).collect();
                }
            })
            .children(order[..fit].iter().map(|&s| entry(s)))
            .child(more)
            .children(order[fit..].iter().map(|&s| entry(s).invisible()));
        let view = cx.entity().downgrade();
        let fit_cell = self.legend_fit.clone();
        let legend = div()
            .flex_1()
            .min_w_0()
            .on_children_prepainted(move |bounds, window, cx| {
                let Some(room) = bounds.first().map(|b| b.size.width.as_f32()) else {
                    return;
                };
                let widths = widths.borrow();
                if widths.len() != count + 1 {
                    return;
                }
                // `gap_3`.
                let gap = window.rem_size().as_f32() * 0.75;
                let entries = widths[..fit].iter().chain(&widths[fit + 1..]).copied();
                let fits = legend_fit(entries, widths[fit], gap, room);
                if fits != fit_cell.get() {
                    fit_cell.set(fits);
                    view.update(cx, |_, cx| cx.notify()).ok();
                }
            })
            .child(div().w_full().overflow_hidden().child(line));
        h_flex()
            .flex_1()
            .min_w_0()
            .gap_3()
            .child(legend)
            // Side-by-side bars beyond `chart.max_series`.
            .when(left_out > 0, |this| {
                this.child(
                    div()
                        .id("chart-left-out")
                        .test_support()
                        .flex_none()
                        .whitespace_nowrap()
                        .child(format!("{left_out} smaller not shown")),
                )
            })
    }

    /// The chart, and the pinned row's values over it. A click pins the row under
    /// the pointer; another click or Esc releases it.
    fn render_chart_area(
        &self,
        chart: ResultChart,
        view: ChartView,
        source: Option<Weak<ChartData>>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let bounds = self.chart_bounds.get();
        let pin = chart.pin();
        let pinned = pin.map(|pin| self.render_pinned(&chart, pin, bounds, cx));
        // The same chart for mapping a click to its row.
        let hit = chart.for_hits();
        let chart_bounds = self.chart_bounds.clone();
        div()
            .id("chart-area")
            .test_support()
            .relative()
            .flex_1()
            .min_h_0()
            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                if pin.is_some() {
                    this.chart_pin = None;
                    cx.notify();
                    return;
                }
                let bounds = chart_bounds.get();
                let Some(source) = source.clone() else {
                    return;
                };
                if let Some((row, series)) = hit.hit(bounds, e.position() - bounds.origin) {
                    this.chart_pin = Some(ChartPin {
                        pin: Pin { row, series },
                        view,
                        source,
                    });
                    window.focus(&this.pin_focus.clone(), cx);
                    cx.notify();
                }
            }))
            .child(chart)
            .children(pinned)
    }

    /// A pinned row's values: every series, in the tooltip's order, scrollable;
    /// Copy copies them as tab-separated lines.
    fn render_pinned(
        &self,
        chart: &ResultChart,
        pin: Pin,
        bounds: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let data = chart.data();
        let row = pin.row;
        let order = data.row_order(chart.style(), row);
        let x = chart.row_x(bounds, row);
        let width = bounds.size.width.as_f32();
        let mut text = format!("{}\t{}\n", data.x_name, data.x[row]);
        for &s in &order {
            let series = &data.series[s];
            let value = series.values[row].map_or("NULL".into(), |v| v.to_string());
            text.push_str(&format!("{}\t{value}\n", series.name));
        }
        let total = chart.style().is_stacked().then(|| data.row_total(row));
        if let Some(total) = total {
            text.push_str(&format!("Total\t{total}\n"));
        }
        let theme = cx.theme();
        let rows: Vec<AnyElement> = order
            .iter()
            .map(|&s| chart.value_row(s, row, pin.series == Some(s), cx))
            .collect();
        v_flex()
            .id("chart-pinned")
            .test_support()
            .track_focus(&self.pin_focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    this.chart_pin = None;
                    cx.notify();
                }
            }))
            // Clicks in here are the list's, not the chart's.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .occlude()
            .absolute()
            .top(px(8.))
            .map(|this| {
                if x < width / 2. {
                    this.left(px(x + 12.))
                } else {
                    this.right(px(width - x + 12.))
                }
            })
            .max_h(px((bounds.size.height.as_f32() - 16.).max(80.)))
            .min_w(px(180.))
            .p_2()
            .gap_1()
            .text_xs()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .shadow_md()
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(div().font_semibold().child(data.x[row].clone()))
                    .child(
                        h_flex()
                            .child(
                                Button::new("chart-pinned-copy")
                                    .label("Copy")
                                    .xsmall()
                                    .ghost()
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            text.clone(),
                                        ))
                                    }),
                            )
                            .child(
                                Button::new("chart-pinned-close")
                                    .icon(IconName::Close)
                                    .xsmall()
                                    .ghost()
                                    .tooltip("Release (Esc)")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.chart_pin = None;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .id("chart-pinned-rows")
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .children(total.map(|total| chart::total_row(total, cx)))
    }

    /// Instead of a stored chart from before charts took one value column: why,
    /// the rules, and the query to hand to an agent.
    fn render_old_chart(&self, cx: &mut Context<Self>) -> AnyElement {
        let message = old_layout_message();
        let for_agent = format!("{message}\n\nThe query:\n{}", self.sql.trim());
        v_flex()
            .id("chart-old-layout")
            .test_support()
            .size_full()
            .p_3()
            .gap_2()
            .overflow_y_scroll()
            .child(alert_card(Severity::Warning, message, cx).max_w(px(560.)))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("chart-old-copy")
                            .label("Copy for agent")
                            .xsmall()
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(for_agent.clone()))
                            }),
                    )
                    .child(
                        Button::new("chart-old-table")
                            .label("Show table")
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| this.set_chart_view(None, cx))),
                    ),
            )
            .into_any_element()
    }

    /// All series in one chart, with a legend when there are several.
    fn render_chart(
        &self,
        data: &ChartData,
        view: ChartView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let style = view.style;
        // The largest series in color, the rest grey (`chart.max_series`).
        let max_series = settings::current(cx).chart_max_series();
        let data = self.drawn_chart(data, style, max_series);
        // One per colored series: the theme's chart colors, or shades of its one.
        let colored = data.series.iter().filter(|s| !s.grey).count();
        let palette = crate::theme::chart_colors(colored.max(1), cx);
        // Opaque, so it fades like the others while a series is highlighted.
        let grey = crate::theme::tint(cx.theme().muted_foreground, cx.theme().background, 0.45);
        let mut next = 0;
        let colors: Vec<Hsla> = data
            .series
            .iter()
            .map(|s| {
                if s.grey {
                    grey
                } else {
                    next += 1;
                    palette[(next - 1) % palette.len()]
                }
            })
            .collect();
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
        let source = self.chart.as_ref().ok().map(Rc::downgrade);
        let pin = self
            .chart_pin
            .as_ref()
            .filter(|p| p.view == view && source.as_ref().is_some_and(|s| s.ptr_eq(&p.source)))
            .map(|p| p.pin);
        let legend = (data.series.len() > 1).then(|| self.render_legend(&data, style, &colors, cx));
        let theme = cx.theme();
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
            // A fact about the data, like the grid's "loaded rows".
            .when(data.truncated, |this| {
                this.child(
                    div()
                        .id("chart-truncated")
                        .test_support()
                        .child(format!("First {} rows only", data.x.len())),
                )
            })
            .when(data.unsorted && !style.is_bars(), |this| {
                this.child(format!(
                    "{} is not in ascending order; add ORDER BY to plot it over time",
                    data.x_name
                ))
            })
            .child(
                self.render_chart_area(
                    ResultChart::new(
                        "result-chart",
                        data,
                        style,
                        colors,
                        self.smooth_lines,
                        label_size.unwrap_or(chart::LABEL_SIZE),
                    )
                    .pinned(pin)
                    .bounds_out(self.chart_bounds.clone()),
                    view,
                    source,
                    cx,
                ),
            )
            .into_any_element()
    }

    /// One chip per filter and for the sort; clicking a chip's ✕ removes it.
    fn render_refine_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = self.table.read(cx).delegate();
        let mark = d.mark_chip();
        if (d.refine.is_empty() && mark.is_none()) || matches!(self.status, Status::Idle) {
            return None;
        }
        let filtered = !d.refine.is_empty();
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
                .when(filtered, |bar| {
                    bar.child(
                        Icon::new(Lucide::Funnel)
                            .xsmall()
                            .text_color(theme.muted_foreground),
                    )
                })
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
                .when(filtered, |bar| {
                    bar.child(
                        Button::new("refine-clear")
                            .label("Clear all")
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| this.clear_refine(cx))),
                    )
                })
                .when_some(mark, |bar, label| {
                    bar.child(
                        h_flex()
                            .id("mark-chip")
                            .test_support()
                            .pl_2()
                            .gap_0p5()
                            .items_center()
                            .rounded(theme.radius)
                            .bg(theme.secondary)
                            .child(label)
                            .child(
                                Button::new("mark-chip-clear")
                                    .icon(IconName::Close)
                                    .xsmall()
                                    .ghost()
                                    .tooltip("Clear mark")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.clear_grid_mark(cx);
                                    })),
                            ),
                    )
                })
                .into_any_element(),
        )
    }

    /// While a drag marks: what is marked, next to the pointer.
    fn render_drag_chip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (label, pointer) = self.table.read(cx).delegate().drag_chip()?;
        let theme = cx.theme();
        Some(
            deferred(
                anchored()
                    .position(pointer + point(px(14.), px(14.)))
                    .child(
                        div()
                            .id("mark-drag-chip")
                            .test_support()
                            .px_2()
                            .py_0p5()
                            .text_xs()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.popover)
                            .text_color(theme.popover_foreground)
                            .shadow_md()
                            .child(label),
                    ),
            )
            .with_priority(grid::LAYER_DRAG_CHIP)
            .into_any_element(),
        )
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        match &self.status {
            Status::Done { error: Some(_), .. } => div()
                .size_full()
                .p_3()
                .child(self.error_text.clone())
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
            Status::Done { .. } if self.old_chart() => self.render_old_chart(cx),
            Status::Done { .. }
                if let (Some(view), Ok(data)) = (self.chart_shown(), &self.chart) =>
            {
                crate::perf::span("result.render_chart", || self.render_chart(data, view, cx))
            }
            _ => DataTable::new(&self.table)
                .stripe(true)
                .xsmall()
                .into_any_element(),
        }
    }
}

impl Render for ResultView {
    /// The result, with the active filters and sort above it.
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let error = match &self.status {
            Status::Done { error: Some(e), .. } => Some(e.clone()),
            _ => None,
        };
        let page_failed = error.is_none() && self.page_error.is_some();
        if let Some(text) = error.or(self.page_error.clone()) {
            self.error_text.update(cx, |e, cx| e.set_text(text, cx));
        }
        let chips = self.render_refine_bar(cx);
        let drag_chip = self.render_drag_chip(cx);
        let body = self.render_body(cx);
        let theme = cx.theme();
        v_flex()
            .size_full()
            .on_action(cx.listener(Self::inspect_action))
            .on_action(cx.listener(Self::copy_mark_action))
            .on_action(cx.listener(Self::mark_all_action))
            .on_action(cx.listener(Self::clear_mark_action))
            .on_action(cx.listener(|this, _: &ExtendMarkUp, _, cx| this.extend_mark(-1, 0, cx)))
            .on_action(cx.listener(|this, _: &ExtendMarkDown, _, cx| this.extend_mark(1, 0, cx)))
            .on_action(cx.listener(|this, _: &ExtendMarkLeft, _, cx| this.extend_mark(0, -1, cx)))
            .on_action(cx.listener(|this, _: &ExtendMarkRight, _, cx| this.extend_mark(0, 1, cx)))
            .children(chips)
            .child(div().flex_1().min_h_0().child(body))
            // A page that failed to load: the rows above stay.
            .when(page_failed, |this| {
                this.child(
                    div()
                        .px_3()
                        .py_1p5()
                        .border_t_1()
                        .border_color(theme.border)
                        .child(self.error_text.clone()),
                )
            })
            .children(drag_chip)
    }
}

/// How many legend entries (`widths`, in order) fit on one line of `room`, `gap`
/// apart, leaving room for "+N more" (`more`) when not all of them do.
fn legend_fit(widths: impl IntoIterator<Item = f32>, more: f32, gap: f32, room: f32) -> usize {
    let widths: Vec<f32> = widths.into_iter().collect();
    // The right edge of each entry from the line's start.
    let ends: Vec<f32> = widths
        .iter()
        .scan(-gap, |end, w| {
            *end += gap + w;
            Some(*end)
        })
        .collect();
    if ends.last().is_none_or(|&end| end <= room) {
        return widths.len();
    }
    ends.iter()
        .take_while(|&&end| end + gap + more <= room)
        .count()
}

/// A chart as drawn, with what it was made from (see `ResultView::drawn`).
struct DrawnChart {
    /// Kept, so its address isn't reused while it is compared.
    source: Rc<ChartData>,
    style: ChartStyle,
    max_series: usize,
    data: Rc<ChartData>,
}

#[cfg(test)]
mod tests {
    use super::legend_fit;

    #[test]
    fn the_legend_keeps_room_for_more() {
        // All fit: no "+N more" needed.
        assert_eq!(legend_fit([40., 40., 40.], 50., 10., 140.), 3);
        // Not all: as many as fit beside "+N more".
        assert_eq!(legend_fit([40., 40., 40.], 50., 10., 139.), 1);
        assert_eq!(legend_fit([40., 40., 40., 40.], 30., 10., 130.), 2);
        // Not even one: only "+N more".
        assert_eq!(legend_fit([40., 40.], 30., 10., 60.), 0);
        assert_eq!(legend_fit([], 30., 10., 60.), 0);
    }
}
