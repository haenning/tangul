//! Virtualized result grid: the table only asks for visible cells, and cells are read
//! straight from the ResultSet arena.
//!
//! The mark (`tangul_assist::mark`: columns × rows) is kept here rather than in the
//! table (which knows a single cell): click marks a cell, ⇧-click a rectangle,
//! ⌘-click toggles a row. The table's own selection is a cell (moved with the arrow
//! keys); a double click or Space makes it the inspected cell (A, shown in the value
//! viewer), ⌥-click picks a cell to compare with it (B). A long hover on a value
//! shows all of it. The right-click menu copies the mark, follows foreign keys to
//! related rows and writes UPDATE / DELETE statements for the marked rows (to open
//! elsewhere, not run).
//!
//! Column headers mark primary-key columns, show count / sum / avg / min / max of a
//! numeric column on hover (over all loaded rows), and open the column's filter and
//! sort popover. Scrolling to the end loads the next page.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable, Sizable as _, Size,
    button::Button,
    h_flex,
    hover_card::HoverCard,
    menu::{PopupMenu, PopupMenuItem},
    popover::Popover,
    table::{Column, TableDelegate, TableState},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_assist::columns::{ColumnFacts, TagKind};
use tangul_assist::mark::{Keys, Mark, tsv};
use tangul_assist::refine::{ColumnKind, Refine};
use tangul_assist::{edit::RowEdit, related::RelatedLink};
use tangul_core::{ColumnInfo, ResultSet};

use super::filter::FilterPanel;
use super::{ResultEvent, ResultView};
use crate::assets::Lucide;
use crate::settings;

/// Longer values are cut in the grid (the hover card and the value viewer show all).
const MAX_CELL_CHARS: usize = 200;
/// Hover before a value's card opens: longer than a tooltip, so moving across the
/// grid opens none.
const PEEK_DELAY: Duration = Duration::from_secs(1);
/// The card's size and how much of the value it shows.
const PEEK_WIDTH: f32 = 420.;
const PEEK_HEIGHT: f32 = 240.;
const PEEK_CHARS: usize = 2_000;

/// What the grid draws after the rows (gpui's deferred priorities), bottom to top:
/// the mark, the inspected and compared cells, the row handles, the column summary,
/// the count beside the pointer while dragging.
const LAYER_MARK: usize = 0;
const LAYER_INSPECTED: usize = 1;
const LAYER_HANDLE: usize = 2;
const LAYER_SUMMARY: usize = 3;
pub(super) const LAYER_DRAG_CHIP: usize = 4;

/// The line the table draws under each row (outside its cells).
const ROW_LINE: Pixels = px(1.);

/// The strip at a row's left edge that shows its handle when the pointer is in it.
const ROW_HANDLE: Pixels = px(30.);

/// What a drag with the button held marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drag {
    /// From a cell: the rectangle to the cell under the pointer (last seen).
    Cells(usize, usize),
    /// Along the row handles: whole rows down to the one under the pointer.
    Rows(usize),
}

/// The filter zone right of a column's name is never narrower (the name gives way).
const FILTER_ZONE: Pixels = px(28.);
/// The filter button's square, at the zone's right end.
const FILTER_BUTTON: Pixels = px(22.);

/// Related tables listed directly in the menu (the most used); the rest are under
/// "Show all".
const RELATED_SHOWN: usize = 3;

#[derive(Default)]
pub struct ResultsDelegate {
    pub result: ResultSet,
    columns: Vec<Column>,
    /// What is marked: columns × rows of the loaded result.
    mark: Mark,
    /// Column under the last right click (the cell the menu acts on).
    menu_col: Option<usize>,
    /// Foreign-key links of the current result, most used first (see
    /// `tangul_assist::related`).
    pub related: Vec<RelatedLink>,
    /// UPDATE / DELETE statements for the rows, if the result is from one table.
    pub edit: Option<RowEdit>,
    /// The view it belongs to, which passes on what the menus ask for.
    view: Option<WeakEntity<ResultView>>,
    /// Per result column, what the schema says about it (keys, type).
    pub facts: Vec<Option<ColumnFacts>>,
    /// Filters and sort applied to the result (shown in the headers).
    pub refine: Refine,
    /// The result can be filtered, sorted and paged (a single SELECT).
    pub refinable: bool,
    /// More rows than loaded: scrolling to the end loads the next page.
    pub more: bool,
    hovered: Option<usize>,
    /// Row whose handle (left edge) is under the pointer.
    handle_hovered: Option<usize>,
    /// A drag that marks, while the button is held.
    drag: Option<Drag>,
    /// Cells clicks went to, oldest first: the table selects each too (its event
    /// comes later), which must not move the mark again (see `cursor_moved`).
    clicked: Vec<(usize, usize)>,
    /// Where the pointer was last seen during that drag (window coordinates).
    drag_pointer: Point<Pixels>,
    /// The chip above the grid showed when the drag began: it stays meanwhile
    /// (rows must not move under the pointer).
    drag_keeps_chip: bool,
    /// The inspected cell (A) and the one compared with it (B): (row, column).
    pub inspected: Option<(usize, usize)>,
    pub compared: Option<(usize, usize)>,
    /// Column whose filter popover is open.
    filter_open: Option<usize>,
    /// Column whose foreign-key card is open (hides the aggregates meanwhile).
    key_card: Option<usize>,
    filter_panel: Option<Entity<FilterPanel>>,
    /// Bumped whenever the mark changes (invalidates `stats`).
    selection_gen: u64,
    /// Aggregates of the hovered column: (column, rows, selection_gen) → stats.
    stats: Option<((usize, usize, u64), Option<ColumnStats>)>,
    /// The same over the marked rows only.
    marked_stats: Option<((usize, usize, u64), Option<ColumnStats>)>,
}

impl ResultsDelegate {
    pub fn new(view: WeakEntity<ResultView>, filter_panel: Entity<FilterPanel>) -> Self {
        Self {
            view: Some(view),
            filter_panel: Some(filter_panel),
            ..Default::default()
        }
    }

    /// The column filter's panel (tests).
    #[cfg(test)]
    pub fn filter_panel(&self) -> Option<&Entity<FilterPanel>> {
        self.filter_panel.as_ref()
    }

    pub fn reset(&mut self, cols: Vec<ColumnInfo>) {
        self.columns = cols
            .iter()
            .enumerate()
            .map(|(ix, c)| {
                Column::new(SharedString::from(format!("c{ix}")), c.name.clone())
                    .width(px(160.))
                    .min_width(px(48.))
            })
            .collect();
        self.result = ResultSet::new(cols);
        self.mark.clear();
        self.inspected = None;
        self.compared = None;
        self.menu_col = None;
        self.related.clear();
        self.edit = None;
        self.facts.clear();
        self.more = false;
        self.hovered = None;
        self.filter_open = None;
        self.key_card = None;
        self.selection_gen += 1;
        self.stats = None;
        self.marked_stats = None;
    }

    /// What the schema says about the columns; they decide which columns get
    /// aggregates, so cached ones are dropped.
    pub fn set_facts(&mut self, facts: Vec<Option<ColumnFacts>>) {
        self.facts = facts;
        self.stats = None;
        self.marked_stats = None;
    }

    /// Drops the rows but keeps the columns (and their widths, keys, filters): a
    /// filtered or sorted version of the result is about to arrive.
    pub fn clear_rows(&mut self) {
        self.result = ResultSet::new(self.result.columns().to_vec());
        self.mark.clear();
        self.inspected = None;
        self.compared = None;
        self.more = false;
        self.filter_open = None;
        self.selection_gen += 1;
        // Same row count as before is likely (a full page): don't reuse old stats.
        self.stats = None;
        self.marked_stats = None;
    }

    /// What the column holds: by its type when it is a known table column,
    /// otherwise by its loaded values.
    pub fn kind(&self, col: usize) -> ColumnKind {
        match self.facts.get(col) {
            Some(Some(f)) => ColumnKind::from_type(&f.data_type),
            _ => ColumnKind::from_values(
                (0..self.result.row_count()).map(|r| self.result.cell(r, col)),
            ),
        }
    }

    /// Summary of a column over all loaded rows.
    fn stats(&mut self, col: usize) -> Option<&ColumnStats> {
        let key = (col, self.result.row_count(), self.selection_gen);
        if self.stats.as_ref().is_none_or(|(k, _)| *k != key) {
            let rows: Vec<usize> = (0..self.result.row_count()).collect();
            self.stats = Some((key, self.column_stats(col, &rows)));
        }
        self.stats.as_ref().and_then(|(_, s)| s.as_ref())
    }

