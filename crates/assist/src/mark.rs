//! The result grid's mark: a set of columns × a set of rows (or "all rows, except
//! …") of the loaded rows, so it is always a clean rectangle in grid order, however it
//! was clicked together. Cells, rectangles, whole columns and whole rows are all
//! marks of this kind; two unrelated rectangles can't be held (on purpose).
//!
//! - click a cell: that cell; ⇧-click or drag to another: the rectangle between them
//!   (from the anchor, the cell clicked first)
//! - click a column name: the column over all rows; ⇧-click another: adds it for the
//!   rows already marked; ⌘/Ctrl-click: removes it (adds it when unmarked)
//! - ⌘/Ctrl-click a row: removes it from the mark (adds it when unmarked), for the
//!   marked columns; removing a row from "all rows" makes "all rows, except …"
//! - ⇧+arrows move the end of the rectangle away from the anchor
//!
//! Row and column numbers are positions in the loaded result (they change with a
//! sort or filter, which clears the mark in the grid).

use std::collections::BTreeSet;

/// The keys held during a click, as the grid's input knows them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Keys {
    pub shift: bool,
    /// ⌘ on macOS, Ctrl elsewhere.
    pub secondary: bool,
}

/// The rows of a mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rows {
    /// Every loaded row (also those loaded later) except these.
    All {
        except: BTreeSet<usize>,
    },
    Only(BTreeSet<usize>),
}

impl Default for Rows {
    fn default() -> Self {
        Self::Only(BTreeSet::new())
    }
}

/// What is marked. Empty when no column or no row is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mark {
    cols: BTreeSet<usize>,
    rows: Rows,
    /// Where a rectangle starts: the cell clicked (not ⇧-clicked) last, as
    /// (row, column).
    anchor: Option<(usize, usize)>,
    /// The other corner of the rectangle (moved by ⇧+arrows and drags).
    end: Option<(usize, usize)>,
}

impl Mark {
    /// A click on a cell.
    /// - plain: that cell (anchor and end there)
    /// - ⇧ with an anchor: the rectangle from the anchor to it (the anchor stays; end
    ///   there); without an anchor like plain
    /// - ⌘/Ctrl: toggles the row in the rows (see [`Mark::toggle_row`]); on an empty
    ///   mark like plain. Anchor and end move to the cell.
    pub fn click_cell(&mut self, row: usize, col: usize, keys: Keys) {
        if keys.secondary && !self.is_empty() {
            self.toggle_row(row);
            self.anchor = Some((row, col));
        } else if let (true, Some(anchor)) = (keys.shift, self.anchor) {
            self.rectangle(anchor, (row, col));
        } else {
            self.rectangle((row, col), (row, col));
            self.anchor = Some((row, col));
        }
        self.end = Some((row, col));
    }

    /// The pointer, button held, moved over a cell after a click: the rectangle from
    /// the anchor to it. Nothing without an anchor.
    pub fn drag_to(&mut self, row: usize, col: usize) {
        let Some(anchor) = self.anchor else {
            return;
        };
        self.rectangle(anchor, (row, col));
        self.end = Some((row, col));
    }

    /// Marks the rectangle between two corners (rows and columns, either order).
    fn rectangle(&mut self, a: (usize, usize), b: (usize, usize)) {
        self.rows = Rows::Only((a.0.min(b.0)..=a.0.max(b.0)).collect());
        self.cols = (a.1.min(b.1)..=a.1.max(b.1)).collect();
    }

    /// A click on a column name.
    /// - plain: that column over all rows (no anchor)
    /// - ⇧: adds the column, rows unchanged (with an area marked: only its rows, so it
    ///   stays a rectangle); on an empty mark like plain
    /// - ⌘/Ctrl: toggles the column; on an empty mark like plain; removing the last
    ///   column clears the mark
    pub fn click_column(&mut self, col: usize, keys: Keys) {
        if self.is_empty() || !(keys.shift || keys.secondary) {
            self.cols = BTreeSet::from([col]);
            self.rows = Rows::All {
                except: BTreeSet::new(),
            };
            self.anchor = None;
            self.end = None;
        } else if keys.secondary {
            if !self.cols.remove(&col) {
                self.cols.insert(col);
            }
            if self.cols.is_empty() {
                self.clear();
            }
        } else {
            self.cols.insert(col);
        }
    }

