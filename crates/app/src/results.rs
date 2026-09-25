//! Virtualized result grid: the table only asks for visible cells, and cells are read
//! straight from the ResultSet arena.
//!
//! Row selection is kept here rather than in the table (which knows a single row):
//! click selects, ⇧-click extends, ⌘-click toggles. The right-click menu copies the
//! selection, follows foreign keys to related rows and writes UPDATE / DELETE
//! statements for the selected rows (opened in a new tab, not run).
//!
//! Column headers mark primary-key columns, show count / sum / avg / min / max of a
//! numeric column on hover (over the selected rows, or all loaded ones), and open
//! the column's filter and sort popover. Scrolling to the end loads the next page.

use std::collections::BTreeSet;

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{PopupMenu, PopupMenuItem},
    popover::Popover,
    table::{Column, TableDelegate, TableState},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rusdb_assist::columns::ColumnFacts;
use rusdb_assist::refine::{ColumnKind, Refine};
use rusdb_assist::{edit::RowEdit, related::RelatedLink};
use rusdb_core::{ColumnInfo, ResultSet};

use crate::assets::Lucide;
use crate::column_filter::FilterPanel;
use crate::query_tab::{QueryTab, TabEvent};

/// Longer values are cut in the grid (full value view is a later feature).
const MAX_CELL_CHARS: usize = 200;
/// Related tables listed directly in the menu (the most used); the rest are under
/// "Show all".
const RELATED_SHOWN: usize = 3;

#[derive(Default)]
pub struct ResultsDelegate {
    pub result: ResultSet,
    columns: Vec<Column>,
    selected: BTreeSet<usize>,
    /// Start of a ⇧-click range.
    anchor: Option<usize>,
    /// Column under the last right click, for "Copy cell".
    menu_col: Option<usize>,
    /// Foreign-key links of the current result, most used first (see
    /// `rusdb_assist::related`).
    pub related: Vec<RelatedLink>,
    /// UPDATE / DELETE statements for the rows, if the result is from one table.
    pub edit: Option<RowEdit>,
    /// The owning tab, which opens related queries.
    tab: Option<WeakEntity<QueryTab>>,
    /// Per result column, what the schema says about it (keys, type).
    pub facts: Vec<Option<ColumnFacts>>,
    /// Filters and sort applied to the result (shown in the headers).
    pub refine: Refine,
    /// The result can be filtered, sorted and paged (a single SELECT).
    pub refinable: bool,
    /// More rows than loaded: scrolling to the end loads the next page.
    pub more: bool,
    hovered: Option<usize>,
    /// Column whose filter popover is open.
    filter_open: Option<usize>,
    filter_panel: Option<Entity<FilterPanel>>,
    /// Bumped whenever the selection changes (invalidates `stats`).
    selection_gen: u64,
    /// Aggregates of the hovered column: (column, rows, selection_gen) → stats.
    stats: Option<((usize, usize, u64), Option<ColumnStats>)>,
}

impl ResultsDelegate {
    pub fn new(tab: WeakEntity<QueryTab>, filter_panel: Entity<FilterPanel>) -> Self {
        Self {
            tab: Some(tab),
            filter_panel: Some(filter_panel),
            ..Default::default()
        }
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
        self.selected.clear();
        self.anchor = None;
        self.menu_col = None;
        self.related.clear();
        self.edit = None;
        self.facts.clear();
        self.more = false;
        self.hovered = None;
        self.filter_open = None;
        self.selection_gen += 1;
    }

