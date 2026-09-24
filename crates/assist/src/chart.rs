//! Whether a result can be shown as a chart, and the numbers to draw it. Deliberately
//! strict, like a spreadsheet's default chart: column 0 is the x axis, every other
//! column is a numeric series named by its header, all in one chart.
//!
//! Charts are a view of the result, never stored data.

use rusdb_core::ResultSet;

use crate::analyze::{significant, statements};

/// Most series (value columns); more are hard to tell apart.
pub const MAX_SERIES: usize = 5;
/// Most rows for bar styles; beyond that the bars are unreadable.
pub const MAX_BARS: usize = 50;
/// Most points per line chart.
pub const MAX_POINTS: usize = 1000;

/// How the series are drawn, all in one chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartStyle {
    Line,
    /// Lines with the surface below them filled.
    Area,
    /// Areas on top of each other: the top line is the total.
    StackedArea,
    /// One bar per series, next to each other.
    Bars,
    /// Bars on top of each other: the top is the total.
    StackedBars,
}

impl ChartStyle {
    pub const ALL: [ChartStyle; 5] = [
        ChartStyle::Line,
        ChartStyle::Area,
        ChartStyle::StackedArea,
        ChartStyle::Bars,
        ChartStyle::StackedBars,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ChartStyle::Line => "Lines",
            ChartStyle::Area => "Areas",
            ChartStyle::StackedArea => "Stacked areas",
            ChartStyle::Bars => "Bars",
            ChartStyle::StackedBars => "Stacked bars",
        }
    }

    pub fn is_bars(self) -> bool {
        matches!(self, ChartStyle::Bars | ChartStyle::StackedBars)
    }

    pub fn is_stacked(self) -> bool {
        matches!(self, ChartStyle::StackedArea | ChartStyle::StackedBars)
    }
}

/// One value column.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    /// The column header.
    pub name: String,
    /// Per row; `None` for NULL.
    pub values: Vec<Option<f64>>,
}

/// A chartable result, in the query's row order.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartData {
    /// Column 0 holds dates or timestamps.
    pub dates: bool,
    /// Header of column 0.
    pub x_name: String,
    /// Column 0 per row (NULL shows as "NULL").
    pub x: Vec<String>,
    pub series: Vec<Series>,
    /// The query groups rows (`GROUP BY`): a chart is the natural view, so the UI
    /// suggests it.
    pub grouped: bool,
    /// Line chart whose dates are not ascending (the query decides the order).
    pub unsorted: bool,
}

/// The chart for a result, or why there is none (shown as the button's tooltip).
pub fn chart_data(sql: &str, result: &ResultSet) -> Result<ChartData, String> {
    let columns = result.columns();
    let rows = result.row_count();
    if columns.len() < 2 {
        return Err("A chart needs a label column followed by number columns".into());
    }
    if columns.len() - 1 > MAX_SERIES {
        return Err(format!(
            "A chart shows at most {MAX_SERIES} number columns (after the label column)"
        ));
    }
    if rows < 2 {
        return Err("A chart needs at least 2 rows".into());
    }

    let mut series = Vec::with_capacity(columns.len() - 1);
    for (col, info) in columns.iter().enumerate().skip(1) {
        let mut values = Vec::with_capacity(rows);
        for row in 0..rows {
            values.push(match result.cell(row, col) {
                None => None,
                Some(v) => Some(number(v).ok_or_else(|| {
                    format!(
                        "Only the first column may hold text; {} has “{}”",
                        info.name,
                        short(v)
                    )
                })?),
            });
        }
        if values.iter().all(Option::is_none) {
            return Err(format!("{} is only NULL", info.name));
        }
        series.push(Series {
            name: info.name.clone(),
            values,
        });
    }

    let x: Vec<Option<&str>> = (0..rows).map(|r| result.cell(r, 0)).collect();
    let dates = x.iter().flatten().all(|v| is_date(v)) && x.iter().any(Option::is_some);
    // Dates may be many points on a line; categories are read bar by bar.
    let limit = if dates { MAX_POINTS } else { MAX_BARS };
    if rows > limit {
        return Err(format!(
            "{rows} rows are too many for a chart (at most {limit}); group them with GROUP BY"
        ));
    }
    let unsorted = dates && !x.iter().flatten().is_sorted();

    Ok(ChartData {
        dates,
        x_name: columns[0].name.clone(),
        x: x.iter().map(|v| v.unwrap_or("NULL").to_string()).collect(),
        series,
        grouped: groups(sql),
        unsorted,
    })
}

impl ChartData {
    /// The styles that fit: bars only for up to [`MAX_BARS`] rows, stacked styles
    /// only for more than one series (with one they equal the plain ones).
    pub fn styles(&self) -> Vec<ChartStyle> {
        ChartStyle::ALL
            .into_iter()
            .filter(|s| !s.is_bars() || self.x.len() <= MAX_BARS)
            .filter(|s| !s.is_stacked() || self.series.len() > 1)
            .collect()
    }