    /// A click on a row's handle (or "Mark row" in its menu), the result having
    /// `n_cols` columns.
    /// - plain: the whole row (all columns; anchor at its first cell, end at its last)
    /// - ⇧ with an anchor: all columns of the rows from the anchor's row to it (also
    ///   while dragging along the handles); without an anchor like plain
    /// - ⌘/Ctrl: toggles the row; on an empty mark like plain
    pub fn click_row(&mut self, row: usize, keys: Keys, n_cols: usize) {
        if keys.secondary && !self.is_empty() {
            self.toggle_row(row);
            self.anchor = Some((row, 0));
            self.end = Some((row, n_cols.saturating_sub(1)));
            return;
        }
        if n_cols == 0 {
            self.clear();
            return;
        }
        let last = (row, n_cols - 1);
        match self.anchor {
            Some((ar, _)) if keys.shift => {
                self.rectangle((ar, 0), last);
                self.end = Some(last);
            }
            _ => {
                self.rectangle((row, 0), last);
                self.anchor = Some((row, 0));
                self.end = Some(last);
            }
        }
    }

    /// Removes `row` from the rows when it is in them, adds it otherwise; "all rows"
    /// becomes "all rows, except …". Removing the last row clears the mark.
    fn toggle_row(&mut self, row: usize) {
        match &mut self.rows {
            Rows::All { except } => {
                if !except.remove(&row) {
                    except.insert(row);
                }
            }
            Rows::Only(rows) => {
                if !rows.remove(&row) {
                    rows.insert(row);
                } else if rows.is_empty() {
                    self.clear();
                }
            }
        }
    }

    /// ⇧+arrows: moves the end of the rectangle by `d_row`, `d_col`, kept within
    /// `n_rows` × `n_cols`, and marks the rectangle from the anchor to it. Without an
    /// anchor (nothing clicked, or a column mark) both start at `from`, the grid's
    /// selected cell. Nothing in an empty result.
    pub fn extend(
        &mut self,
        from: (usize, usize),
        d_row: isize,
        d_col: isize,
        n_rows: usize,
        n_cols: usize,
    ) {
        if n_rows == 0 || n_cols == 0 {
            return;
        }
        let from = (from.0.min(n_rows - 1), from.1.min(n_cols - 1));
        let anchor = self.anchor.unwrap_or(from);
        let end = match self.anchor {
            Some(_) => self.end.unwrap_or(anchor),
            None => from,
        };
        let step = |at: usize, d: isize, n: usize| at.saturating_add_signed(d).min(n - 1);
        let end = (step(end.0, d_row, n_rows), step(end.1, d_col, n_cols));
        self.rectangle(anchor, end);
        self.anchor = Some(anchor);
        self.end = Some(end);
    }

