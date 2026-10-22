//! A query's result, shown as a grid or a chart, with its run state: running,
//! finished, filtered, sorted and paged. It knows nothing about tabs: whatever runs
//! queries (a tab now; a board tile or the assistant later) feeds it the engine's
//! events and acts on its [`ResultEvent`]s.

mod filter;
mod grid;

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{PopupMenu, PopupMenuItem},
    table::{DataTable, TableEvent, TableState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_assist::chart::{ChartData, ChartStyle, ChartView, chart_data};
use tangul_assist::columns::ColumnFacts;
use tangul_assist::dialect::Dialect;
use tangul_assist::refine::{ColumnFilter, ColumnKind, Refine, Sort, refinable, refined_sql};
use tangul_assist::{edit::RowEdit, related::RelatedLink};
use tangul_core::QueryId;
use tangul_engine::{ExecEvent, QueryEvent, QueryOutput};

use crate::InspectValue;
use crate::assets::{Lucide, style_icon};
use crate::chart::{self, ResultChart};
use crate::settings;
use filter::FilterPanel;
use grid::ResultsDelegate;

/// Shows a result as the view picked (`None`: the table).
pub type SetView = Rc<dyn Fn(Option<ChartView>, &mut App)>;

/// The table and every chart `data` fits, `shown` checked: all value columns in
/// one style, and one column at a time. `set` applies the pick. The one menu of
/// views, for the status bar and board tiles alike.
pub fn chart_menu(
    mut menu: PopupMenu,
    data: &ChartData,
    shown: Option<ChartView>,
    set: SetView,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let item = |label: String, view: Option<ChartView>, icon: bool| {
        let set = set.clone();
        PopupMenuItem::new(label)
            .when(icon, |item| {
                item.icon(Icon::new(style_icon(view.map(|v| v.style))))
            })
            .checked(shown == view)
            .on_click(move |_, _, cx| set(view, cx))
    };
    menu = menu.item(item("Table".into(), None, true));
    let multi = data.multi_styles();
    if !multi.is_empty() {
        menu = menu.separator().label("All columns");
        for style in multi {
            let view = ChartView {
                style,
                series: None,
            };
            menu = menu.item(item(style.label().into(), Some(view), true));
        }
    }
    menu = menu.separator().label("Single column");
    for style in data.single_styles() {
        let label = style.single_label();
        if data.series.len() < 2 {
            let view = ChartView {
                style,
                series: Some(0),
            };
            menu = menu.item(item(label.into(), Some(view), true));
            continue;
        }
        // Several value columns: pick which one.
        let columns: Vec<(String, ChartView)> = data
            .series
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let view = ChartView {
                    style,
                    series: Some(i),
                };
                (s.name.clone(), view)
            })
            .collect();
        let set = set.clone();
        menu = menu.submenu_with_icon(
            Some(Icon::new(style_icon(Some(style)))),
            label,
            window,
            cx,
            move |menu, _, _| {
                columns.iter().fold(menu, |menu, (name, view)| {
                    let (set, view) = (set.clone(), *view);
                    menu.item(
                        PopupMenuItem::new(name.clone())
                            .checked(shown == Some(view))
                            .on_click(move |_, _, cx| set(Some(view), cx)),
                    )
                })
            },
        );
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

enum Status {
    Idle,
    Running,
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
    /// The chart picked in the status bar, shown instead of the grid when the result
    /// can be charted; `None` shows the grid. Kept across runs.
    chart_view: Option<ChartView>,
    /// A chart asked for by style and column name before the result is known (a
    /// proposal from the assistant): resolved against the next result, then
    /// dropped (see `ChartData::view_for`).
    chart_request: Option<(ChartStyle, Option<String>)>,
    /// Curved instead of straight lines; starts straight, which is exact.
    smooth_lines: bool,
    /// The last run's statement, when its result can be filtered, sorted and paged.
    base_sql: Option<String>,
    /// While a derived query runs (see [`ResultEvent::Refine`]): whether it appends
    /// a page.
    derived: Option<bool>,
    /// Loading a further page failed (the loaded rows stay).
    page_error: Option<SharedString>,
    /// SQL of the connection the results come from, for filtering, sorting, paging.
    dialect: Option<&'static dyn Dialect>,
    _table_events: Subscription,
}

impl EventEmitter<ResultEvent> for ResultView {}

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
            chart_view: None,
            chart_request: None,
            smooth_lines: false,
            base_sql: None,
            derived: None,
            page_error: None,
            dialect: None,
            _table_events: table_events,
        }
    }

    fn on_table_event(
        &mut self,
        _: Entity<TableState<ResultsDelegate>>,
        event: &TableEvent,
        cx: &mut Context<Self>,
    ) {
        // A click or the arrow keys only select; the viewer changes on a double
        // click, Space or "Inspect value".
        if let TableEvent::DoubleClickedCell(row, col) = event {
            self.inspect(*row, *col, true, cx);
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
        self.table
            .update(cx, |t, cx| t.set_selected_cell(row, col, cx));
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
            self.status = Status::Running;
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
                    if let Some((style, column)) = self.chart_request.take() {
                        // A result that can't be a chart shows the grid.
                        self.chart_view = self
                            .chart
                            .as_ref()
                            .ok()
                            .map(|data| data.view_for(style, column.as_deref()));
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
    pub fn chart(&self) -> Result<Rc<ChartData>, SharedString> {
        self.chart.clone()
    }

    /// What the result is shown as, if it is shown as a chart.
    pub fn chart_shown(&self) -> Option<ChartView> {
        let data = self.chart.as_ref().ok()?;
        self.chart_view.map(|v| data.view_or_default(Some(v)))
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

    /// Shows the next result as `style`, of one value column by name or all of
    /// them; the default view when the result doesn't fit.
    pub fn request_chart(
        &mut self,
        style: ChartStyle,
        column: Option<String>,
        cx: &mut Context<Self>,
    ) {
        // Set already, so the status bar says a chart is coming.
        self.chart_view = Some(ChartView {
            style,
            series: None,
        });
        self.chart_request = Some((style, column));
        cx.notify();
    }

    /// What the column filter's value and upper end hold (tests).
    #[cfg(test)]
    pub fn filter_inputs(&self, cx: &App) -> (String, String) {
        let table = self.table.read(cx);
        let panel = table.delegate().filter_panel().expect("a filter panel");
        panel.read(cx).inputs(cx)
    }

    /// Why the last run failed, if it did (tests).
    #[cfg(test)]
    pub fn error(&self) -> Option<String> {
        match &self.status {
            Status::Done { error, .. } => error.as_ref().map(ToString::to_string),
            _ => None,
        }
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

    /// All series in one chart, with a legend when there are several.
    fn render_chart(
        &self,
        data: &ChartData,
        view: ChartView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // One per series: the theme's chart colors, or shades of its one.
        let mut colors = crate::theme::chart_colors(data.series.len(), cx);
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
        // Capped: many columns would push the chart off the view (one column at a
        // time is in the status bar's chart menu).
        let legend =
            (data.series.len() > 1).then(|| {
                let hidden = data.series.len().saturating_sub(chart::MAX_LISTED);
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .children(data.series.iter().take(chart::MAX_LISTED).enumerate().map(
                        |(i, s)| {
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(div().size_2().rounded_sm().bg(colors[i % colors.len()]))
                                .child(s.name.clone())
                        },
                    ))
                    .when(hidden > 0, |this| this.child(format!("+{hidden} more")))
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

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
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
}

impl Render for ResultView {
    /// The result, with the active filters and sort above it.
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chips = self.render_refine_bar(cx);
        let body = self.render_body(cx);
        v_flex()
            .size_full()
            .on_action(cx.listener(Self::inspect_action))
            .children(chips)
            .child(div().flex_1().min_h_0().child(body))
    }
}
