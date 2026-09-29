//! Whether a result can be shown as a chart, and the numbers to draw it. Strict about
//! the shape, like a spreadsheet's default chart: column 0 is the x axis, every other
//! column is a numeric series named by its header, all in one chart. Not about the
//! size: how many rows or columns are readable is the user's call.
//!
//! Charts are a view of the result, never stored data.

use tangul_core::ResultSet;
pub use tangul_core::chart::{ChartStyle, ChartView};

use crate::analyze::{significant, statements};

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
    /// The result has more rows than the chart got (the row limit, or pages not
    /// loaded): the chart shows only the first ones.
    pub truncated: bool,
}

/// The chart for a result, or why there is none (shown as the button's tooltip).
/// `truncated`: the result has more rows than `result` holds.
pub fn chart_data(sql: &str, result: &ResultSet, truncated: bool) -> Result<ChartData, String> {
    let columns = result.columns();
    let rows = result.row_count();
    if columns.len() < 2 {
        return Err("A chart needs a label column followed by number columns".into());
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
    let unsorted = dates && !x.iter().flatten().is_sorted();

    Ok(ChartData {
        dates,
        x_name: columns[0].name.clone(),
        x: x.iter().map(|v| v.unwrap_or("NULL").to_string()).collect(),
        series,
        grouped: groups(sql),
        unsorted,
        truncated,
    })
}

impl ChartData {
    /// Styles for all value columns in one chart; none with a single column (that
    /// is [`Self::single_styles`]).
    pub fn multi_styles(&self) -> Vec<ChartStyle> {
        if self.series.len() < 2 {
            return Vec::new();
        }
        ChartStyle::ALL.to_vec()
    }

    /// Styles for one value column.
    pub fn single_styles(&self) -> Vec<ChartStyle> {
        ChartStyle::SINGLE.to_vec()
    }

    pub fn fits(&self, view: ChartView) -> bool {
        match view.series {
            None => self.multi_styles().contains(&view.style),
            Some(i) => i < self.series.len() && self.single_styles().contains(&view.style),
        }
    }

    /// Lines over time, bars for categories; all columns when there are several.
    pub fn default_view(&self) -> ChartView {
        let style = if self.dates {
            ChartStyle::Line
        } else {
            ChartStyle::Bars
        };
        let series = (self.series.len() < 2).then_some(0);
        ChartView { style, series }
    }

    /// `view` if it fits this result, otherwise the default.
    pub fn view_or_default(&self, view: Option<ChartView>) -> ChartView {
        view.filter(|&v| self.fits(v))
            .unwrap_or_else(|| self.default_view())
    }

    /// The view asked for as a style and a column name (`None`: all columns,
    /// or the only one); the default view when the result has no such column or
    /// the style doesn't fit it.
    pub fn view_for(&self, style: ChartStyle, column: Option<&str>) -> ChartView {
        let series = match column {
            Some(name) => {
                let name = name.trim();
                match self.series.iter().position(|s| s.name == name).or_else(|| {
                    self.series
                        .iter()
                        .position(|s| s.name.eq_ignore_ascii_case(name))
                }) {
                    Some(i) => Some(i),
                    None => return self.default_view(),
                }
            }
            None => (self.series.len() < 2).then_some(0),
        };
        self.view_or_default(Some(ChartView { style, series }))
    }

    /// Name of a view for the status bar: `Stacked bars`, or `Line · revenue`.
    pub fn view_label(&self, view: ChartView) -> String {
        match view.series.and_then(|i| self.series.get(i)) {
            None => view.style.label().to_string(),
            Some(_) if self.series.len() < 2 => view.style.single_label().to_string(),
            Some(s) => format!("{} · {}", view.style.single_label(), s.name),
        }
    }

    /// The chart data a view draws.
    pub fn for_view(&self, view: ChartView) -> ChartData {
        match view.series {
            Some(i) if self.series.len() > 1 => ChartData {
                series: self.series.get(i).into_iter().cloned().collect(),
                ..self.clone()
            },
            _ => self.clone(),
        }
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

/// Points of a line (x in pixels, ascending) reduced to at most two per pixel
/// column: its lowest and highest, in their order. The line looks the same, and
/// painting it costs the width, not the row count. Few points stay as they are.
pub fn min_max_per_pixel(points: &[(f32, f32)], width: f32) -> Vec<(f32, f32)> {
    if points.len() as f32 <= 2. * width.max(1.) {
        return points.to_vec();
    }
    let mut out = Vec::with_capacity(2 * width as usize + 2);
    for column in points.chunk_by(|a, b| a.0.floor() == b.0.floor()) {
        let (mut lo, mut hi) = (0, 0);
        for (i, p) in column.iter().enumerate() {
            if p.1 < column[lo].1 {
                lo = i;
            }
            if p.1 > column[hi].1 {
                hi = i;
            }
        }
        out.push(column[lo.min(hi)]);
        if lo != hi {
            out.push(column[lo.max(hi)]);
        }
    }
    out
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
    use tangul_core::{ColumnInfo, RowBatch};

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
            false,
        )
        .unwrap();
        assert!(!c.dates);
        let all = ChartView {
            style: ChartStyle::Bars,
            series: None,
        };
        assert_eq!(c.default_view(), all);
        assert_eq!(c.multi_styles(), ChartStyle::ALL);
        assert_eq!(c.view_label(all), "Bars side by side");
        let revenue = ChartView {
            style: ChartStyle::Line,
            series: Some(1),
        };
        assert!(c.fits(revenue));
        assert_eq!(c.view_label(revenue), "Line · revenue");
        assert_eq!(c.for_view(revenue).series[0].name, "revenue");
        // Stacking is for all columns only.
        assert!(!c.fits(ChartView {
            style: ChartStyle::StackedBars,
            series: Some(0)
        }));
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
        let c = chart_data("select * from app.revenue_by_day", &r, false).unwrap();
        assert!(c.dates);
        // One value column: only the single-column styles.
        assert!(c.multi_styles().is_empty());
        assert_eq!(c.single_styles(), ChartStyle::SINGLE);
        let line = ChartView {
            style: ChartStyle::Line,
            series: Some(0),
        };
        assert_eq!(c.default_view(), line);
        assert_eq!(
            c.view_or_default(Some(ChartView {
                style: ChartStyle::StackedBars,
                series: None
            })),
            line
        );
        assert_eq!(c.view_label(line), "Line");
        assert!(!c.grouped && c.unsorted);
        // Numbers in column 0 are labels.
        let r = result(
            &["year", "n"],
            &[&[Some("2023"), Some("1")], &[Some("2024"), Some("2")]],
        );
        assert!(!chart_data("", &r, false).unwrap().dates);
    }

    #[test]
    fn refuses_other_shapes() {
        let one_col = result(&["n"], &[&[Some("1")], &[Some("2")]]);
        assert!(chart_data("", &one_col, false).is_err());
        let text_value = result(
            &["a", "b"],
            &[&[Some("x"), Some("1")], &[Some("y"), Some("n/a")]],
        );
        assert_eq!(
            chart_data("", &text_value, false).unwrap_err(),
            "Only the first column may hold text; b has “n/a”"
        );
        let one_row = result(&["a", "b"], &[&[Some("x"), Some("1")]]);
        assert!(chart_data("", &one_row, false).is_err());
        let nulls = result(&["a", "b"], &[&[Some("x"), None], &[Some("y"), None]]);
        assert!(chart_data("", &nulls, false).is_err());
    }

    #[test]
    fn views_asked_for_by_column_name() {
        let r = result(
            &["day", "orders", "Revenue"],
            &[
                &[Some("2024-01-01"), Some("3"), Some("10")],
                &[Some("2024-01-02"), Some("4"), Some("12")],
            ],
        );
        let c = chart_data("", &r, false).unwrap();
        let view = |style, series| ChartView { style, series };
        // A column by name, in any case.
        assert_eq!(
            c.view_for(ChartStyle::Area, Some("revenue")),
            view(ChartStyle::Area, Some(1))
        );
        // All columns.
        assert_eq!(
            c.view_for(ChartStyle::StackedBars, None),
            view(ChartStyle::StackedBars, None)
        );
        // A column the result doesn't have, or a style that doesn't fit one
        // column: the default view.
        assert_eq!(c.view_for(ChartStyle::Bars, Some("nope")), c.default_view());
        assert_eq!(
            c.view_for(ChartStyle::StackedArea, Some("orders")),
            c.default_view()
        );
        // With one value column, no column means that one.
        let one = result(
            &["a", "n"],
            &[&[Some("x"), Some("1")], &[Some("y"), Some("2")]],
        );
        let c = chart_data("", &one, false).unwrap();
        assert_eq!(
            c.view_for(ChartStyle::Line, None),
            view(ChartStyle::Line, Some(0))
        );
    }

    #[test]
    fn size_is_no_reason_to_refuse() {
        // 10 000 rows and 12 number columns: a chart, with all styles.
        let cols: Vec<String> = (0..13).map(|c| format!("c{c}")).collect();
        let cols: Vec<&str> = cols.iter().map(String::as_str).collect();
        let labels: Vec<String> = (0..10_000).map(|r| format!("r{r}")).collect();
        let rows: Vec<Vec<Option<&str>>> = labels
            .iter()
            .map(|l| {
                std::iter::once(Some(l.as_str()))
                    .chain([Some("1"); 12])
                    .collect()
            })
            .collect();
        let rows: Vec<&[Option<&str>]> = rows.iter().map(Vec::as_slice).collect();
        let c = chart_data("", &result(&cols, &rows), false).unwrap();
        assert_eq!((c.x.len(), c.series.len()), (10_000, 12));
        assert_eq!(c.multi_styles(), ChartStyle::ALL);
        assert!(c.fits(ChartView {
            style: ChartStyle::Bars,
            series: Some(11)
        }));
        assert!(!c.truncated);
        // A result cut at the row limit says so.
        let r = result(
            &["a", "b"],
            &[&[Some("x"), Some("1")], &[Some("y"), Some("2")]],
        );
        assert!(chart_data("", &r, true).unwrap().truncated);
    }

    #[test]
    fn many_points_keep_each_pixel_s_min_and_max() {
        // 10 points per pixel column over 100 px.
        let points: Vec<(f32, f32)> = (0..1000)
            .map(|i| (i as f32 / 10., ((i * 7919) % 101) as f32))
            .collect();
        let reduced = min_max_per_pixel(&points, 100.);
        assert!(reduced.len() <= 200);
        for px in 0..100 {
            let column: Vec<f32> = points
                .iter()
                .filter(|p| p.0.floor() == px as f32)
                .map(|p| p.1)
                .collect();
            let kept: Vec<f32> = reduced
                .iter()
                .filter(|p| p.0.floor() == px as f32)
                .map(|p| p.1)
                .collect();
            let min = column.iter().copied().fold(f32::MAX, f32::min);
            let max = column.iter().copied().fold(f32::MIN, f32::max);
            assert!(kept.contains(&min) && kept.contains(&max), "column {px}");
        }
        // In x order, so the line is drawn left to right.
        assert!(reduced.is_sorted_by(|a, b| a.0 <= b.0));
        // Few points stay as they are.
        let few = [(0., 1.), (0.5, 3.), (1., 2.)];
        assert_eq!(min_max_per_pixel(&few, 100.), few);
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
        let c = chart_data("", &r, false).unwrap();
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