    /// ⌘/Ctrl+A: every column of `n_cols`, all rows (no anchor).
    pub fn all(&mut self, n_cols: usize) {
        self.cols = (0..n_cols).collect();
        self.rows = Rows::All {
            except: BTreeSet::new(),
        };
        self.anchor = None;
        self.end = None;
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn is_empty(&self) -> bool {
        self.cols.is_empty() || matches!(&self.rows, Rows::Only(rows) if rows.is_empty())
    }

    /// Whether the rows are "all rows" (a column mark, or everything), with or
    /// without exceptions.
    pub fn is_column_mark(&self) -> bool {
        matches!(self.rows, Rows::All { .. })
    }

    pub fn contains(&self, row: usize, col: usize) -> bool {
        self.row_marked(row) && self.col_marked(col)
    }

    /// Whether `row` is one of the mark's rows (false for an empty mark).
    pub fn row_marked(&self, row: usize) -> bool {
        !self.is_empty()
            && match &self.rows {
                Rows::All { except } => !except.contains(&row),
                Rows::Only(rows) => rows.contains(&row),
            }
    }

    /// Whether `col` is one of the mark's columns (false for an empty mark).
    pub fn col_marked(&self, col: usize) -> bool {
        !self.is_empty() && self.cols.contains(&col)
    }

    /// The marked rows among the `loaded` ones, in grid order.
    pub fn rows(&self, loaded: usize) -> Vec<usize> {
        if self.is_empty() {
            return Vec::new();
        }
        match &self.rows {
            Rows::All { except } => (0..loaded).filter(|r| !except.contains(r)).collect(),
            Rows::Only(rows) => rows.iter().copied().filter(|&r| r < loaded).collect(),
        }
    }

    /// The marked columns, in grid order.
    pub fn cols(&self) -> Vec<usize> {
        if self.is_empty() {
            return Vec::new();
        }
        self.cols.iter().copied().collect()
    }

    pub fn anchor(&self) -> Option<(usize, usize)> {
        self.anchor
    }

    /// The rectangle's moving corner as (row, column): where ⇧+arrows continue; None
    /// after a column mark or ⌘/Ctrl+A.
    pub fn end(&self) -> Option<(usize, usize)> {
        self.end
    }

    /// How many cells are marked among the `loaded` rows.
    pub fn cell_count(&self, loaded: usize) -> usize {
        self.rows(loaded).len() * self.cols().len()
    }

    /// For the chip: "3 columns × 120 rows"; a column mark adds what is loaded:
    /// "1 column × 4000 of 4000 rows loaded" (a partial column is never mistaken
    /// for the whole table). Singular for one ("1 column × 1 row"); "" when empty.
    pub fn describe(&self, loaded: usize) -> String {
        if self.is_empty() {
            return String::new();
        }
        let plural =
            |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let cols = plural(self.cols.len(), "column", "columns");
        let rows = self.rows(loaded).len();
        if self.is_column_mark() {
            let noun = if loaded == 1 { "row" } else { "rows" };
            format!("{cols} × {rows} of {loaded} {noun} loaded")
        } else {
            format!("{cols} × {}", plural(rows, "row", "rows"))
        }
    }

    /// Which sides of a marked cell are on the mark's outer edge, where its outline
    /// goes: (top, right, bottom, left), each true where the neighbour that way is
    /// not marked (or there is none: the last of the `loaded` rows has a bottom edge).
    /// All false for a cell outside the mark. Rows toggled out of the middle make
    /// edges around the gap.
    pub fn edges(&self, row: usize, col: usize, loaded: usize) -> (bool, bool, bool, bool) {
        if !self.contains(row, col) {
            return (false, false, false, false);
        }
        (
            row == 0 || !self.contains(row - 1, col),
            !self.contains(row, col + 1),
            row + 1 >= loaded || !self.contains(row + 1, col),
            col == 0 || !self.contains(row, col - 1),
        )
    }
}

/// `rows` × `cols` as TSV that pastes cleanly into spreadsheets, in the order given
/// (pass grid order); with `names` (every column's name, by column number) a header
/// line of the columns' names first. Fields as [`tsv_field`].
pub fn tsv<'a>(
    rows: &[usize],
    cols: &[usize],
    cell: impl Fn(usize, usize) -> Option<&'a str>,
    names: Option<&[String]>,
) -> String {
    let line = |fields: Vec<String>| fields.join("\t");
    let mut lines = Vec::with_capacity(rows.len() + 1);
    if let Some(names) = names {
        let header = cols
            .iter()
            .map(|&c| tsv_field(names.get(c).map(String::as_str)))
            .collect();
        lines.push(line(header));
    }
    for &r in rows {
        lines.push(line(cols.iter().map(|&c| tsv_field(cell(r, c))).collect()));
    }
    lines.join("\n")
}