    /// Drops the rows but keeps the columns (and their widths, keys, filters): a
    /// filtered or sorted version of the result is about to arrive.
    pub fn clear_rows(&mut self) {
        self.result = ResultSet::new(self.result.columns().to_vec());
        self.selected.clear();
        self.anchor = None;
        self.more = false;
        self.filter_open = None;
        self.selection_gen += 1;
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

    /// Aggregates of a numeric column over the selected rows, or all loaded rows.
    fn stats(&mut self, col: usize) -> Option<&ColumnStats> {
        let key = (col, self.result.row_count(), self.selection_gen);
        if self.stats.as_ref().is_none_or(|(k, _)| *k != key) {
            let known_other = matches!(self.facts.get(col),
                Some(Some(f)) if ColumnKind::from_type(&f.data_type) != ColumnKind::Number);
            let stats = if known_other {
                None
            } else if self.selected.is_empty() {
                column_stats((0..self.result.row_count()).map(|r| self.result.cell(r, col)))
            } else {
                column_stats(self.selected.iter().map(|&r| self.result.cell(r, col)))
            };
            self.stats = Some((key, stats));
        }
        self.stats.as_ref().and_then(|(_, s)| s.as_ref())
    }

    pub fn clear(&mut self) {
        self.reset(Vec::new());
    }

    pub fn column_names(&self) -> Vec<String> {
        self.result
            .columns()
            .iter()
            .map(|c| c.name.clone())
            .collect()
    }

    fn click(&mut self, row: usize, modifiers: Modifiers) {
        self.selection_gen += 1;
        match self.anchor {
            Some(anchor) if modifiers.shift => {
                self.selected = (anchor.min(row)..=anchor.max(row)).collect();
            }
            _ if modifiers.secondary() => {
                if !self.selected.remove(&row) {
                    self.selected.insert(row);
                }
                self.anchor = Some(row);
            }
            _ => {
                self.selected = BTreeSet::from([row]);
                self.anchor = Some(row);
            }
        }
    }

    /// The rows a menu opened on `row` acts on: the selection if `row` is in it.
    fn target_rows(&self, row: usize) -> Vec<usize> {
        if self.selected.contains(&row) {
            self.selected.iter().copied().collect()
        } else {
            vec![row]
        }
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
            link.query(&values).map(|sql| TabEvent::OpenRelated {
                title: link.table.clone(),
                sql,
            }),
        )
    }

    /// A menu item that has the owning tab emit `event`; disabled without one.
    fn open_item(&self, label: String, event: Option<TabEvent>) -> PopupMenuItem {
        let tab = self.tab.clone();
        PopupMenuItem::new(label)
            .disabled(event.is_none())
            .on_click(move |_, _, cx| {
                let (Some(tab), Some(event)) = (tab.as_ref().and_then(|t| t.upgrade()), &event)
                else {
                    return;
                };
                tab.update(cx, |_, cx| cx.emit(event.clone()));
            })
    }

    fn tsv(&self, rows: &[usize], headers: bool) -> String {
        let cols = self.result.columns().len();
        let mut lines = Vec::with_capacity(rows.len() + 1);
        if headers {
            let names: Vec<String> = self
                .result
                .columns()
                .iter()
                .map(|c| tsv_field(Some(&c.name)))
                .collect();
            lines.push(names.join("\t"));
        }
        for &row in rows {
            let fields: Vec<String> = (0..cols)
                .map(|col| tsv_field(self.result.cell(row, col)))
                .collect();
            lines.push(fields.join("\t"));
        }
        lines.join("\n")
    }
}

impl ResultsDelegate {
    /// The filter popover of `col` opened or closed; opening fills the tab's panel
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
}

fn stats_card(scope: String, stats: &ColumnStats, cx: &App) -> impl IntoElement {
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
        .child(div().pb_1().text_color(theme.muted_foreground).child(scope))
        .child(row("count", stats.count.to_string().into()))
        .when(stats.nulls > 0, |this| {
            this.child(row("nulls", stats.nulls.to_string().into()))
        })
        .when(stats.count > 0, |this| {
            this.child(row("sum", stats.sum.clone().into()))
                .child(row("avg", stats.avg.clone().into()))
                .child(row("min", stats.min.clone().into()))
                .child(row("max", stats.max.clone().into()))
        })
}

/// Aggregates of a numeric column; values as the server wrote them.
#[derive(Debug, Clone, PartialEq)]
struct ColumnStats {
    /// Non-NULL values.
    count: usize,
    nulls: usize,
    sum: String,
    avg: String,
    min: String,
    max: String,
}