    /// Lines over time, bars for categories.
    pub fn default_style(&self) -> ChartStyle {
        if self.dates {
            ChartStyle::Line
        } else {
            ChartStyle::Bars
        }
    }

    /// `style` if it fits, otherwise the default.
    pub fn style_or_default(&self, style: Option<ChartStyle>) -> ChartStyle {
        style
            .filter(|s| self.styles().contains(s))
            .unwrap_or_else(|| self.default_style())
    }

    /// Per series and row the `(from, to)` of its stacked segment. Positive values
    /// stack upwards from 0, negative ones downwards; NULL counts as 0.
    pub fn stacks(&self) -> Vec<Vec<(f64, f64)>> {
        let rows = self.x.len();
        let (mut up, mut down) = (vec![0.0; rows], vec![0.0; rows]);
        self.series
            .iter()
            .map(|series| {
                (0..rows)
                    .map(|row| {
                        let v = series.values[row].unwrap_or(0.0);
                        let base = if v < 0.0 { &mut down } else { &mut up };
                        let from = base[row];
                        base[row] += v;
                        (from, base[row])
                    })
                    .collect()
            })
            .collect()
    }

    /// Lowest and highest value the chart has to show, including 0 (bars and areas
    /// start there).
    pub fn value_range(&self, style: ChartStyle) -> (f64, f64) {
        let values: Vec<f64> = if style.is_stacked() {
            self.stacks()
                .iter()
                .flatten()
                .flat_map(|&(a, b)| [a, b])
                .collect()
        } else {
            self.series
                .iter()
                .flat_map(|s| s.values.iter().flatten().copied())
                .collect()
        };
        values
            .into_iter()
            .fold((0.0, 0.0), |(lo, hi), v| (f64::min(lo, v), f64::max(hi, v)))
    }
}

/// About `count` round axis values covering `min..=max` (steps of 1, 2 or 5 times a
/// power of ten), from at or below `min` to at or above `max`.
pub fn ticks(min: f64, max: f64, count: usize) -> Vec<f64> {
    let (min, max) = if max > min {
        (min, max)
    } else {
        (min - 1.0, min + 1.0)
    };
    let raw = (max - min) / count.max(1) as f64;
    let magnitude = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|m| m * magnitude)
        .find(|&s| s >= raw)
        .unwrap_or(10.0 * magnitude);
    let first = (min / step).floor() as i64;
    let last = (max / step).ceil() as i64;
    (first..=last).map(|i| i as f64 * step).collect()
}

/// Short axis / tooltip text: `1.5k`, `12M`, `0.25`, `-3`.
pub fn format_value(v: f64) -> String {
    let (div, suffix) = match v.abs() {
        a if a >= 1e9 => (1e9, "G"),
        a if a >= 1e6 => (1e6, "M"),
        a if a >= 1e4 => (1e3, "k"),
        _ => (1.0, ""),
    };
    let scaled = v / div;
    let text = format!("{:.2}", scaled);
    let text = text.trim_end_matches('0').trim_end_matches('.');
    let text = if text == "-0" { "0" } else { text };
    format!("{text}{suffix}")
}

/// A finite number as the server writes it (`12`, `-3.5`, `1e3`); not `NaN`, `inf`
/// or anything with letters or spaces.
fn number(v: &str) -> Option<f64> {
    let plain = !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'));
    v.parse::<f64>().ok().filter(|n| plain && n.is_finite())
}

/// `YYYY-MM-DD`, optionally followed by a time (as both servers print dates,
/// timestamps and `date_trunc` results).
fn is_date(v: &str) -> bool {
    let b = v.as_bytes();
    let digits =
        |r: std::ops::Range<usize>| b.get(r).is_some_and(|s| s.iter().all(u8::is_ascii_digit));
    digits(0..4)
        && b.get(4) == Some(&b'-')
        && digits(5..7)
        && b.get(7) == Some(&b'-')
        && digits(8..10)
        && matches!(b.get(10), None | Some(b' ' | b'T'))
}

/// Whether the last statement (the one the grid shows) has a `GROUP BY`.
fn groups(sql: &str) -> bool {
    let Some(stmt) = statements(sql)
        .into_iter()
        .map(|(s, e)| &sql[s..e])
        .rfind(|s| !s.trim().is_empty())
    else {
        return false;
    };
    let tokens = significant(stmt);
    tokens
        .windows(2)
        .any(|w| w[0].is_word(stmt, "group") && w[1].is_word(stmt, "by"))
}