    /// Summary of a marked column over the marked rows, when more than one row is
    /// marked (one value needs no sums).
    fn marked_stats(&mut self, col: usize) -> Option<&ColumnStats> {
        let rows = self.mark.rows(self.result.row_count());
        if !self.mark.col_marked(col) || rows.len() < 2 {
            return None;
        }
        let key = (col, self.result.row_count(), self.selection_gen);
        if self.marked_stats.as_ref().is_none_or(|(k, _)| *k != key) {
            self.marked_stats = Some((key, self.column_stats(col, &rows)));
        }
        self.marked_stats.as_ref().and_then(|(_, s)| s.as_ref())
    }

    fn column_stats(&self, col: usize, rows: &[usize]) -> Option<ColumnStats> {
        // A column the schema says is not a number gets no sums, even if its
        // values look like numbers (codes, zip codes).
        let numeric = !matches!(self.facts.get(col),
            Some(Some(f)) if ColumnKind::from_type(&f.data_type) != ColumnKind::Number);
        column_stats(rows.iter().map(|&r| self.result.cell(r, col)), numeric)
    }

    pub fn clear(&mut self) {
        self.reset(Vec::new());
    }

    /// What is marked (tests).
    #[cfg(test)]
    pub fn mark(&self) -> Mark {
        self.mark.clone()
    }

    /// The cell at `row`, `col` (tests).
    #[cfg(test)]
    pub fn cell(&self, row: usize, col: usize) -> Option<String> {
        self.result.cell(row, col).map(str::to_string)
    }

    pub fn column_names(&self) -> Vec<String> {
        self.result
            .columns()
            .iter()
            .map(|c| c.name.clone())
            .collect()
    }

    /// The mark changed: cached summaries are stale.
    fn marked(&mut self) {
        self.selection_gen += 1;
    }

    /// ⌘/Ctrl+C: the mark as TSV without headers; without a mark the selected cell's
    /// value alone (NULL as empty); None when there is neither.
    pub(super) fn copy_text(&self, selected: Option<(usize, usize)>) -> Option<String> {
        let cell = |r: usize, c: usize| self.result.cell(r, c);
        if !self.mark.is_empty() {
            let rows = self.mark.rows(self.result.row_count());
            return Some(tsv(&rows, &self.mark.cols(), cell, None));
        }
        let (row, col) = selected
            .filter(|&(r, c)| r < self.result.row_count() && c < self.result.columns().len())?;
        Some(tsv(&[row], &[col], cell, None))
    }

    /// ⇧+arrows: the mark's moving corner moves; returns it (to scroll to) and
    /// `None` in an empty result. `from` is the selected cell, if any.
    pub(super) fn extend_mark(
        &mut self,
        from: Option<(usize, usize)>,
        d_row: isize,
        d_col: isize,
    ) -> Option<(usize, usize)> {
        let (rows, cols) = (self.result.row_count(), self.result.columns().len());
        self.mark
            .extend(from.unwrap_or((0, 0)), d_row, d_col, rows, cols);
        self.marked();
        self.mark.end()
    }

    /// ⌘/Ctrl+A.
    pub(super) fn mark_all(&mut self) {
        self.mark.all(self.result.columns().len());
        self.marked();
    }

    /// Esc: clears the mark; false when there was none.
    /// The table's own selected cell (never drawn; the keys move it) is now `row`,
    /// `col`. A click marked already; the keys make it the mark, one cell.
    pub(super) fn cursor_moved(&mut self, row: usize, col: usize) -> bool {
        if let Some(i) = self.clicked.iter().position(|&c| c == (row, col)) {
            self.clicked.drain(..=i);
            return false;
        }
        self.mark.click_cell(row, col, Keys::default());
        self.marked();
        true
    }

    /// The table is about to select `row`, `col` for something that marked or
    /// shows it already (a click, "Inspect value").
    pub(super) fn selecting(&mut self, row: usize, col: usize) {
        self.clicked.push((row, col));
    }

    pub(super) fn clear_mark(&mut self) -> bool {
        if self.mark.is_empty() {
            return false;
        }
        self.mark.clear();
        self.marked();
        true
    }

    /// The chip's text: None unless more than one cell is marked.
    /// The chip above the grid: what is marked, when more than a cell is. During a
    /// drag it neither comes nor goes (the rows would move under the pointer).
    pub(super) fn mark_chip(&self) -> Option<String> {
        let loaded = self.result.row_count();
        let shown = match self.drag {
            Some(_) => self.drag_keeps_chip,
            None => self.mark.cell_count(loaded) > 1,
        };
        shown.then(|| self.mark.describe(loaded))
    }

    /// A drag begins at `pointer` (before the click changes the mark).
    fn start_drag(&mut self, drag: Drag, pointer: Point<Pixels>) {
        self.drag_keeps_chip = self.mark_chip().is_some();
        self.drag = Some(drag);
        self.drag_pointer = pointer;
    }

    /// While a drag marks more than a cell: what is marked, shown next to the
    /// pointer (the chip above the grid would push the rows away under it), and
    /// where the pointer is.
    pub(super) fn drag_chip(&self) -> Option<(String, Point<Pixels>)> {
        let loaded = self.result.row_count();
        (self.drag.is_some() && self.mark.cell_count(loaded) > 1)
            .then(|| (self.mark.describe(loaded), self.drag_pointer))
    }

    /// The button was released: the drag is over (the chip goes above the grid).
    fn end_drag(&mut self) -> bool {
        self.drag.take().is_some()
    }

    /// The rows a menu opened on `row` acts on: the mark's (it holds the clicked
    /// cell), else that row.
    fn menu_rows(&self, row: usize) -> Vec<usize> {
        let rows = self.mark.rows(self.result.row_count());
        if rows.is_empty() { vec![row] } else { rows }
    }

    /// All values of the rows.
    fn row_values(&self, rows: &[usize]) -> Vec<Vec<Option<&str>>> {
        let cols = self.result.columns().len();
        rows.iter()
            .map(|&r| (0..cols).map(|c| self.result.cell(r, c)).collect())
            .collect()
    }

    fn related_item(&self, link: &RelatedLink, rows: &[usize]) -> PopupMenuItem {
        let values: Vec<Vec<Option<&str>>> = rows
            .iter()
            .map(|&r| {
                link.source_cols()
                    .iter()
                    .map(|&c| self.result.cell(r, c))
                    .collect()
            })
            .collect();
        let arrow = if link.incoming { "←" } else { "→" };
        self.open_item(
            format!("{arrow} {}", link.title),
            link.query(&values).map(|sql| ResultEvent::OpenRelated {
                title: link.table.clone(),
                sql,
            }),
        )
    }

    /// A menu item that has the view emit `event`; disabled without one.
    fn open_item(&self, label: String, event: Option<ResultEvent>) -> PopupMenuItem {
        let view = self.view.clone();
        PopupMenuItem::new(label)
            .disabled(event.is_none())
            .on_click(move |_, _, cx| {
                let (Some(view), Some(event)) = (view.as_ref().and_then(|v| v.upgrade()), &event)
                else {
                    return;
                };
                view.update(cx, |_, cx| cx.emit(event.clone()));
            })
    }
}

impl ResultsDelegate {
    /// The filter popover of `col` opened or closed; opening fills the view's panel
    /// for the column.
    fn filter_opened(
        &mut self,
        col: usize,
        open: bool,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        if !open {
            if self.filter_open == Some(col) {
                self.filter_open = None;
            }
            return;
        }
        self.filter_open = Some(col);
        let Some(panel) = self.filter_panel.clone() else {
            return;
        };
        let name: SharedString = self.columns[col].name.clone();
        let kind = self.kind(col);
        let filter = self.refine.filter(col).cloned();
        let sort = self
            .refine
            .sort
            .filter(|s| s.column == col)
            .map(|s| s.descending);
        panel.update(cx, |p, cx| {
            p.show(col, name, kind, filter, sort, window, cx)
        });
    }

    pub fn close_filter(&mut self) {
        self.filter_open = None;
    }