/// A TSV field: NULL is empty, values with tabs, line breaks or quotes are quoted
/// (quotes doubled).
pub fn tsv_field(value: Option<&str>) -> String {
    match value {
        None => String::new(),
        Some(v) if v.contains(['\t', '\n', '\r', '"']) => format!("\"{}\"", v.replace('"', "\"\"")),
        Some(v) => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Keys = Keys {
        shift: false,
        secondary: false,
    };
    const SHIFT: Keys = Keys {
        shift: true,
        secondary: false,
    };
    const CMD: Keys = Keys {
        shift: false,
        secondary: true,
    };

    fn cell(row: usize, col: usize) -> Mark {
        let mut m = Mark::default();
        m.click_cell(row, col, PLAIN);
        m
    }

    fn rect(a: (usize, usize), b: (usize, usize)) -> Mark {
        let mut m = cell(a.0, a.1);
        m.click_cell(b.0, b.1, SHIFT);
        m
    }

    fn column(col: usize) -> Mark {
        let mut m = Mark::default();
        m.click_column(col, PLAIN);
        m
    }

    #[test]
    fn end_and_cell_count() {
        let mut m = Mark::default();
        assert_eq!((m.end(), m.cell_count(10)), (None, 0));
        m = rect((1, 1), (3, 2));
        assert_eq!(m.end(), Some((3, 2)));
        assert_eq!(m.cell_count(10), 6);
        m.all(2);
        assert_eq!(m.end(), None);
        assert_eq!(m.cell_count(4), 8);
        assert_eq!(cell(0, 0).cell_count(4), 1);
    }

    #[test]
    fn plain_click_marks_the_cell() {
        let m = cell(2, 3);
        assert_eq!(m.rows(10), vec![2]);
        assert_eq!(m.cols(), vec![3]);
        assert_eq!(m.anchor(), Some((2, 3)));
        assert!(m.contains(2, 3));
        assert!(!m.contains(2, 2));
        assert!(!m.is_column_mark());
    }

    #[test]
    fn shift_click_makes_a_rectangle_from_the_anchor() {
        let mut m = rect((1, 1), (3, 2));
        assert_eq!(m.rows(10), vec![1, 2, 3]);
        assert_eq!(m.cols(), vec![1, 2]);
        assert_eq!(m.anchor(), Some((1, 1)));
        m.click_cell(2, 4, SHIFT);
        assert_eq!(m.rows(10), vec![1, 2]);
        assert_eq!(m.cols(), vec![1, 2, 3, 4]);
        assert_eq!(m.anchor(), Some((1, 1)));
    }

    #[test]
    fn shift_click_upwards_and_leftwards() {
        let m = rect((4, 4), (2, 1));
        assert_eq!(m.rows(10), vec![2, 3, 4]);
        assert_eq!(m.cols(), vec![1, 2, 3, 4]);
        assert_eq!(m.anchor(), Some((4, 4)));
    }

    #[test]
    fn shift_click_without_anchor_is_plain() {
        let mut m = Mark::default();
        m.click_cell(2, 2, SHIFT);
        assert_eq!(m, cell(2, 2));
        // A column mark has no anchor either.
        let mut m = column(1);
        m.click_cell(3, 3, SHIFT);
        assert_eq!(m, cell(3, 3));
    }

    #[test]
    fn drag_follows_the_pointer() {
        let mut m = cell(1, 1);
        m.drag_to(3, 3);
        m.drag_to(0, 2);
        assert_eq!(m.rows(10), vec![0, 1]);
        assert_eq!(m.cols(), vec![1, 2]);
        assert_eq!(m.anchor(), Some((1, 1)));
    }

    #[test]
    fn drag_without_anchor_does_nothing() {
        let mut m = Mark::default();
        m.drag_to(3, 3);
        assert!(m.is_empty());
        let mut m = column(2);
        m.drag_to(3, 3);
        assert_eq!(m, column(2));
    }

    #[test]
    fn column_click_marks_all_rows() {
        let mut m = rect((1, 1), (2, 2));
        m.click_column(3, PLAIN);
        assert_eq!(m.cols(), vec![3]);
        assert_eq!(m.rows(4), vec![0, 1, 2, 3]);
        assert_eq!(m.anchor(), None);
        assert!(m.is_column_mark());
        assert!(m.contains(100, 3));
    }

    #[test]
    fn shift_click_column_adds_it() {
        let mut m = Mark::default();
        m.click_column(2, SHIFT);
        assert_eq!(m, column(2));

        m.click_column(0, SHIFT);
        assert_eq!(m.cols(), vec![0, 2]);
        assert_eq!(m.rows(3), vec![0, 1, 2]);
        assert!(m.is_column_mark());
    }

    #[test]
    fn shift_click_column_with_area_keeps_its_rows() {
        let mut m = rect((1, 1), (2, 2));
        m.click_column(5, SHIFT);
        assert_eq!(m.rows(10), vec![1, 2]);
        assert_eq!(m.cols(), vec![1, 2, 5]);
        assert!(!m.is_column_mark());
        assert_eq!(m.anchor(), Some((1, 1)));
    }

    #[test]
    fn command_click_column_toggles_it() {
        let mut m = column(1);
        m.click_column(3, CMD);
        assert_eq!(m.cols(), vec![1, 3]);
        m.click_column(1, CMD);
        assert_eq!(m.cols(), vec![3]);
        m.click_column(1, CMD);
        assert_eq!(m.cols(), vec![1, 3]);
        m.click_column(1, CMD);
        m.click_column(3, CMD);
        assert!(m.is_empty());
        assert_eq!(m, Mark::default());
    }

    #[test]
    fn command_click_column_on_empty_is_plain() {
        let mut m = Mark::default();
        m.click_column(4, CMD);
        assert_eq!(m, column(4));
    }

    #[test]
    fn command_click_row_on_a_rectangle() {
        let mut m = rect((1, 0), (3, 1));
        m.click_cell(2, 0, CMD);
        assert_eq!(m.rows(10), vec![1, 3]);
        assert_eq!(m.cols(), vec![0, 1]);
        assert_eq!(m.anchor(), Some((2, 0)));
        m.click_cell(2, 1, CMD);
        assert_eq!(m.rows(10), vec![1, 2, 3]);
        m.click_cell(7, 0, CMD);
        assert_eq!(m.rows(10), vec![1, 2, 3, 7]);
    }

    #[test]
    fn command_click_row_on_a_column_mark_excludes_it() {
        let mut m = column(1);
        m.click_cell(1, 1, CMD);
        assert_eq!(m.rows(4), vec![0, 2, 3]);
        assert!(m.is_column_mark());
        assert!(!m.row_marked(1));
        assert!(m.row_marked(2));
        assert!(m.row_marked(50));
        assert_eq!(m.describe(4), "1 column × 3 of 4 rows loaded");
        m.click_cell(1, 1, CMD);
        assert_eq!(m.rows(4), vec![0, 1, 2, 3]);
    }

    #[test]
    fn removing_the_last_row_clears() {
        let mut m = cell(2, 2);
        m.click_cell(2, 2, CMD);
        assert!(m.is_empty());
        assert_eq!(m.anchor(), Some((2, 2)));
        assert_eq!(m.rows(10), Vec::<usize>::new());
        assert_eq!(m.cols(), Vec::<usize>::new());
    }

    #[test]
    fn command_click_cell_on_empty_is_plain() {
        let mut m = Mark::default();
        m.click_cell(1, 2, CMD);
        assert_eq!(m, cell(1, 2));
    }

    #[test]
    fn row_click_marks_the_whole_row() {
        let mut m = Mark::default();
        m.click_row(3, PLAIN, 4);
        assert_eq!(m.rows(10), vec![3]);
        assert_eq!(m.cols(), vec![0, 1, 2, 3]);
        assert_eq!(m.anchor(), Some((3, 0)));
        // Replaces what was marked.
        m.click_row(5, PLAIN, 2);
        assert_eq!(m.rows(10), vec![5]);
        assert_eq!(m.cols(), vec![0, 1]);
    }

    #[test]
    fn row_click_with_shift_spans_rows() {
        let mut m = Mark::default();
        m.click_row(3, PLAIN, 3);
        m.click_row(1, SHIFT, 3);
        assert_eq!(m.rows(10), vec![1, 2, 3]);
        assert_eq!(m.cols(), vec![0, 1, 2]);
        assert_eq!(m.anchor(), Some((3, 0)));
        m.click_row(5, SHIFT, 3);
        assert_eq!(m.rows(10), vec![3, 4, 5]);
    }

    #[test]
    fn row_click_with_shift_without_anchor_is_plain() {
        let mut m = Mark::default();
        m.click_row(2, SHIFT, 2);
        let mut plain = Mark::default();
        plain.click_row(2, PLAIN, 2);
        assert_eq!(m, plain);
    }

    #[test]
    fn row_click_with_command_toggles() {
        let mut m = Mark::default();
        m.click_row(2, CMD, 3);
        assert_eq!(m.rows(10), vec![2]);
        m.click_row(4, CMD, 3);
        assert_eq!(m.rows(10), vec![2, 4]);
        // A ⇧-click after it starts at the toggled row.
        assert_eq!(m.anchor(), Some((4, 0)));
        m.click_row(2, CMD, 3);
        assert_eq!(m.rows(10), vec![4]);
        m.click_row(4, CMD, 3);
        assert!(m.is_empty());
    }

    #[test]
    fn row_click_without_columns_marks_nothing() {
        let mut m = Mark::default();
        m.click_row(2, PLAIN, 0);
        assert!(m.is_empty());
        let mut m = cell(1, 1);
        m.click_row(2, PLAIN, 0);
        assert!(m.is_empty());
    }

    #[test]
    fn extend_without_anchor_starts_at_from() {
        let mut m = Mark::default();
        m.extend((2, 2), 1, 0, 10, 5);
        assert_eq!(m.anchor(), Some((2, 2)));
        assert_eq!(m.rows(10), vec![2, 3]);
        assert_eq!(m.cols(), vec![2]);

        // A column mark has no anchor.
        let mut m = column(4);
        m.extend((1, 1), 0, -1, 10, 5);
        assert_eq!(m.anchor(), Some((1, 1)));
        assert_eq!(m.rows(10), vec![1]);
        assert_eq!(m.cols(), vec![0, 1]);
    }

    #[test]
    fn extend_repeats_from_the_end() {
        let mut m = cell(2, 2);
        m.extend((2, 2), 1, 1, 10, 10);
        m.extend((2, 2), 1, 1, 10, 10);
        assert_eq!(m.rows(10), vec![2, 3, 4]);
        assert_eq!(m.cols(), vec![2, 3, 4]);
        m.extend((2, 2), -3, -3, 10, 10);
        assert_eq!(m.rows(10), vec![1, 2]);
        assert_eq!(m.cols(), vec![1, 2]);
        assert_eq!(m.anchor(), Some((2, 2)));
    }

    #[test]
    fn extend_stays_within_the_result() {
        let mut m = cell(1, 1);
        m.extend((1, 1), -5, -5, 3, 3);
        assert_eq!(m.rows(3), vec![0, 1]);
        assert_eq!(m.cols(), vec![0, 1]);
        m.extend((1, 1), 9, 9, 3, 3);
        assert_eq!(m.rows(3), vec![1, 2]);
        assert_eq!(m.cols(), vec![1, 2]);
        // `from` outside the result is pulled in as well.
        let mut m = Mark::default();
        m.extend((8, 8), 0, 0, 3, 3);
        assert_eq!(m.rows(3), vec![2]);
        assert_eq!(m.cols(), vec![2]);
    }

    #[test]
    fn extend_in_an_empty_result_does_nothing() {
        let mut m = Mark::default();
        m.extend((0, 0), 1, 1, 0, 3);
        m.extend((0, 0), 1, 1, 3, 0);
        assert!(m.is_empty());
        assert_eq!(m.anchor(), None);
    }

    #[test]
    fn all_marks_everything() {
        let mut m = cell(1, 1);
        m.all(3);
        assert_eq!(m.cols(), vec![0, 1, 2]);
        assert_eq!(m.rows(2), vec![0, 1]);
        assert!(m.is_column_mark());
        assert_eq!(m.anchor(), None);
        m.all(0);
        assert!(m.is_empty());
    }

    #[test]
    fn clear_empties() {
        let mut m = rect((0, 0), (2, 2));
        m.clear();
        assert_eq!(m, Mark::default());
    }

    #[test]
    fn empty_mark_contains_nothing() {
        let m = Mark::default();
        assert!(m.is_empty());
        assert!(!m.is_column_mark());
        assert!(!m.contains(0, 0));
        assert!(!m.row_marked(0));
        assert!(!m.col_marked(0));
        assert_eq!(m.describe(5), "");
        assert_eq!(m.rows(5), Vec::<usize>::new());
    }

    #[test]
    fn rows_and_cols_are_in_grid_order() {
        let mut m = Mark::default();
        m.click_row(5, PLAIN, 1);
        m.click_row(1, CMD, 1);
        m.click_row(3, CMD, 1);
        assert_eq!(m.rows(10), vec![1, 3, 5]);
        // Rows past what is loaded are not handed out.
        assert_eq!(m.rows(4), vec![1, 3]);

        let mut m = column(4);
        m.click_column(0, CMD);
        m.click_column(2, CMD);
        assert_eq!(m.cols(), vec![0, 2, 4]);
        assert!(m.col_marked(2));
        assert!(!m.col_marked(1));
    }

    #[test]
    fn describe_counts() {
        assert_eq!(cell(0, 0).describe(10), "1 column × 1 row");
        assert_eq!(rect((0, 0), (1, 2)).describe(10), "3 columns × 2 rows");
        assert_eq!(
            column(1).describe(4000),
            "1 column × 4000 of 4000 rows loaded"
        );
        let mut m = column(1);
        m.click_column(2, SHIFT);
        assert_eq!(m.describe(1), "2 columns × 1 of 1 row loaded");
        let mut all = Mark::default();
        all.all(2);
        all.click_cell(0, 0, CMD);
        assert_eq!(all.describe(120), "2 columns × 119 of 120 rows loaded");
    }

    #[test]
    fn edges_of_a_single_cell() {
        let m = cell(2, 2);
        assert_eq!(m.edges(2, 2, 10), (true, true, true, true));
        assert_eq!(m.edges(2, 3, 10), (false, false, false, false));
        assert_eq!(m.edges(0, 0, 10), (false, false, false, false));
    }

    #[test]
    fn edges_of_a_rectangle() {
        let m = rect((0, 0), (1, 1));
        assert_eq!(m.edges(0, 0, 10), (true, false, false, true));
        assert_eq!(m.edges(0, 1, 10), (true, true, false, false));
        assert_eq!(m.edges(1, 0, 10), (false, false, true, true));
        assert_eq!(m.edges(1, 1, 10), (false, true, true, false));
    }

    #[test]
    fn edges_of_a_column_mark() {
        let m = column(1);
        assert_eq!(m.edges(0, 1, 10), (true, true, false, true));
        assert_eq!(m.edges(5, 1, 10), (false, true, false, true));
        assert_eq!(m.edges(5, 0, 10), (false, false, false, false));
        // The last loaded row closes the outline.
        assert_eq!(m.edges(9, 1, 10), (false, true, true, true));
    }

    #[test]
    fn edges_around_a_gap() {
        let mut m = column(0);
        m.click_cell(2, 0, CMD);
        assert_eq!(m.edges(1, 0, 10), (false, true, true, true));
        assert_eq!(m.edges(2, 0, 10), (false, false, false, false));
        assert_eq!(m.edges(3, 0, 10), (true, true, false, true));
    }

    #[test]
    fn tsv_in_given_order_with_headers() {
        let data = [["a", "b", "c"], ["d", "e", "f"], ["g", "h", "i"]];
        let names: Vec<String> = ["x", "y", "z"].map(String::from).to_vec();
        let get = |r: usize, c: usize| Some(data[r][c]);
        assert_eq!(tsv(&[0, 2], &[0, 2], get, Some(&names)), "x\tz\na\tc\ng\ti");
        assert_eq!(tsv(&[1], &[1], get, None), "e");
        assert_eq!(tsv(&[0, 1], &[0, 1], get, None), "a\tb\nd\te");
    }

    #[test]
    fn tsv_nulls_and_quoting() {
        let get = |r: usize, c: usize| match (r, c) {
            (0, 0) => None,
            (0, 1) => Some("a\tb"),
            (0, 2) => Some("say \"hi\""),
            _ => Some("line\nbreak"),
        };
        assert_eq!(
            tsv(&[0, 1], &[0, 1, 2], get, None),
            "\t\"a\tb\"\t\"say \"\"hi\"\"\"\n\"line\nbreak\"\t\"line\nbreak\"\t\"line\nbreak\""
        );
        assert_eq!(tsv_field(Some("plain")), "plain");
        assert_eq!(tsv_field(Some("a\rb")), "\"a\rb\"");
    }

    #[test]
    fn tsv_without_rows() {
        let names: Vec<String> = vec!["x".into(), "y".into()];
        let get = |_: usize, _: usize| Some("v");
        assert_eq!(tsv(&[], &[1, 0], get, Some(&names)), "y\tx");
        assert_eq!(tsv(&[], &[0], get, None), "");
    }
}
