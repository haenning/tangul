//! Compact result storage: all cell text lives in one `String` arena with `u32` end
//! offsets and a null bitmap. Avoids one heap allocation per cell, which matters for
//! large results.

#[derive(Debug, Clone)]
pub struct ColumnInfo {
    pub name: String,
}

/// A chunk of rows as produced by a driver. Same layout as [`ResultSet`], so appending
/// is a couple of `extend`s.
#[derive(Debug, Default, Clone)]
pub struct RowBatch {
    text: String,
    ends: Vec<u32>,
    nulls: Vec<bool>,
    rows: usize,
}

impl RowBatch {
    pub fn with_capacity(cells: usize) -> Self {
        Self {
            text: String::with_capacity(cells * 8),
            ends: Vec::with_capacity(cells),
            nulls: Vec::with_capacity(cells),
            rows: 0,
        }
    }

    /// Push one cell of the current row. Call [`Self::finish_row`] after the last column.
    pub fn push_cell(&mut self, value: Option<&str>) {
        match value {
            Some(v) => {
                self.text.push_str(v);
                self.nulls.push(false);
            }
            None => self.nulls.push(true),
        }
        self.ends.push(self.text.len() as u32);
    }

    pub fn finish_row(&mut self) {
        self.rows += 1;
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }
}

#[derive(Debug, Default)]
pub struct ResultSet {
    columns: Vec<ColumnInfo>,
    text: String,
    /// End offset of each cell (row-major). Start is the previous end.
    ends: Vec<u32>,
    // Kept as Vec<bool> for simplicity; a bitmap is a drop-in optimization.
    nulls: Vec<bool>,
    rows: usize,
}

impl ResultSet {
    pub fn new(columns: Vec<ColumnInfo>) -> Self {
        Self {
            columns,
            ..Default::default()
        }
    }

    pub fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    pub fn row_count(&self) -> usize {
        self.rows
    }

    pub fn append(&mut self, batch: RowBatch) {
        let base = self.text.len() as u32;
        self.text.push_str(&batch.text);
        self.ends.extend(batch.ends.iter().map(|e| e + base));
        self.nulls.extend_from_slice(&batch.nulls);
        self.rows += batch.rows;
    }

    /// Its columns and all its rows as one batch, e.g. to replay a finished result
    /// as a driver's events.
    pub fn into_batch(self) -> (Vec<ColumnInfo>, RowBatch) {
        let batch = RowBatch {
            text: self.text,
            ends: self.ends,
            nulls: self.nulls,
            rows: self.rows,
        };
        (self.columns, batch)
    }

    /// Returns `None` for SQL NULL.
    pub fn cell(&self, row: usize, col: usize) -> Option<&str> {
        let ix = row * self.columns.len() + col;
        if self.nulls[ix] {
            return None;
        }
        let start = if ix == 0 {
            0
        } else {
            self.ends[ix - 1] as usize
        };
        Some(&self.text[start..self.ends[ix] as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_read() {
        let mut rs = ResultSet::new(vec![
            ColumnInfo { name: "a".into() },
            ColumnInfo { name: "b".into() },
        ]);
        let mut b = RowBatch::default();
        b.push_cell(Some("1"));
        b.push_cell(None);
        b.finish_row();
        rs.append(b);
        let mut b = RowBatch::default();
        b.push_cell(Some(""));
        b.push_cell(Some("héllo"));
        b.finish_row();
        rs.append(b);

        assert_eq!(rs.row_count(), 2);
        assert_eq!(rs.cell(0, 0), Some("1"));
        assert_eq!(rs.cell(0, 1), None);
        assert_eq!(rs.cell(1, 0), Some(""));
        assert_eq!(rs.cell(1, 1), Some("héllo"));

        // Replayed as one batch, it reads the same.
        let (columns, batch) = rs.into_batch();
        assert_eq!(batch.rows(), 2);
        let mut again = ResultSet::new(columns);
        again.append(batch);
        assert_eq!(again.row_count(), 2);
        assert_eq!(again.cell(0, 1), None);
        assert_eq!(again.cell(1, 1), Some("héllo"));
    }
}