    /// The handle at a row's left edge: a narrow strip that, under the pointer,
    /// shows the row's number and marks the whole row (⇧: the rows from the
    /// anchor, ⌘: toggles it; dragging along the handles marks the rows passed).
    /// Drawn over the row, so it stays at the left edge when the grid scrolls
    /// sideways; it covers the first cell only while shown.
    fn row_handle(&self, row_ix: usize, cx: &mut Context<TableState<Self>>) -> impl IntoElement {
        let theme = cx.theme();
        let shown = self.handle_hovered == Some(row_ix);
        let n_cols = self.columns.len();
        deferred(
            h_flex()
                .id(("row-handle", row_ix))
                .test_support()
                .absolute()
                .left_0()
                .top_0()
                .bottom_0()
                .min_w(ROW_HANDLE)
                .items_center()
                .when(shown, |this| {
                    this.gap_1()
                        .px_1p5()
                        .bg(theme.tokens.table_head)
                        .border_r_1()
                        .border_color(theme.border)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(
                            div()
                                .font_family(theme.mono_font_family.clone())
                                .child((row_ix + 1).to_string()),
                        )
                        .child(
                            div()
                                .id(("row-handle-mark", row_ix))
                                .test_support()
                                .tooltip(|window, cx| {
                                    Tooltip::new("Mark entire row").build(window, cx)
                                })
                                .child(
                                    Icon::new(Lucide::ArrowRightToLine)
                                        .size_3()
                                        .text_color(theme.foreground),
                                ),
                        )
                })
                .occlude()
                .on_hover(cx.listener(move |table, hovered: &bool, _, cx| {
                    let d = table.delegate_mut();
                    if *hovered {
                        d.handle_hovered = Some(row_ix);
                    } else if d.handle_hovered == Some(row_ix) {
                        d.handle_hovered = None;
                    }
                    cx.notify();
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |table, e: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        let d = table.delegate_mut();
                        d.start_drag(Drag::Rows(row_ix), e.position);
                        d.mark.click_row(row_ix, keys(e.modifiers), n_cols);
                        d.marked();
                        cx.notify();
                    }),
                )
                .on_mouse_move(cx.listener(move |table, e: &MouseMoveEvent, _, cx| {
                    let d = table.delegate_mut();
                    if e.pressed_button != Some(MouseButton::Left) {
                        if d.end_drag() {
                            cx.notify();
                        }
                        return;
                    }
                    if d.drag.is_some() {
                        d.drag_pointer = e.position;
                        cx.notify();
                    }
                    if matches!(d.drag, Some(Drag::Rows(r)) if r != row_ix) {
                        let shift = Keys {
                            shift: true,
                            ..Keys::default()
                        };
                        d.mark.click_row(row_ix, shift, n_cols);
                        d.drag = Some(Drag::Rows(row_ix));
                        d.marked();
                        cx.notify();
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|table, _, _, cx| {
                        if table.delegate_mut().end_drag() {
                            cx.notify();
                        }
                    }),
                ),
        )
        .with_priority(LAYER_HANDLE)
    }

    /// Key and link markers left of a column name: a gold key for a primary-key
    /// column, a blue key when that key part also references another table, a
    /// chain for any other foreign-key column. Hovering a reference shows its
    /// target with a button that opens that table.
    fn key_icon(
        &self,
        col_ix: usize,
        facts: &ColumnFacts,
        cx: &mut Context<TableState<Self>>,
    ) -> Option<AnyElement> {
        let theme = cx.theme();
        let (icon, color, label) = match KeyMarker::of(facts)? {
            KeyMarker::Primary => {
                return Some(
                    div()
                        .id(("pk", col_ix))
                        .flex_none()
                        .tooltip(|window, cx| Tooltip::new("Primary key").build(window, cx))
                        .child(
                            Icon::new(Lucide::KeyRound)
                                .size_3()
                                .text_color(theme.warning),
                        )
                        .into_any_element(),
                );
            }
            KeyMarker::PrimaryForeign => (Lucide::Key, theme.info, "Primary key, foreign key"),
            KeyMarker::Foreign => (Lucide::Link, theme.muted_foreground, "Foreign key"),
        };
        let target = facts.references.clone()?;
        let view = self.view.clone();
        let table = cx.entity().downgrade();
        Some(
            HoverCard::new(("fk", col_ix))
                .anchor(Anchor::TopLeft)
                .open_delay(Duration::from_millis(250))
                .on_open_change(move |open, _, cx| {
                    let _ = table.update(cx, |t, cx| {
                        let d = t.delegate_mut();
                        if *open {
                            d.key_card = Some(col_ix);
                        } else if d.key_card == Some(col_ix) {
                            d.key_card = None;
                        }
                        cx.notify();
                    });
                })
                .trigger(
                    div()
                        .flex_none()
                        .child(Icon::new(icon).size_3().text_color(color)),
                )
                .content(move |_, _, cx| {
                    let theme = cx.theme();
                    let view = view.clone();
                    let jump = target.clone();
                    v_flex()
                        .gap_1p5()
                        .text_xs()
                        .child(div().text_color(theme.muted_foreground).child(label))
                        .child(
                            h_flex()
                                .gap_1()
                                .child(Icon::new(Lucide::Link).size_3())
                                .child(
                                    div()
                                        .font_family(theme.mono_font_family.clone())
                                        .child(format!("{}.{}", target.schema, target)),
                                ),
                        )
                        .child(
                            Button::new("jump-to-table")
                                .label("Jump to table")
                                .icon(Icon::new(Lucide::Table))
                                .xsmall()
                                .outline()
                                .on_click(move |_, _, cx| {
                                    let Some(view) = view.as_ref().and_then(WeakEntity::upgrade)
                                    else {
                                        return;
                                    };
                                    let event = ResultEvent::OpenTable {
                                        schema: jump.schema.clone(),
                                        table: jump.table.clone(),
                                    };
                                    view.update(cx, |_, cx| cx.emit(event));
                                }),
                        )
                })
                .into_any_element(),
        )
    }
}

/// The marker a column gets left of its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyMarker {
    /// Part of the primary key, local to the table (gold key).
    Primary,
    /// Part of the primary key that also references another table (blue key).
    PrimaryForeign,
    /// A foreign-key column outside the primary key (chain).
    Foreign,
}

impl KeyMarker {
    fn of(facts: &ColumnFacts) -> Option<Self> {
        match (facts.primary_key, facts.references.is_some()) {
            (true, false) => Some(Self::Primary),
            (true, true) => Some(Self::PrimaryForeign),
            (false, true) => Some(Self::Foreign),
            (false, false) => None,
        }
    }
}

fn tag_icon(kind: TagKind) -> Lucide {
    match kind {
        TagKind::Type => Lucide::Type,
        TagKind::Nullable => Lucide::CircleDashed,
        TagKind::Unique => Lucide::FingerprintPattern,
        TagKind::Enum => Lucide::List,
        TagKind::Default => Lucide::CornerDownRight,
        TagKind::Computed => Lucide::Sigma,
        TagKind::Auto => Lucide::ListOrdered,
        TagKind::Index => Lucide::Zap,
    }
}

/// What the schema says about a column: pills in the accent color, then its comment.
fn schema_tags(facts: &ColumnFacts, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    v_flex()
        .id("column-tags")
        .test_support()
        .gap_1()
        .pb_1()
        .child(
            h_flex()
                .flex_wrap()
                .gap_1()
                .children(facts.tags().into_iter().map(|tag| {
                    h_flex()
                        .gap_1()
                        .px_1p5()
                        .py_0p5()
                        .items_center()
                        .rounded_full()
                        .bg(theme.primary.opacity(0.12))
                        .text_color(theme.primary)
                        .child(Icon::new(tag_icon(tag.kind)).size_3())
                        .child(div().child(tag.text))
                })),
        )
        .when_some(facts.comment.clone(), |this, comment| {
            this.child(
                div()
                    .text_color(theme.muted_foreground)
                    .line_clamp(3)
                    .child(comment),
            )
        })
}

fn stats_card(
    scope: String,
    stats: &ColumnStats,
    marked: Option<(String, ColumnStats)>,
    facts: Option<&ColumnFacts>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let label = |text: String| div().pb_1().text_color(theme.muted_foreground).child(text);
    v_flex()
        .id("column-stats")
        .test_support()
        .min_w(px(160.))
        .max_w(px(320.))
        .p_2()
        .gap_0p5()
        .text_xs()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .shadow_md()
        .child(label(scope))
        .children(facts.map(|f| schema_tags(f, cx)))
        .child(stats_rows(stats, cx))
        .when_some(marked, |this, (scope, stats)| {
            this.child(
                v_flex()
                    .id("column-stats-marked")
                    .test_support()
                    .mt_1()
                    .pt_1p5()
                    .gap_0p5()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(label(scope))
                    .child(stats_rows(&stats, cx)),
            )
        })
}

/// Count / sum / avg / min / max of one summary, label left, value right.
fn stats_rows(stats: &ColumnStats, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let row = |label: &'static str, value: SharedString| {
        h_flex()
            .gap_4()
            .justify_between()
            .child(div().text_color(theme.muted_foreground).child(label))
            .child(
                div()
                    .font_family(theme.mono_font_family.clone())
                    .child(value),
            )
    };
    v_flex()
        .gap_0p5()
        .child(row("rows", stats.rows.to_string().into()))
        .when(stats.nulls > 0, |this| {
            this.child(row("non-null", stats.count.to_string().into()))
                .child(row("nulls", stats.nulls.to_string().into()))
        })
        .when(stats.count > 0, |this| {
            this.child(row("distinct", stats.distinct.to_string().into()))
        })
        .when(stats.numeric(), |this| {
            let this = this.when(stats.other > 0, |this| {
                this.child(row("not numbers", stats.other.to_string().into()))
            });
            let this = this.child(row("sum", stats.sum.clone().into()));
            let this = if stats.nulls > 0 && stats.other == 0 {
                this.child(row("avg (all rows)", stats.avg_all.clone().into()))
                    .child(row("avg (non-null)", stats.avg.clone().into()))
            } else {
                this.child(row("avg", stats.avg.clone().into()))
            };
            this.child(row("min", stats.min.clone().into()))
                .child(row("max", stats.max.clone().into()))
        })
}