/// Aggregates over `values`, or `None` if one of them is not a number. Plain
/// decimals are summed exactly (no float rounding in a sum of prices); anything
/// else (exponents, huge values) in floating point.
fn column_stats<'a>(values: impl Iterator<Item = Option<&'a str>>) -> Option<ColumnStats> {
    let mut count = 0;
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
        let x: f64 = value.parse().ok()?;
        count += 1;
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
    let (sum, avg) = match exact {
        Some((sum, scale)) => {
            let avg = sum as f64 / 10f64.powi(scale as i32) / count.max(1) as f64;
            (format_decimal(sum, scale), format_float(avg, scale + 2))
        }
        None => (
            format_float(float_sum, 6),
            format_float(float_sum / count.max(1) as f64, 6),
        ),
    };
    Some(ColumnStats {
        count,
        nulls,
        sum,
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

/// A TSV field that pastes cleanly into spreadsheets: NULL is empty, values with
/// tabs, line breaks or quotes are quoted.
fn tsv_field(value: Option<&str>) -> String {
    match value {
        None => String::new(),
        Some(v) if v.contains(['\t', '\n', '\r', '"']) => format!("\"{}\"", v.replace('"', "\"\"")),
        Some(v) => v.to_string(),
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

    /// Key icon, name, and on hover the filter button (over the end of a long name)
    /// and the aggregates below the cell.
    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let name = self.columns[col_ix].name.clone();
        let hovered = self.hovered == Some(col_ix);
        let open = self.filter_open == Some(col_ix);
        let filtered = self.refine.filter(col_ix).is_some();
        let sorted = self.refine.sort.filter(|s| s.column == col_ix);
        let active = filtered || sorted.is_some();

        let key = self
            .facts
            .get(col_ix)
            .and_then(Option::as_ref)
            .and_then(|f| {
                f.primary_key.then(|| match &f.references {
                    // Part of the key that points to another table.
                    Some(target) => (
                        Lucide::Key,
                        theme.info,
                        SharedString::from(format!("Primary key, references {target}")),
                    ),
                    None => (Lucide::KeyRound, theme.warning, "Primary key".into()),
                })
            });
        let key_icon = key.map(|(icon, color, tip)| {
            div()
                .id(("pk", col_ix))
                .flex_none()
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .child(Icon::new(icon).size_3().text_color(color))
        });

        let table = cx.entity().downgrade();
        let filter_button = (self.refinable && (hovered || open || active)).then(|| {
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
                icon
            };
            let panel = self.filter_panel.clone();
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .right_0()
                .flex()
                .items_center()
                .pl_1()
                .bg(theme.tokens.table_head)
                .child(
                    Popover::new(("col-filter", col_ix))
                        .anchor(Anchor::TopRight)
                        .trigger(
                            Button::new(("col-filter-button", col_ix))
                                .icon(icon)
                                .xsmall()
                                .ghost()
                                .selected(open),
                        )
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
                )
        });

        let stats_card = (hovered && self.filter_open.is_none())
            .then(|| self.stats(col_ix).cloned())
            .flatten()
            .map(|stats| {
                let scope = if !self.selected.is_empty() {
                    let n = self.selected.len();
                    format!("{n} selected row{}", if n == 1 { "" } else { "s" })
                } else if self.more {
                    format!("{} loaded rows", self.result.row_count())
                } else {
                    format!("All {} rows", self.result.row_count())
                };
                div().absolute().top_full().left_1_2().child(deferred(
                    anchored()
                        .anchor(Anchor::TopCenter)
                        .offset(point(px(0.), px(8.)))
                        .snap_to_window_with_margin(px(8.))
                        .child(stats_card(scope, &stats, cx)),
                ))
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
            .children(key_icon)
            .child(div().min_w_0().overflow_hidden().child(name))
            .children(filter_button)
            .children(stats_card)
    }

    fn has_more(&self, _: &App) -> bool {
        self.more
    }

    fn load_more_threshold(&self) -> usize {
        200
    }

    fn load_more(&mut self, _: &mut Window, cx: &mut Context<TableState<Self>>) {
        // The table is being updated right now and the tab reads it: afterwards.
        if let Some(tab) = self.tab.clone() {
            cx.defer(move |cx| {
                let _ = tab.update(cx, |tab, cx| tab.load_more(cx));
            });
        }
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let selected = self.selected.contains(&row_ix);
        let highlight = cx.theme().selection.opacity(0.35);
        div()
            .id(("row", row_ix))
            .relative()
            .when(selected, |this| {
                this.child(div().absolute().inset_0().bg(highlight))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |table, e: &MouseDownEvent, _, cx| {
                    table.delegate_mut().click(row_ix, e.modifiers);
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |table, _, _, cx| {
                    let d = table.delegate_mut();
                    if !d.selected.contains(&row_ix) {
                        d.selected = BTreeSet::from([row_ix]);
                        d.anchor = Some(row_ix);
                        d.selection_gen += 1;
                    }
                    cx.notify();
                }),
            )
    }

    fn context_menu(
        &mut self,
        row_ix: usize,
        mut menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let rows = self.target_rows(row_ix);
        let n = rows.len();
        let (s, count) = if n == 1 {
            ("", String::new())
        } else {
            ("s", format!("{n} "))
        };
        if let Some(col) = self.menu_col.filter(|&c| c < self.result.columns().len()) {
            let values: Vec<String> = rows
                .iter()
                .map(|&r| self.result.cell(r, col).unwrap_or_default().to_string())
                .collect();
            menu = menu.item(copy_item(format!("Copy {count}cell{s}"), values.join("\n")));
        }
        menu = menu
            .item(copy_item(
                format!("Copy {count}row{s}"),
                self.tsv(&rows, false),
            ))
            .item(copy_item(
                format!("Copy {count}row{s} with headers"),
                self.tsv(&rows, true),
            ));

        if let Some(edit) = &self.edit {
            let values = self.row_values(&rows);
            let title = |verb: &str| format!("{verb} {}", edit.table);
            menu = menu.separator();
            let open = |sql: Option<String>| {
                sql.map(|sql| TabEvent::OpenSql {
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
                format!("DELETE {count}row{s}"),
                edit.delete(&values).map(|sql| TabEvent::OpenSql {
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
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let cell = div()
            .size_full()
            .font_family(cx.theme().mono_font_family.clone())
            .text_xs()
            .whitespace_nowrap()
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |table, _, _, _| table.delegate_mut().menu_col = Some(col_ix)),
            );
        match self.result.cell(row_ix, col_ix) {
            None => cell.text_color(cx.theme().muted_foreground).child("NULL"),
            Some(value) => cell.child(display_value(value)),
        }
    }
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
    use super::{ResultsDelegate, column_stats, format_decimal};
    use gpui_kit::Modifiers;
    use rusdb_core::{ColumnInfo, RowBatch};

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

    #[test]
    fn copies_tsv() {
        let d = delegate();
        assert_eq!(d.tsv(&[0, 1], true), "id\tnote\n1\t\"a\tb\"\n2\t");
        assert_eq!(d.tsv(&[2], false), "3\t\"say \"\"hi\"\"\"");
    }

    #[test]
    fn aggregates() {
        let s =
            column_stats([Some("0.1"), Some("0.2"), None, Some("-1.25"), Some("3")].into_iter())
                .unwrap();
        assert_eq!((s.count, s.nulls), (4, 1));
        assert_eq!(s.sum, "2.05");
        assert_eq!(s.avg, "0.5125");
        assert_eq!((s.min.as_str(), s.max.as_str()), ("-1.25", "3"));
        let s = column_stats([Some("1e3"), Some("2")].into_iter()).unwrap();
        assert_eq!((s.sum.as_str(), s.avg.as_str()), ("1002", "501"));
        assert!(column_stats([Some("1"), Some("abc")].into_iter()).is_none());
        assert_eq!(format_decimal(-5, 2), "-0.05");
        assert_eq!(format_decimal(1234, 0), "1234");
    }

    #[test]
    fn selection() {
        let mut d = delegate();
        let none = Modifiers::default();
        let shift = Modifiers {
            shift: true,
            ..Default::default()
        };
        d.click(0, none);
        d.click(2, shift);
        assert_eq!(d.target_rows(1), [0, 1, 2]);
        d.click(1, Modifiers::secondary_key());
        assert_eq!(d.target_rows(0), [0, 2]);
        // A menu on an unselected row acts on that row only.
        assert_eq!(d.target_rows(1), [1]);
    }
}
