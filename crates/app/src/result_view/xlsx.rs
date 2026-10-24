//! Rows of a result as an Excel workbook ("Save as" Excel): one sheet, the column
//! names in bold, numbers of numeric columns as numbers, NULL an empty cell.

use rust_xlsxwriter::{Format, Workbook};

/// What the workbook holds, owned: it is made off the UI thread.
#[derive(Debug, Clone, PartialEq)]
pub struct Sheet {
    pub names: Vec<String>,
    /// Per column: its values are numbers.
    pub numeric: Vec<bool>,
    pub rows: Vec<Vec<Option<String>>>,
}

/// More significant digits than Excel keeps (an `f64`): such numbers stay text.
const MAX_DIGITS: usize = 15;

/// The `.xlsx` file's bytes; an error says why Excel can't hold the rows (too many,
/// a value too long).
pub fn xlsx(sheet: &Sheet) -> Result<Vec<u8>, String> {
    let mut book = Workbook::new();
    let ws = book.add_worksheet();
    let bold = Format::new().set_bold();
    let err = |e: rust_xlsxwriter::XlsxError| format!("Excel can't hold this: {e}");
    for (c, name) in sheet.names.iter().enumerate() {
        ws.write_string_with_format(0, col(c)?, name, &bold)
            .map_err(err)?;
    }
    for (r, row) in sheet.rows.iter().enumerate() {
        let r = u32::try_from(r + 1).map_err(|_| "Excel can't hold this many rows".to_string())?;
        for (c, value) in row.iter().enumerate() {
            let Some(value) = value else { continue };
            match number(value).filter(|_| sheet.numeric.get(c).copied().unwrap_or(false)) {
                Some(n) => ws.write_number(r, col(c)?, n),
                None => ws.write_string(r, col(c)?, value),
            }
            .map_err(err)?;
        }
    }
    ws.set_freeze_panes(1, 0).map_err(err)?;
    ws.autofit();
    book.save_to_buffer().map_err(err)
}

fn col(c: usize) -> Result<u16, String> {
    u16::try_from(c).map_err(|_| "Excel can't hold this many columns".to_string())
}

/// `value` as a number Excel keeps exactly.
fn number(value: &str) -> Option<f64> {
    let digits = value.chars().filter(char::is_ascii_digit).count();
    let n: f64 = value.trim().parse().ok()?;
    (n.is_finite() && digits <= MAX_DIGITS).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_only_where_exact() {
        assert_eq!(number("42"), Some(42.));
        assert_eq!(number("-1.5"), Some(-1.5));
        assert_eq!(number("1234567890123456"), None);
        assert_eq!(number("NaN"), None);
        assert_eq!(number("12 apples"), None);
    }

    #[test]
    fn writes_a_workbook() {
        let sheet = Sheet {
            names: vec!["id".into(), "note".into()],
            numeric: vec![true, false],
            rows: vec![
                vec![Some("1".into()), Some("a".into())],
                vec![Some("2".into()), None],
            ],
        };
        let bytes = xlsx(&sheet).unwrap();
        // A zip archive.
        assert_eq!(&bytes[..2], b"PK");
        let long = Sheet {
            rows: vec![vec![None, Some("x".repeat(40_000))]],
            ..sheet
        };
        assert!(
            xlsx(&long)
                .unwrap_err()
                .starts_with("Excel can't hold this")
        );
    }
}