/// Summary of a column; numbers as the server wrote them.
#[derive(Debug, Clone, PartialEq)]
struct ColumnStats {
    /// All rows looked at.
    rows: usize,
    /// Non-NULL values.
    count: usize,
    nulls: usize,
    /// Different non-NULL values.
    distinct: usize,
    /// Non-NULL values that are numbers (the sums are over these) …
    numbers: usize,
    /// … and those that are not.
    other: usize,
    sum: String,
    /// Sum over all rows, NULL counting as 0.
    avg_all: String,
    /// Sum over the non-NULL values only (like SQL's AVG).
    avg: String,
    min: String,
    max: String,
}

impl ColumnStats {
    /// Whether to show sums: mostly numbers (one stray text value doesn't hide
    /// them; a text column with a few number-like values doesn't get them).
    fn numeric(&self) -> bool {
        self.numbers > 0 && self.numbers > self.other
    }
}

/// Summary of `values`, `None` without any. With `numeric`, the values that are
/// numbers are summed: plain decimals exactly (no float rounding in a sum of
/// prices), anything else (exponents, huge values) in floating point.
fn column_stats<'a>(
    values: impl Iterator<Item = Option<&'a str>>,
    numeric: bool,
) -> Option<ColumnStats> {
    let mut count = 0;
    let mut numbers = 0;
    let mut distinct = std::collections::HashSet::new();
    let mut nulls = 0;
    let mut float_sum = 0f64;
    // Sum as an integer of `scale` decimal places, while every value fits.
    let mut exact: Option<(i128, u32)> = Some((0, 0));
    let mut min: Option<(f64, &str)> = None;
    let mut max: Option<(f64, &str)> = None;
    for value in values {
        let Some(value) = value.map(str::trim) else {
            nulls += 1;
            continue;
        };
        count += 1;
        distinct.insert(value);
        // NaN and infinities parse, but would make every aggregate NaN / inf.
        let Some(x) = numeric
            .then(|| value.parse::<f64>().ok())
            .flatten()
            .filter(|x| x.is_finite())
        else {
            continue;
        };
        numbers += 1;
        float_sum += x;
        exact = exact.and_then(|(sum, scale)| {
            let (v, s) = decimal(value)?;
            let target = scale.max(s);
            let sum = sum.checked_mul(10i128.checked_pow(target - scale)?)?;
            let v = v.checked_mul(10i128.checked_pow(target - s)?)?;
            Some((sum.checked_add(v)?, target))
        });
        if min.is_none_or(|(m, _)| x < m) {
            min = Some((x, value));
        }
        if max.is_none_or(|(m, _)| x > m) {
            max = Some((x, value));
        }
    }
    if count == 0 && nulls == 0 {
        return None;
    }
    let rows = count + nulls;
    let (sum, sum_f, decimals) = match exact {
        Some((sum, scale)) => (
            format_decimal(sum, scale),
            sum as f64 / 10f64.powi(scale as i32),
            scale + 2,
        ),
        None => (format_float(float_sum, 6), float_sum, 6),
    };
    let avg = format_float(sum_f / numbers.max(1) as f64, decimals);
    let avg_all = format_float(sum_f / rows.max(1) as f64, decimals);
    Some(ColumnStats {
        rows,
        count,
        nulls,
        distinct: distinct.len(),
        numbers,
        other: count - numbers,
        sum,
        avg_all,
        avg,
        min: min.map_or_else(String::new, |(_, v)| v.to_string()),
        max: max.map_or_else(String::new, |(_, v)| v.to_string()),
    })
}

