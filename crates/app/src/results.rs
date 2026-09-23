//! Virtualized result grid: the table only asks for visible cells, and cells are read
//! straight from the ResultSet arena.
//!
//! Row selection is kept here rather than in the table (which knows a single row):
//! click selects, ⇧-click extends, ⌘-click toggles. The right-click menu copies the
//! selection and follows foreign keys to related rows.

use std::collections::BTreeSet;

use gpui_kit::component::{
    ActiveTheme as _,
    menu::{PopupMenu, PopupMenuItem},
    table::{Column, TableDelegate, TableState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rusdb_assist::related::RelatedLink;
use rusdb_core::{ColumnInfo, ResultSet};

use crate::query_tab::{QueryTab, TabEvent};

/// Longer values are cut in the grid (full value view is a later feature).
const MAX_CELL_CHARS: usize = 200;

#[derive(Default)]
pub struct ResultsDelegate {
    pub result: ResultSet,
    columns: Vec<Column>,
    selected: BTreeSet<usize>,
    /// Start of a ⇧-click range.
    anchor: Option<usize>,
    /// Column under the last right click, for "Copy cell".
    menu_col: Option<usize>,
    /// Foreign-key links of the current result (see `rusdb_assist::related`).
    pub related: Vec<RelatedLink>,
    /// The owning tab, which opens related queries.
    tab: Option<WeakEntity<QueryTab>>,
}

impl ResultsDelegate {
    pub fn new(tab: WeakEntity<QueryTab>) -> Self {
        Self {
            tab: Some(tab),
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
                    }
                    cx.notify();
                }),
            )
    }

    fn context_menu(
        &mut self,
        row_ix: usize,
        mut menu: PopupMenu,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
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

        if !self.related.is_empty() {
            menu = menu.separator().label("Show related");
            for link in &self.related {
                let values: Vec<Vec<Option<&str>>> = rows
                    .iter()
                    .map(|&r| {
                        link.source_cols()
                            .iter()
                            .map(|&c| self.result.cell(r, c))
                            .collect()
                    })
                    .collect();
                let query = link.query(&values);
                let (tab, title) = (self.tab.clone(), link.table.clone());
                let label = if link.incoming {
                    format!("← {}", link.title)
                } else {
                    format!("→ {}", link.title)
                };
                menu = menu.item(
                    PopupMenuItem::new(label)
                        .disabled(query.is_none())
                        .on_click(move |_, _, cx| {
                            let (Some(tab), Some(sql)) =
                                (tab.as_ref().and_then(|t| t.upgrade()), &query)
                            else {
                                return;
                            };
                            tab.update(cx, |_, cx| {
                                cx.emit(TabEvent::OpenRelated {
                                    title: title.clone(),
                                    sql: sql.clone(),
                                })
                            });
                        }),
                );
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
    use super::ResultsDelegate;
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
