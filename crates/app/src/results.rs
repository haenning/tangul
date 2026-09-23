//! Virtualized result grid: the table only asks for visible cells, and cells are read
//! straight from the ResultSet arena.

use gpui_kit::component::{
    ActiveTheme as _,
    table::{Column, TableDelegate, TableState},
};
use gpui_kit::*;
use rusdb_core::{ColumnInfo, ResultSet};

/// Longer values are cut in the grid (full value view is a later feature).
const MAX_CELL_CHARS: usize = 200;

#[derive(Default)]
pub struct ResultsDelegate {
    pub result: ResultSet,
    columns: Vec<Column>,
}

impl ResultsDelegate {
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
    }

    pub fn clear(&mut self) {
        self.reset(Vec::new());
    }
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

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let cell = div()
            .font_family(cx.theme().mono_font_family.clone())
            .text_xs()
            .whitespace_nowrap()
            .overflow_hidden();
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