/// `-12.345` → `(-12345, 3)`; `None` for exponents and the like.
fn decimal(value: &str) -> Option<(i128, u32)> {
    let (int, frac) = value.split_once('.').unwrap_or((value, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    let unsigned = int.strip_prefix(['-', '+']).unwrap_or(int);
    if !digits(unsigned) || !digits(frac) || frac.len() > 30 {
        return None;
    }
    let v: i128 = format!("{int}{frac}").parse().ok()?;
    Some((v, frac.len() as u32))
}

fn format_decimal(v: i128, scale: u32) -> String {
    if scale == 0 {
        return v.to_string();
    }
    let digits = v.unsigned_abs().to_string();
    let digits = format!("{digits:0>width$}", width = scale as usize + 1);
    let (int, frac) = digits.split_at(digits.len() - scale as usize);
    let sign = if v < 0 { "-" } else { "" };
    format!("{sign}{int}.{frac}")
}

/// At most `decimals` places, without trailing zeros.
fn format_float(x: f64, decimals: u32) -> String {
    let s = format!("{x:.*}", decimals.min(12) as usize);
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// An overlay over a whole cell: the table pads what a cell (or a header) draws,
/// and marks reach over that padding to the cell's edges, like the table's own
/// selected cell. The grid's size is `xsmall` (see `ResultView::render_body`).
fn over_cell<E: Styled>(overlay: E) -> E {
    let p = Size::XSmall.table_cell_padding();
    overlay
        .absolute()
        .top(-p.top)
        .bottom(-p.bottom)
        .left(-p.left)
        .right(-p.right)
}

/// An overlay over a cell drawn after the rows, so it also covers the row's line
/// under the cell (the table clips a cell to above that line). Otherwise clipped
/// like the cell's text: to it and to the grid's view, grown by the line only where
/// the cell's bottom is in view (nothing reaches past the grid's bottom edge).
///
/// The clip is taken while painting: while laying out, the table clips the right
/// edge short by its scrollbar's width, so a mark taken from then stopped short of
/// the last visible column's end. The overlay paints after the rows, with it.
struct OverRowLine {
    child: Option<AnyElement>,
    priority: usize,
    clip: Rc<Cell<Option<Bounds<Pixels>>>>,
}

fn over_row_line(child: impl IntoElement, priority: usize) -> OverRowLine {
    let clip = Rc::<Cell<Option<Bounds<Pixels>>>>::default();
    let child = Clipped {
        child: child.into_any_element(),
        clip: clip.clone(),
    };
    OverRowLine {
        child: Some(child.into_any_element()),
        priority,
        clip,
    }
}

impl Element for OverRowLine {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let child = self.child.as_mut().expect("laid out once");
        (child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) {
        let child = self.child.take().expect("prepainted once");
        window.defer_draw(child, window.element_offset(), self.priority, None);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) {
        let mut clip = window.content_mask().bounds;
        // The overlay reaches over the line; the cell's clip ends just above it.
        if clip.bottom() >= bounds.bottom() - ROW_LINE {
            clip.size.height = bounds.bottom() - clip.top();
        }
        self.clip.set(Some(clip));
    }
}

impl IntoElement for OverRowLine {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// What [`OverRowLine`] draws later, painted within the clip it took.
struct Clipped {
    child: AnyElement,
    clip: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl Element for Clipped {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        // Nothing to clip to: the cell wasn't painted, so neither is its mark.
        let Some(bounds) = self.clip.get() else {
            return;
        };
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            self.child.paint(window, cx)
        });
    }
}

impl IntoElement for Clipped {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// The keys held, as the mark takes them.
fn keys(modifiers: Modifiers) -> Keys {
    Keys {
        shift: modifiers.shift,
        secondary: modifiers.secondary(),
    }
}

/// What a column's filter popover hangs from: nothing to see or click (the filter
/// zone around it opens and closes the popover).
#[derive(IntoElement, Default)]
struct PopoverAnchor {
    selected: bool,
}

impl Selectable for PopoverAnchor {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for PopoverAnchor {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div().size_0()
    }
}

/// "Copy 3 rows", "Copy row".
fn rows_label(verb: &str, n: usize) -> String {
    if n == 1 {
        format!("{verb} row")
    } else {
        format!("{verb} {n} rows")
    }
}

/// "DELETE statements for 3 rows", "DELETE statement for 1 row".
fn delete_label(n: usize) -> String {
    if n == 1 {
        "DELETE statement for 1 row".into()
    } else {
        format!("DELETE statements for {n} rows")
    }
}

fn copy_item(label: String, text: String) -> PopupMenuItem {
    PopupMenuItem::new(label).on_click(move |_, _, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
    })
}

impl TableDelegate for ResultsDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.result.row_count()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    /// Key icon and name (a click marks the column), then the filter zone: the free
    /// space right of the name, with the funnel / sort icon on hover, opens the
    /// column's filter and sort popover. Below the cell on hover, the aggregates.
    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let facts = self.facts.get(col_ix).cloned().flatten();
        let key_icon = facts.clone().and_then(|f| self.key_icon(col_ix, &f, cx));
        let theme = cx.theme();
        let name = self.columns[col_ix].name.clone();
        let hovered = self.hovered == Some(col_ix);
        let open = self.filter_open == Some(col_ix);
        let filtered = self.refine.filter(col_ix).is_some();
        let sorted = self.refine.sort.filter(|s| s.column == col_ix);
        let active = filtered || sorted.is_some();

        let header_fill = self
            .mark
            .col_marked(col_ix)
            .then(|| crate::theme::grid(cx).header_mark);
        let table = cx.entity().downgrade();
        let filter_zone = self.refinable.then(|| {
            let icon = match sorted {
                Some(s) if !filtered => Icon::new(if s.descending {
                    IconName::SortDescending
                } else {
                    IconName::SortAscending
                }),
                _ => Icon::new(Lucide::Funnel),
            };
            let icon = if active {
                icon.text_color(theme.primary)
            } else {
                icon.text_color(theme.muted_foreground)
            };
            let panel = self.filter_panel.clone();
            // Opened from outside, the popover focuses itself as it is drawn: the
            // value field must be the one it focuses.
            let focus = open
                .then(|| panel.as_ref().and_then(|p| p.read(cx).focus_target(cx)))
                .flatten();
            h_flex()
                .id(("col-filter-button", col_ix))
                .test_support()
                .relative()
                .flex_1()
                .min_w(FILTER_ZONE)
                .h_full()
                .group(SharedString::from(format!("col-filter-{col_ix}")))
                .justify_end()
                .items_center()
                // The button: a square at the right; the free space left of it
                // opens the popover too.
                .when(hovered || open || active, |this| {
                    this.child(
                        div()
                            .flex()
                            .flex_none()
                            .size(FILTER_BUTTON)
                            .items_center()
                            .justify_center()
                            .rounded(theme.radius)
                            .when(open, |this| this.bg(theme.tokens.table_hover))
                            .group_hover(format!("col-filter-{col_ix}"), |this| {
                                this.bg(theme.tokens.table_hover)
                            })
                            .child(icon.size_4()),
                    )
                })
                // Clicked while open: the popover may have closed on this very
                // click already (it closes on clicks outside it), so only a state
                // still as drawn is toggled, like gpui-kit's own triggers.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |table, _, window, cx| {
                        cx.stop_propagation();
                        let d = table.delegate_mut();
                        if (d.filter_open == Some(col_ix)) == open {
                            d.filter_opened(col_ix, !open, window, cx);
                        }
                        cx.notify();
                    }),
                )
                .child(
                    // The popover hangs from the zone's bottom right corner; the
                    // zone opens and closes it.
                    div().absolute().right_0().bottom_0().child(
                        Popover::new(("col-filter", col_ix))
                            .anchor(Anchor::TopRight)
                            .trigger(PopoverAnchor::default())
                            .when_some(focus, |p, focus| p.track_focus(&focus))
                            .open(open)
                            .on_open_change(move |open, window, cx| {
                                let _ = table.update(cx, |t, cx| {
                                    t.delegate_mut().filter_opened(col_ix, *open, window, cx);
                                    cx.notify();
                                });
                            })
                            .content(move |_, _, _| {
                                panel
                                    .clone()
                                    .map(|p| p.into_any_element())
                                    .unwrap_or_else(|| div().into_any_element())
                            }),
                    ),
                )
        });

        let stats_card = (hovered && self.filter_open.is_none() && self.key_card.is_none())
            .then(|| self.stats(col_ix).cloned())
            .flatten()
            .map(|stats| {
                let scope = if self.more {
                    format!("{} loaded rows", self.result.row_count())
                } else {
                    format!("All {} rows", self.result.row_count())
                };
                let marked = self
                    .marked_stats(col_ix)
                    .cloned()
                    .map(|marked| (format!("Marked: {} rows", marked.rows), marked));
                // Above the rows' handles (deferred too).
                div().absolute().top_full().left_1_2().child(
                    deferred(
                        anchored()
                            .anchor(Anchor::TopCenter)
                            .offset(point(px(0.), px(8.)))
                            .snap_to_window_with_margin(px(8.))
                            .child(stats_card(scope, &stats, marked, facts.as_ref(), cx)),
                    )
                    .with_priority(LAYER_SUMMARY),
                )
            });

        h_flex()
            .id(("th", col_ix))
            .relative()
            .size_full()
            .gap_1()
            .items_center()
            .on_hover(cx.listener(move |table, hovered: &bool, _, cx| {
                let d = table.delegate_mut();
                if *hovered {
                    d.hovered = Some(col_ix);
                } else if d.hovered == Some(col_ix) {
                    d.hovered = None;
                }
                cx.notify();
            }))
            .when_some(header_fill, |this, fill| {
                this.child(over_cell(div()).bg(fill))
            })
            .child({
                let s = settings::current(cx);
                h_flex()
                    .id(("th-name", col_ix))
                    .test_support()
                    .min_w_0()
                    .h_full()
                    .gap_1()
                    .items_center()
                    .children(key_icon)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .when_some(s.table.family(), |d, family| {
                                d.font_family(SharedString::from(family.to_string()))
                            })
                            .when_some(s.table_font_size(), |d, size| d.text_size(px(size)))
                            .child(name),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |table, e: &MouseDownEvent, _, cx| {
                            let d = table.delegate_mut();
                            d.mark.click_column(col_ix, keys(e.modifiers));
                            d.marked();
                            cx.notify();
                        }),
                    )
            })
            .children(filter_zone)
            .children(stats_card)
    }

    fn has_more(&self, _: &App) -> bool {
        self.more
    }

    fn load_more_threshold(&self) -> usize {
        200
    }

    fn load_more(&mut self, _: &mut Window, cx: &mut Context<TableState<Self>>) {
        // The table is being updated right now and the view reads it: afterwards.
        if let Some(view) = self.view.clone() {
            cx.defer(move |cx| {
                let _ = view.update(cx, |view, cx| view.load_more(cx));
            });
        }
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        // A column mark covers every row: tinting them all would tint the grid.
        let selected = !self.mark.is_column_mark() && self.mark.row_marked(row_ix);
        let highlight = crate::theme::grid(cx).row;
        // Clicks are handled by the cells: the table stops them there (it selects
        // the cell), so they never reach the row.
        div()
            .id(("row", row_ix))
            .relative()
            .when(selected, |this| {
                this.child(div().absolute().inset_0().bg(highlight))
            })
            .child(self.row_handle(row_ix, cx))
    }

    fn context_menu(
        &mut self,
        row_ix: usize,
        mut menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let rows = self.menu_rows(row_ix);
        let n = rows.len();
        let n_cols = self.result.columns().len();
        if let (Some(col), Some(view)) = (self.menu_col.filter(|&c| c < n_cols), self.view.clone())
        {
            // The value viewer: this cell as A, or as B next to the inspected one.
            let inspect = view.clone();
            menu = menu.item(
                PopupMenuItem::new("Inspect value").on_click(move |_, _, cx| {
                    let _ = inspect.update(cx, |v, cx| v.inspect_cell(row_ix, col, cx));
                }),
            );
            if self.inspected.is_some_and(|a| a != (row_ix, col)) {
                menu = menu.item(
                    PopupMenuItem::new("Compare with A").on_click(move |_, _, cx| {
                        let _ = view.update(cx, |v, cx| v.compare(row_ix, col, cx));
                    }),
                );
            }
            menu = menu.separator();
        }
        // The marked columns, or the clicked one / all when the mark is gone.
        let cols = match self.mark.cols() {
            c if !c.is_empty() => c,
            _ => self
                .menu_col
                .filter(|&c| c < n_cols)
                .map_or_else(|| (0..n_cols).collect(), |c| vec![c]),
        };
        let all_cols: Vec<usize> = (0..n_cols).collect();
        let names = self.column_names();
        let cell = |r, c| self.result.cell(r, c);
        let table = cx.entity().downgrade();
        menu = menu
            .item(copy_item("Copy".into(), tsv(&rows, &cols, cell, None)))
            .item(copy_item(
                "Copy with headers".into(),
                tsv(&rows, &cols, cell, Some(&names)),
            ))
            .item(copy_item(
                rows_label("Copy", n),
                tsv(&rows, &all_cols, cell, None),
            ))
            .item(PopupMenuItem::new("Mark row").on_click(move |_, _, cx| {
                let _ = table.update(cx, |t, cx| {
                    let d = t.delegate_mut();
                    d.mark.click_row(row_ix, Keys::default(), n_cols);
                    d.marked();
                    cx.notify();
                });
            }));

        if let Some(edit) = &self.edit {
            let values = self.row_values(&rows);
            let title = |verb: &str| format!("{verb} {}", edit.table);
            menu = menu.separator();
            let open = |sql: Option<String>| {
                sql.map(|sql| ResultEvent::OpenSql {
                    title: title("update"),
                    sql,
                })
            };
            // UPDATE submenu: the clicked cell under its column name (one value for all
            // rows, or each row's own), and the whole row when only one is selected.
            let mut cell_items = Vec::new();
            let column = self.menu_col.filter(|&c| edit.editable(c)).map(|col| {
                let value = self.result.cell(row_ix, col);
                if n == 1 {
                    cell_items.push(
                        self.open_item("Cell".into(), open(edit.update_cell(col, value, &values))),
                    );
                } else {
                    cell_items.push(self.open_item(
                        format!("One value for {n} rows"),
                        open(edit.update_cell(col, value, &values)),
                    ));
                    cell_items.push(self.open_item(
                        "Each row's own value".into(),
                        open(edit.update_cells(col, &values)),
                    ));
                }
                self.result.columns()[col].name.clone()
            });
            let row_item = match &values[..] {
                [row] => Some(self.open_item("Row".into(), open(edit.update_row(row)))),
                _ => None,
            };
            if column.is_some() || row_item.is_some() {
                let submenu = PopupMenu::build(window, cx, move |mut menu, _, _| {
                    if let Some(name) = column {
                        menu = cell_items
                            .into_iter()
                            .fold(menu.label(name), PopupMenu::item);
                    }
                    if let Some(row) = row_item {
                        if !menu.is_empty() {
                            menu = menu.separator();
                        }
                        menu = menu.item(row);
                    }
                    menu
                });
                menu = menu.item(PopupMenuItem::submenu("UPDATE", submenu));
            }
            menu = menu.item(self.open_item(
                delete_label(n),
                edit.delete(&values).map(|sql| ResultEvent::OpenSql {
                    title: title("delete"),
                    sql,
                }),
            ));
        }

        if !self.related.is_empty() {
            menu = menu.separator().label("Show related");
            for link in self.related.iter().take(RELATED_SHOWN) {
                menu = menu.item(self.related_item(link, &rows));
            }
            if self.related.len() > RELATED_SHOWN {
                let items: Vec<PopupMenuItem> = self
                    .related
                    .iter()
                    .map(|link| self.related_item(link, &rows))
                    .collect();
                let all = PopupMenu::build(window, cx, move |menu, _, _| {
                    items.into_iter().fold(menu, PopupMenu::item)
                });
                menu = menu.item(PopupMenuItem::submenu(
                    format!("Show all ({})", self.related.len()),
                    all,
                ));
            }
        }
        menu
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let s = settings::current(cx);
        let family = s.table.family().map_or_else(
            || cx.theme().mono_font_family.clone(),
            |f| f.to_string().into(),
        );
        let font_size = s
            .table_font_size()
            .map_or_else(|| rems(0.75).to_pixels(window.rem_size()), px);
        let loaded = self.result.row_count();
        let marked = self.mark.contains(row_ix, col_ix).then(|| {
            let (top, right, bottom, left) = self.mark.edges(row_ix, col_ix, loaded);
            (crate::theme::grid(cx), top, right, bottom, left)
        });
        let theme = cx.theme();
        let mark = if self.inspected == Some((row_ix, col_ix)) {
            Some(theme.info)
        } else if self.compared == Some((row_ix, col_ix)) {
            Some(theme.warning)
        } else {
            None
        };
        let cell = div()
            .id(("cell", col_ix))
            .test_support()
            .relative()
            .size_full()
            .font_family(family.clone())
            .text_xs()
            .when_some(s.table_font_size(), |d, size| d.text_size(px(size)))
            .when_some(marked, |d, (style, top, right, bottom, left)| {
                let (width, radius) = (style.mark_border_width, style.mark_radius);
                let p = Size::XSmall.table_cell_padding();
                d.child(over_row_line(
                    // Down over the row's line, so marked rows run together.
                    over_cell(div().id(("mark-fill", col_ix)).test_support())
                        .bottom(-(p.bottom + ROW_LINE))
                        .bg(style.mark)
                        .border_color(style.mark_border)
                        .when(top, |d| d.border_t(width))
                        .when(right, |d| d.border_r(width))
                        .when(bottom, |d| d.border_b(width))
                        .when(left, |d| d.border_l(width))
                        // The mark's outer corners.
                        .when(top && left, |d| d.rounded_tl(radius))
                        .when(top && right, |d| d.rounded_tr(radius))
                        .when(bottom && right, |d| d.rounded_br(radius))
                        .when(bottom && left, |d| d.rounded_bl(radius)),
                    LAYER_MARK,
                ))
            })
            .when_some(mark, |d, color| {
                d.child(over_row_line(
                    over_cell(div())
                        .border_1()
                        .border_color(color)
                        .bg(color.opacity(0.12)),
                    LAYER_INSPECTED,
                ))
            })
            // The table selects the cell and stops the event, so the row
            // selection happens here, before it.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |table, e: &MouseDownEvent, _, cx| {
                    let d = table.delegate_mut();
                    if e.modifiers.alt {
                        // B: the table must not select the cell.
                        cx.stop_propagation();
                        if let Some(view) = d.view.clone() {
                            cx.defer(move |cx| {
                                let _ = view.update(cx, |v, cx| v.compare(row_ix, col_ix, cx));
                            });
                        }
                    } else {
                        d.start_drag(Drag::Cells(row_ix, col_ix), e.position);
                        d.selecting(row_ix, col_ix);
                        d.mark.click_cell(row_ix, col_ix, keys(e.modifiers));
                        d.marked();
                    }
                    cx.notify();
                }),
            )
            // Dragging from a cell marks the rectangle to the cell under the pointer.
            .on_mouse_move(cx.listener(move |table, e: &MouseMoveEvent, _, cx| {
                let d = table.delegate_mut();
                if e.pressed_button != Some(MouseButton::Left) {
                    if d.end_drag() {
                        cx.notify();
                    }
                    return;
                }
                if d.drag.is_some() {
                    d.drag_pointer = e.position;
                    cx.notify();
                }
                if matches!(d.drag, Some(Drag::Cells(r, c)) if (r, c) != (row_ix, col_ix)) {
                    // Dragged off the cell: no click follows for the table.
                    d.clicked.clear();
                    d.mark.drag_to(row_ix, col_ix);
                    d.drag = Some(Drag::Cells(row_ix, col_ix));
                    d.marked();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|table, _, _, cx| {
                    if table.delegate_mut().end_drag() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |table, _, _, cx| {
                    // In cell mode the table takes a right click for itself and
                    // stops it, so the context menu never opens. For this click it
                    // is off: the row records the click and the menu opens.
                    table.cell_selectable = false;
                    let this = cx.entity();
                    cx.defer(move |cx| this.update(cx, |t, _| t.cell_selectable = true));
                    let d = table.delegate_mut();
                    d.menu_col = Some(col_ix);
                    if !d.mark.contains(row_ix, col_ix) {
                        d.mark.click_cell(row_ix, col_ix, Keys::default());
                        d.marked();
                    }
                    cx.notify();
                }),
            );
        // The value, clipped to the cell (the marks above reach over its padding).
        let value = div().size_full().overflow_hidden().whitespace_nowrap();
        cell.child(match self.result.cell(row_ix, col_ix) {
            None => value.text_color(theme.muted_foreground).child("NULL"),
            // Nothing more to show for an empty string.
            Some("") => value,
            Some(text) => {
                let full: SharedString = peek_text(text).into();
                value.child(
                    HoverCard::new(("peek", col_ix))
                        .anchor(Anchor::TopLeft)
                        .open_delay(PEEK_DELAY)
                        .appearance(false)
                        .trigger(div().child(display_value(text)))
                        .content(move |_, _, cx| {
                            peek_card(full.clone(), family.clone(), font_size, cx)
                        }),
                )
            }
        })
    }
}

/// The start of `value` for the hover card.
fn peek_text(value: &str) -> String {
    match value.char_indices().nth(PEEK_CHARS) {
        Some((i, _)) => format!("{}…", &value[..i]),
        None => value.to_string(),
    }
}

/// A value in full (up to [`PEEK_CHARS`]), wrapped, with how to see all of it.
fn peek_card(text: SharedString, family: SharedString, size: Pixels, cx: &App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .max_w(px(PEEK_WIDTH))
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .shadow_md()
        .child(
            div()
                .id("peek-text")
                .test_support()
                .max_h(px(PEEK_HEIGHT))
                .overflow_hidden()
                .px_2()
                .py_1p5()
                .font_family(family)
                .text_size(size)
                .whitespace_normal()
                .child(text),
        )
        .child(
            div()
                .px_2()
                .py_1()
                .border_t_1()
                .border_color(theme.border)
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("Space to open the full value"),
        )
        .into_any_element()
}

/// Single-line, length-limited representation for the grid.
fn display_value(value: &str) -> SharedString {
    let mut out = String::with_capacity(value.len().min(MAX_CELL_CHARS + 1));
    for (i, ch) in value.chars().enumerate() {
        if i == MAX_CELL_CHARS {
            out.push('…');
            break;
        }
        out.push(match ch {
            '\n' | '\r' | '\t' => ' ',
            c => c,
        });
    }
    out.into()
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` attribute would shadow the standard one.
    use super::{
        KeyMarker, ResultsDelegate, column_stats, delete_label, format_decimal, rows_label,
    };
    use tangul_assist::columns::{ColumnFacts, ForeignRef};
    use tangul_assist::mark::{Keys, tsv};
    use tangul_core::{ColumnInfo, RowBatch};

    fn delegate() -> ResultsDelegate {
        let mut d = ResultsDelegate::default();
        let col = |n: &str| ColumnInfo { name: n.into() };
        d.reset(vec![col("id"), col("note")]);
        let mut batch = RowBatch::with_capacity(6);
        for row in [
            [Some("1"), Some("a\tb")],
            [Some("2"), None],
            [Some("3"), Some("say \"hi\"")],
        ] {
            for cell in row {
                batch.push_cell(cell);
            }
            batch.finish_row();
        }
        d.result.append(batch);
        d
    }

    /// A delegate holding one numeric column `n` with these values.
    fn numbers(values: &[Option<&str>]) -> ResultsDelegate {
        let mut d = ResultsDelegate::default();
        d.reset(vec![ColumnInfo { name: "n".into() }]);
        let mut batch = RowBatch::default();
        for v in values {
            batch.push_cell(*v);
            batch.finish_row();
        }
        d.result.append(batch);
        d
    }

    #[test]
    fn copies_tsv() {
        let d = delegate();
        let cell = |r, c| d.result.cell(r, c);
        let names = d.column_names();
        assert_eq!(
            tsv(&[0, 1], &[0, 1], cell, Some(&names)),
            "id\tnote\n1\t\"a\tb\"\n2\t"
        );
        assert_eq!(tsv(&[2], &[0, 1], cell, None), "3\t\"say \"\"hi\"\"\"");
        // Only the marked columns.
        assert_eq!(tsv(&[0, 1], &[1], cell, Some(&names)), "note\n\"a\tb\"\n");
    }

    #[test]
    fn labels_state_the_count() {
        assert_eq!(rows_label("Copy", 1), "Copy row");
        assert_eq!(rows_label("Copy", 3), "Copy 3 rows");
        assert_eq!(delete_label(1), "DELETE statement for 1 row");
        assert_eq!(delete_label(100), "DELETE statements for 100 rows");
    }

    #[test]
    fn aggregates() {
        let s = column_stats(
            [Some("0.1"), Some("0.2"), None, Some("-1.25"), Some("3")].into_iter(),
            true,
        )
        .unwrap();
        assert_eq!((s.rows, s.count, s.nulls), (5, 4, 1));
        assert_eq!(s.sum, "2.05");
        assert_eq!(s.avg, "0.5125");
        assert_eq!(s.avg_all, "0.41");
        assert_eq!((s.min.as_str(), s.max.as_str()), ("-1.25", "3"));
        let s = column_stats([Some("1e3"), Some("2")].into_iter(), true).unwrap();
        assert_eq!((s.sum.as_str(), s.avg.as_str()), ("1002", "501"));
        // A stray value that is not a number is counted, not a reason to drop
        // the sums; NaN and infinities are not summed either.
        let s = column_stats(
            [Some("1"), Some("abc"), Some("2"), Some("NaN"), Some("3")].into_iter(),
            true,
        )
        .unwrap();
        assert!(s.numeric());
        assert_eq!((s.numbers, s.other, s.distinct), (3, 2, 5));
        assert_eq!((s.sum.as_str(), s.avg.as_str()), ("6", "2"));
        assert_eq!((s.min.as_str(), s.max.as_str()), ("1", "3"));
        assert_eq!(format_decimal(-5, 2), "-0.05");
        assert_eq!(format_decimal(1234, 0), "1234");
    }

    #[test]
    fn other_columns_get_counts() {
        // Text: rows, nulls, distinct; no sums.
        let s = column_stats([Some("a"), Some("b"), Some("a"), None].into_iter(), true).unwrap();
        assert!(!s.numeric());
        assert_eq!((s.rows, s.count, s.nulls, s.distinct), (4, 3, 1, 2));
        // Mostly text with a number-like value: still no sums.
        let s = column_stats([Some("a"), Some("12"), Some("b")].into_iter(), true).unwrap();
        assert!(!s.numeric());
        // A column the schema says is text: no sums, however it looks.
        let s = column_stats([Some("1"), Some("2")].into_iter(), false).unwrap();
        assert!(!s.numeric());
        assert_eq!((s.count, s.distinct, s.other), (2, 2, 2));
    }

    #[test]
    fn averages_with_nulls() {
        // 10, NULL, 20, NULL: SQL's AVG is 15; over all four rows it is 7.5.
        let s = column_stats([Some("10"), None, Some("20"), None].into_iter(), true).unwrap();
        assert_eq!((s.rows, s.count, s.nulls), (4, 2, 2));
        assert_eq!(
            (s.sum.as_str(), s.avg.as_str(), s.avg_all.as_str()),
            ("30", "15", "7.5")
        );
        assert_eq!((s.min.as_str(), s.max.as_str()), ("10", "20"));
        // No NULLs: both averages agree.
        let s = column_stats([Some("1"), Some("2")].into_iter(), true).unwrap();
        assert_eq!((s.avg.as_str(), s.avg_all.as_str()), ("1.5", "1.5"));
        // Only NULLs: counted, nothing to sum.
        let s = column_stats([None, None].into_iter(), true).unwrap();
        assert_eq!((s.rows, s.count, s.nulls), (2, 0, 2));
        // Nothing at all.
        assert!(column_stats(std::iter::empty(), true).is_none());
    }

    #[test]
    fn large_sums_stay_exact() {
        // 0.01 added 100 000 times drifts in floating point; the sum is exact.
        let s = column_stats(std::iter::repeat_n(Some("0.01"), 100_000), true).unwrap();
        assert_eq!(
            (s.rows, s.sum.as_str(), s.avg.as_str()),
            (100_000, "1000.00", "0.01")
        );
        // Integers beyond f64's exact range.
        let big = "9007199254740993";
        let s = column_stats([Some(big), Some(big)].into_iter(), true).unwrap();
        assert_eq!(s.sum, "18014398509481986");
        // 1..=10 000 plus as many NULLs.
        let values: Vec<String> = (1..=10_000).map(|i| i.to_string()).collect();
        let s = column_stats(
            values
                .iter()
                .map(|v| Some(v.as_str()))
                .chain(std::iter::repeat_n(None, 10_000)),
            true,
        )
        .unwrap();
        assert_eq!((s.rows, s.count, s.nulls), (20_000, 10_000, 10_000));
        assert_eq!((s.sum.as_str(), s.avg.as_str()), ("50005000", "5000.5"));
        assert_eq!(s.avg_all, "2500.25");
        assert_eq!((s.min.as_str(), s.max.as_str()), ("1", "10000"));
    }

    #[test]
    fn stats_cover_all_rows_and_follow_new_results() {
        let mut d = numbers(&[Some("1"), Some("2"), Some("3")]);
        assert_eq!(d.stats(0).unwrap().sum, "6");
        // A mark doesn't narrow the summary (the "Marked" section is separate).
        d.mark.click_cell(1, 0, Keys::default());
        d.marked();
        assert_eq!(d.stats(0).unwrap().sum, "6");

        // A new result with the same number of rows (the bug of #79 / #81).
        d.reset(vec![ColumnInfo { name: "n".into() }]);
        let mut batch = RowBatch::default();
        for v in ["10", "20", "30"] {
            batch.push_cell(Some(v));
            batch.finish_row();
        }
        d.result.append(batch);
        assert_eq!(d.stats(0).unwrap().sum, "60");

        // Filtered or sorted rows replacing the old ones, same count again.
        d.clear_rows();
        let mut batch = RowBatch::default();
        for v in ["7", "7", "7"] {
            batch.push_cell(Some(v));
            batch.finish_row();
        }
        d.result.append(batch);
        assert_eq!(d.stats(0).unwrap().sum, "21");

        // Once the schema says the column is text, only counts.
        d.set_facts(vec![Some(facts("text", false, None))]);
        let s = d.stats(0).unwrap();
        assert!(!s.numeric());
        assert_eq!((s.rows, s.distinct), (3, 1));
    }

    #[test]
    fn the_chip_neither_comes_nor_goes_during_a_drag() {
        use super::Drag;
        let mut d = delegate();
        let at = gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.));
        // A drag from one cell: no chip above the grid, one at the pointer.
        d.start_drag(Drag::Cells(0, 0), at);
        d.mark.click_cell(0, 0, Keys::default());
        d.mark.drag_to(2, 1);
        assert_eq!(d.mark_chip(), None);
        assert_eq!(d.drag_chip().unwrap().0, "2 columns × 3 rows");
        // Released: the chip goes above the grid.
        assert!(d.end_drag());
        assert_eq!(d.mark_chip().as_deref(), Some("2 columns × 3 rows"));
        assert!(d.drag_chip().is_none());
        // A drag that starts with the chip shown keeps it, even on one cell.
        d.start_drag(Drag::Cells(1, 1), at);
        d.mark.click_cell(1, 1, Keys::default());
        assert_eq!(d.mark_chip().as_deref(), Some("1 column × 1 row"));
        d.end_drag();
        assert_eq!(d.mark_chip(), None);
    }

    #[test]
    fn marked_cells_get_their_own_sums() {
        let mut d = numbers(&[Some("1"), Some("2"), Some("3"), None]);
        // Nothing marked, or one row: no section.
        assert!(d.marked_stats(0).is_none());
        d.mark.click_cell(1, 0, Keys::default());
        d.marked();
        assert!(d.marked_stats(0).is_none());
        // Rows 1 to 3.
        let shift = Keys {
            shift: true,
            ..Keys::default()
        };
        d.mark.click_cell(3, 0, shift);
        d.marked();
        let s = d.marked_stats(0).unwrap();
        assert_eq!((s.rows, s.nulls, s.sum.as_str()), (3, 1, "5"));
        // The overall summary stays over all rows.
        assert_eq!(d.stats(0).unwrap().sum, "6");
        // Taking a row out changes the marked sums.
        d.mark.click_cell(
            2,
            0,
            Keys {
                secondary: true,
                ..Keys::default()
            },
        );
        d.marked();
        assert_eq!(d.marked_stats(0).unwrap().sum, "2");
        // A column mark: all loaded rows.
        d.mark.click_column(0, Keys::default());
        d.marked();
        assert_eq!(d.marked_stats(0).unwrap().sum, "6");
    }

    #[test]
    fn copy_text_is_the_mark_or_the_selected_cell() {
        let mut d = delegate();
        assert_eq!(d.copy_text(None), None);
        // No mark: the selected cell alone, NULL empty, values quoted as in TSV.
        assert_eq!(d.copy_text(Some((0, 1))).as_deref(), Some("\"a\tb\""));
        assert_eq!(d.copy_text(Some((1, 1))).as_deref(), Some(""));
        assert_eq!(d.copy_text(Some((9, 0))), None);
        // A mark wins over the selection, without headers.
        d.mark.click_cell(0, 0, Keys::default());
        d.mark.click_cell(
            1,
            1,
            Keys {
                shift: true,
                secondary: false,
            },
        );
        assert_eq!(
            d.copy_text(Some((2, 0))).as_deref(),
            Some("1\t\"a\tb\"\n2\t")
        );
    }

    #[test]
    fn keys_extend_all_and_clear_the_mark() {
        let mut d = delegate();
        assert!(!d.clear_mark());
        assert_eq!(d.mark_chip(), None);
        // From the selected cell, or (0, 0).
        assert_eq!(d.extend_mark(Some((1, 0)), 1, 0), Some((2, 0)));
        assert_eq!(d.mark.rows(3), vec![1, 2]);
        assert_eq!(d.mark_chip().as_deref(), Some("1 column × 2 rows"));
        assert_eq!(d.extend_mark(None, 0, 1), Some((2, 1)));
        // Kept within the result.
        assert_eq!(d.extend_mark(None, 5, 5), Some((2, 1)));
        d.mark_all();
        assert_eq!(
            d.mark_chip().as_deref(),
            Some("2 columns × 3 of 3 rows loaded")
        );
        assert!(d.clear_mark());
        assert!(d.mark.is_empty());
        // One cell: no chip.
        d.mark.click_cell(0, 0, Keys::default());
        assert_eq!(d.mark_chip(), None);
        let mut empty = ResultsDelegate::default();
        assert_eq!(empty.extend_mark(None, 1, 0), None);
    }

    #[test]
    fn an_unmarked_column_has_no_marked_sums() {
        let mut d = delegate();
        d.mark.click_column(1, Keys::default());
        d.marked();
        assert!(d.marked_stats(0).is_none());
        assert_eq!(d.marked_stats(1).unwrap().rows, 3);
    }

    fn facts(data_type: &str, primary_key: bool, references: Option<&str>) -> ColumnFacts {
        ColumnFacts {
            table: "t".into(),
            column: "c".into(),
            data_type: data_type.into(),
            numeric: data_type == "integer",
            primary_key,
            references: references.map(|t| ForeignRef {
                schema: "app".into(),
                table: t.into(),
                column: "id".into(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn key_markers() {
        assert_eq!(
            KeyMarker::of(&facts("integer", true, None)),
            Some(KeyMarker::Primary)
        );
        assert_eq!(
            KeyMarker::of(&facts("integer", true, Some("users"))),
            Some(KeyMarker::PrimaryForeign)
        );
        assert_eq!(
            KeyMarker::of(&facts("integer", false, Some("users"))),
            Some(KeyMarker::Foreign)
        );
        assert_eq!(KeyMarker::of(&facts("text", false, None)), None);
    }

    #[test]
    fn mark_is_cleared_with_the_rows_but_not_by_paging() {
        let mut d = delegate();
        d.mark.click_cell(0, 0, Keys::default());
        d.mark.click_cell(
            2,
            1,
            Keys {
                shift: true,
                ..Keys::default()
            },
        );
        assert_eq!(d.menu_rows(1), [0, 1, 2]);
        d.mark.click_cell(
            1,
            1,
            Keys {
                secondary: true,
                ..Keys::default()
            },
        );
        assert_eq!(d.menu_rows(0), [0, 2]);
        // No mark: the menu acts on the row it was opened on.
        d.mark.clear();
        assert_eq!(d.menu_rows(1), [1]);

        d.mark.click_cell(1, 0, Keys::default());
        let mut batch = RowBatch::default();
        batch.push_cell(Some("4"));
        batch.push_cell(None);
        batch.finish_row();
        d.result.append(batch);
        assert!(d.mark.contains(1, 0));
        d.clear_rows();
        assert!(d.mark.is_empty());
        d.mark.click_cell(1, 0, Keys::default());
        d.reset(Vec::new());
        assert!(d.mark.is_empty());
    }
}