fn short(v: &str) -> String {
    const MAX: usize = 30;
    match v.char_indices().nth(MAX) {
        Some((i, _)) => format!("{}…", &v[..i]),
        None => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusdb_core::{ColumnInfo, RowBatch};

    fn result(cols: &[&str], rows: &[&[Option<&str>]]) -> ResultSet {
        let mut r = ResultSet::new(
            cols.iter()
                .map(|c| ColumnInfo {
                    name: c.to_string(),
                })
                .collect(),
        );
        let mut batch = RowBatch::with_capacity(cols.len() * rows.len());
        for row in rows {
            for cell in *row {
                batch.push_cell(*cell);
            }
            batch.finish_row();
        }
        r.append(batch);
        r
    }

    #[test]
    fn bars_for_categories() {
        let r = result(
            &["status", "orders", "revenue"],
            &[
                &[Some("paid"), Some("3"), Some("120.50")],
                &[None, Some("1"), None],
            ],
        );
        let c = chart_data(
            "select status, count(*) orders, sum(total) revenue from app.orders group by 1",
            &r,
        )
        .unwrap();
        assert!(!c.dates);
        assert_eq!(c.default_style(), ChartStyle::Bars);
        assert_eq!(c.styles(), ChartStyle::ALL);
        assert!(c.grouped && !c.unsorted);
        assert_eq!(c.x, ["paid", "NULL"]);
        assert_eq!(c.series[1].name, "revenue");
        assert_eq!(c.series[1].values, [Some(120.5), None]);
    }

    #[test]
    fn lines_for_dates() {
        let r = result(
            &["day", "n"],
            &[
                &[Some("2024-01-02 00:00:00+00"), Some("5")],
                &[Some("2024-01-01 00:00:00+00"), Some("7")],
            ],
        );
        let c = chart_data("select * from app.revenue_by_day", &r).unwrap();
        assert!(c.dates);
        assert_eq!(c.default_style(), ChartStyle::Line);
        // One series: stacking would look the same as the plain styles.
        assert_eq!(
            c.styles(),
            [ChartStyle::Line, ChartStyle::Area, ChartStyle::Bars]
        );
        assert_eq!(
            c.style_or_default(Some(ChartStyle::StackedBars)),
            ChartStyle::Line
        );
        assert!(!c.grouped && c.unsorted);
        // Numbers in column 0 are labels.
        let r = result(
            &["year", "n"],
            &[&[Some("2023"), Some("1")], &[Some("2024"), Some("2")]],
        );
        assert!(!chart_data("", &r).unwrap().dates);
    }

    #[test]
    fn refuses_other_shapes() {
        let one_col = result(&["n"], &[&[Some("1")], &[Some("2")]]);
        assert!(chart_data("", &one_col).is_err());
        let text_value = result(
            &["a", "b"],
            &[&[Some("x"), Some("1")], &[Some("y"), Some("n/a")]],
        );
        assert_eq!(
            chart_data("", &text_value).unwrap_err(),
            "Only the first column may hold text; b has “n/a”"
        );
        let one_row = result(&["a", "b"], &[&[Some("x"), Some("1")]]);
        assert!(chart_data("", &one_row).is_err());
        let nulls = result(&["a", "b"], &[&[Some("x"), None], &[Some("y"), None]]);
        assert!(chart_data("", &nulls).is_err());
        let many: Vec<[Option<&str>; 2]> = (0..=MAX_BARS).map(|_| [Some("x"), Some("1")]).collect();
        let many: Vec<&[Option<&str>]> = many.iter().map(|r| &r[..]).collect();
        assert!(
            chart_data("", &result(&["a", "b"], &many))
                .unwrap_err()
                .contains("GROUP BY")
        );
    }

    #[test]
    fn stacks_and_ranges() {
        let r = result(
            &["x", "a", "b"],
            &[
                &[Some("p"), Some("2"), Some("3")],
                &[Some("q"), Some("-1"), None],
                &[Some("r"), Some("4"), Some("-2")],
            ],
        );
        let c = chart_data("", &r).unwrap();
        assert_eq!(
            c.stacks(),
            [
                vec![(0.0, 2.0), (0.0, -1.0), (0.0, 4.0)],
                vec![(2.0, 5.0), (0.0, 0.0), (0.0, -2.0)],
            ]
        );
        assert_eq!(c.value_range(ChartStyle::Bars), (-2.0, 4.0));
        assert_eq!(c.value_range(ChartStyle::StackedBars), (-2.0, 5.0));
    }

    #[test]
    fn axis_ticks_and_labels() {
        assert_eq!(ticks(0.0, 37.0, 4), [0.0, 10.0, 20.0, 30.0, 40.0]);
        assert_eq!(ticks(-2.0, 5.0, 4), [-2.0, 0.0, 2.0, 4.0, 6.0]);
        assert_eq!(ticks(0.0, 0.0, 4).len(), 5);
        assert_eq!(format_value(1500.0), "1500");
        assert_eq!(format_value(25_000.0), "25k");
        assert_eq!(format_value(1_234_567.0), "1.23M");
        assert_eq!(format_value(0.25), "0.25");
        assert_eq!(format_value(-3.0), "-3");
    }

    #[test]
    fn values() {
        assert_eq!(number("-1.5e2"), Some(-150.0));
        assert_eq!(number("NaN"), None);
        assert_eq!(number("inf"), None);
        assert_eq!(number(" 1"), None);
        assert!(is_date("2024-01-31") && is_date("2024-01-31T10:00:00"));
        assert!(!is_date("2024-1-31") && !is_date("20240131") && !is_date("2024-01-31x"));
        assert!(groups("select a, count(*) from t GROUP  BY a"));
        assert!(!groups("select 'group by' from t"));
    }
}
