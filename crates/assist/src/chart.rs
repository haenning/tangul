//! Whether a result can be shown as a chart, and the numbers to draw it. Strict about
//! the shape ([`RULES`]): label columns first, one number column last; with 3
//! columns the middle one's labels are the series, so the series come from the data
//! and needn't be known when the query is written. Not about the size: how many
//! rows or series are readable is the user's call (beyond `chart.max_series` the
//! rest is grey).
//!
//! Charts are a view of the result, never stored data.

use tangul_core::ResultSet;
pub use tangul_core::chart::{ChartStyle, ChartView};

use crate::analyze::{significant, statements};
use crate::lexer::{Token, TokenKind};

/// The rules a result has to follow to be a chart, as the user and AI tools read
/// them (the messages and the tools' descriptions all use this text).
pub const RULES: &str = "Charts take label columns first and one number column last:\n\
- 2 columns: x and value (day, revenue).\n\
- 3 columns: x, series and value (day, status, count(*)), one row per x and series \
(GROUP BY both).\n\
Several values go in rows, with a column naming them: LATERAL (VALUES ('revenue', \
revenue), ('cost', cost)) or UNION ALL. More labels are combined in SQL: \
to_char(day, 'YYYY/MM') or a || ' / ' || b. ORDER BY sets the order of x and of \
the series.";

/// What a stored chart from before [`RULES`] shows instead of the chart.
pub fn old_layout_message() -> String {
    format!(
        "tangul has changed how charts work. Let your agent recreate the query in \
         this format.\n\n{RULES}"
    )
}

/// One series: the values of one label in the series column, or the value
/// column itself when there is no series column.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    /// The label (or the value column's header).
    pub name: String,
    /// Per x; `None` for NULL or no row.
    pub values: Vec<Option<f64>>,
    /// Drawn in grey: beyond the colored ones (see [`ChartData::drawn`]).
    pub grey: bool,
    /// The grey series added up into one ("Other"): how many.
    pub other: Option<usize>,
}

impl Series {
    fn new(name: String, values: Vec<Option<f64>>) -> Self {
        Self {
            name,
            values,
            grey: false,
            other: None,
        }
    }
}

/// A chartable result.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartData {
    /// The x labels are dates or timestamps.
    pub dates: bool,
    /// Header of the x column.
    pub x_name: String,
    /// Header of the series column, when there is one.
    pub series_name: Option<String>,
    /// The x labels (NULL shows as "NULL").
    pub x: Vec<String>,
    pub series: Vec<Series>,
    /// The query groups rows (`GROUP BY`): a chart is the natural view, so the UI
    /// suggests it.
    pub grouped: bool,
    /// Dates the query puts out of ascending order (lines then go back in time).
    pub unsorted: bool,
    /// The result has more rows than the chart got (the row limit, or pages not
    /// loaded): the chart shows only the first ones.
    pub truncated: bool,
    /// Series left out of the drawing (grouped bars beyond the colored ones).
    pub left_out: usize,
}

/// The chart for a result, or the rule it breaks (shown instead of the chart).
/// `truncated`: the result has more rows than `result` holds.
///
/// Every column but the last is a label, the last the value (see [`RULES`]): 2
/// columns are x and value, 3 are x, series and value. x keeps the query's order
/// when it has an `ORDER BY`; without one, dates and numbers go ascending and
/// other labels by their total, largest first. Series follow an `ORDER BY` on
/// their column, else their total.
pub fn chart_data(sql: &str, result: &ResultSet, truncated: bool) -> Result<ChartData, String> {
    let columns = result.columns();
    let rows = result.row_count();
    match columns.len() {
        0 | 1 => {
            return Err("A chart needs a label column and a number column last".into());
        }
        2 | 3 => {}
        n => {
            return Err(format!(
                "A chart takes at most 3 columns (x, series, value); this has {n}. \
                 Combine labels in SQL, e.g. a || ' / ' || b"
            ));
        }
    }
    if rows < 2 {
        return Err("A chart needs at least 2 rows".into());
    }
    let last = columns.len() - 1;
    let value_name = columns[last].name.clone();
    let mut values = Vec::with_capacity(rows);
    for row in 0..rows {
        values.push(match result.cell(row, last) {
            None => None,
            Some(v) => Some(number(v).ok_or_else(|| {
                format!(
                    "The last column is the value and must be a number; {value_name} has “{}”",
                    short(v)
                )
            })?),
        });
    }
    if values.iter().all(Option::is_none) {
        return Err(format!("{value_name} is only NULL"));
    }
    let label = |row: usize, col: usize| result.cell(row, col).unwrap_or("NULL").to_string();
    let order = order_by(sql);

    let (x, series, series_name) = if last == 1 {
        // One series: every row is a point.
        let x: Vec<String> = (0..rows).map(|r| label(r, 0)).collect();
        (x, vec![Series::new(value_name, values)], None)
    } else {
        let mut x: Vec<String> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        let mut cells: std::collections::HashMap<(usize, usize), Option<f64>> =
            std::collections::HashMap::new();
        for (row, value) in values.into_iter().enumerate() {
            let (xl, sl) = (label(row, 0), label(row, 1));
            let xi = x.iter().position(|v| *v == xl).unwrap_or_else(|| {
                x.push(xl.clone());
                x.len() - 1
            });
            let si = names.iter().position(|v| *v == sl).unwrap_or_else(|| {
                names.push(sl.clone());
                names.len() - 1
            });
            if cells.insert((xi, si), value).is_some() {
                return Err(format!(
                    "{} {xl} with {} {sl} is in more than one row: group by {}, {}",
                    columns[0].name, columns[1].name, columns[0].name, columns[1].name
                ));
            }
        }
        let series = names
            .into_iter()
            .enumerate()
            .map(|(si, name)| {
                let values = (0..x.len())
                    .map(|xi| cells.get(&(xi, si)).copied().flatten())
                    .collect();
                Series::new(name, values)
            })
            .collect();
        (x, series, Some(columns[1].name.clone()))
    };

    let mut data = ChartData {
        dates: x.iter().all(|v| is_date(v) || v == "NULL") && x.iter().any(|v| is_date(v)),
        x_name: columns[0].name.clone(),
        series_name,
        x,
        series,
        grouped: groups(sql),
        unsorted: false,
        truncated,
        left_out: 0,
    };
    if order.is_none() {
        data.sort_x();
    }
    data.unsorted = data.dates && !data.x.iter().filter(|v| *v != "NULL").is_sorted();
    let series_order = match (&order, data.series_name.as_deref()) {
        (Some(items), Some(name)) => items.iter().find(|i| i.names(2, name)).map(|i| i.desc),
        _ => None,
    };
    data.sort_series(series_order);
    Ok(data)
}

/// Whether a stored chart's result has the old layout: a label column and
/// several number columns (e.g. `day, revenue, cost`). Read the new way, the
/// middle column would be series labels, so it isn't drawn.
pub fn old_layout(result: &ResultSet) -> bool {
    let columns = result.columns().len();
    columns >= 3
        && (1..columns).all(|col| {
            (0..result.row_count())
                .all(|row| result.cell(row, col).is_none_or(|v| number(v).is_some()))
        })
}

/// Compares labels: numbers as numbers, everything else as text (ISO dates sort
/// as text), case aside.
fn compare_labels(a: &str, b: &str) -> std::cmp::Ordering {
    match (number(a), number(b)) {
        (Some(a), Some(b)) => a.total_cmp(&b),
        _ => a
            .to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b)),
    }
}

impl ChartData {
    /// The sum of a series' values by size (its rank among the series).
    fn weight(&self, s: usize) -> f64 {
        self.series[s]
            .values
            .iter()
            .flatten()
            .map(|v| v.abs())
            .sum()
    }

    /// x without an `ORDER BY`: dates and numbers ascending, other labels by
    /// their total, largest first.
    fn sort_x(&mut self) {
        let natural = self
            .x
            .iter()
            .filter(|v| *v != "NULL")
            .all(|v| is_date(v) || number(v).is_some());
        let mut index: Vec<usize> = (0..self.x.len()).collect();
        if natural {
            index.sort_by(|&a, &b| compare_labels(&self.x[a], &self.x[b]));
        } else {
            let total = |xi: usize| -> f64 {
                self.series
                    .iter()
                    .filter_map(|s| s.values[xi])
                    .map(f64::abs)
                    .sum()
            };
            index.sort_by(|&a, &b| total(b).total_cmp(&total(a)));
        }
        self.x = index.iter().map(|&i| self.x[i].clone()).collect();
        for series in &mut self.series {
            series.values = index.iter().map(|&i| series.values[i]).collect();
        }
    }

    /// Series by their labels in the `ORDER BY`'s direction (`Some(desc)`), or
    /// by their total, largest first.
    fn sort_series(&mut self, by_label: Option<bool>) {
        let mut index: Vec<usize> = (0..self.series.len()).collect();
        match by_label {
            Some(desc) => index.sort_by(|&a, &b| {
                let order = compare_labels(&self.series[a].name, &self.series[b].name);
                if desc { order.reverse() } else { order }
            }),
            None => index.sort_by(|&a, &b| self.weight(b).total_cmp(&self.weight(a))),
        }
        let series = std::mem::take(&mut self.series);
        let mut slots: Vec<Option<Series>> = series.into_iter().map(Some).collect();
        self.series = index.iter().filter_map(|&i| slots[i].take()).collect();
    }

    /// Styles it can be drawn in: stacking needs several series.
    pub fn styles(&self) -> Vec<ChartStyle> {
        if self.series.len() < 2 {
            ChartStyle::SINGLE.to_vec()
        } else {
            ChartStyle::ALL.to_vec()
        }
    }

    pub fn fits(&self, view: ChartView) -> bool {
        self.styles().contains(&view.style)
    }

    /// Lines over time, bars for categories.
    pub fn default_view(&self) -> ChartView {
        ChartView::new(if self.dates {
            ChartStyle::Line
        } else {
            ChartStyle::Bars
        })
    }

    /// `view` if it fits this result, or the nearest view that does, otherwise the
    /// default. Views are picked before the result is known (boards, forms, AI
    /// tools), so a stacked style of a single series is the same style unstacked.
    pub fn view_or_default(&self, view: Option<ChartView>) -> ChartView {
        view.and_then(|v| self.nearest(v))
            .unwrap_or_else(|| self.default_view())
    }

    fn nearest(&self, view: ChartView) -> Option<ChartView> {
        if self.fits(view) {
            return Some(view);
        }
        let unstacked = ChartView {
            style: view.style.unstacked(),
            ..view
        };
        self.fits(unstacked).then_some(unstacked)
    }

    /// Name of a view for the status bar: `Stacked bars`, or `Line` for one series.
    pub fn view_label(&self, view: ChartView) -> String {
        if self.series.len() < 2 {
            view.style.single_label().to_string()
        } else {
            view.style.label().to_string()
        }
    }

    /// The data as drawn in `style` with at most `max` colored series (the largest
    /// by total): beyond them, lines and areas draw the rest in grey, stacked
    /// styles add them up into one grey "Other" on top, grouped bars leave them
    /// out ([`ChartData::left_out`]).
    pub fn drawn(&self, style: ChartStyle, max: usize) -> ChartData {
        let max = max.max(1);
        if self.series.len() <= max {
            return self.clone();
        }
        let mut ranked: Vec<usize> = (0..self.series.len()).collect();
        ranked.sort_by(|&a, &b| self.weight(b).total_cmp(&self.weight(a)));
        let colored = |s: usize| ranked[..max].contains(&s);
        let mut out = self.clone();
        match style {
            ChartStyle::Line | ChartStyle::Area => {
                for (s, series) in out.series.iter_mut().enumerate() {
                    series.grey = !colored(s);
                }
            }
            ChartStyle::StackedArea | ChartStyle::StackedBars => {
                let rest: Vec<&Series> = (0..self.series.len())
                    .filter(|&s| !colored(s))
                    .map(|s| &self.series[s])
                    .collect();
                let values = (0..self.x.len())
                    .map(|xi| {
                        let some: Vec<f64> = rest.iter().filter_map(|s| s.values[xi]).collect();
                        (!some.is_empty()).then(|| some.iter().sum())
                    })
                    .collect();
                out.series
                    .retain(|s| !rest.iter().any(|r| r.name == s.name));
                out.series.push(Series {
                    name: "Other".into(),
                    values,
                    grey: true,
                    other: Some(rest.len()),
                });
            }
            ChartStyle::Bars => {
                out.left_out = self.series.len() - max;
                let keep: Vec<String> = ranked[..max]
                    .iter()
                    .map(|&s| self.series[s].name.clone())
                    .collect();
                out.series.retain(|s| keep.contains(&s.name));
            }
        }
        out
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

    /// The series of a row as they are listed (tooltip), top to bottom. Stacked
    /// styles follow the stack as drawn: the top segment first, i.e. the positive
    /// values (NULL as 0) from the last series down, then the negative ones from
    /// 0 downwards. Other styles have no vertical order: series order.
    pub fn row_order(&self, style: ChartStyle, row: usize) -> Vec<usize> {
        let all = 0..self.series.len();
        if !style.is_stacked() {
            return all.collect();
        }
        let negative = |s: &usize| self.series[*s].values[row].is_some_and(|v| v < 0.0);
        let mut order: Vec<usize> = all.clone().rev().filter(|s| !negative(s)).collect();
        order.extend(all.filter(negative));
        order
    }

    /// The series as the legend lists them: stacked styles top of the stack first
    /// (the last series), like the tooltip; others in series order.
    pub fn legend_order(&self, style: ChartStyle) -> Vec<usize> {
        let all = 0..self.series.len();
        if style.is_stacked() {
            all.rev().collect()
        } else {
            all.collect()
        }
    }

    /// At most `max` of `order` to list at `row`, still in that order: the largest
    /// values by size (NULL smallest; ties keep the earlier), always including
    /// `keep` (the series under the pointer) in place of the smallest. Also how
    /// many are left out.
    pub fn pick_listed(
        &self,
        order: &[usize],
        row: usize,
        max: usize,
        keep: Option<usize>,
    ) -> (Vec<usize>, usize) {
        if order.len() <= max {
            return (order.to_vec(), 0);
        }
        let size = |s: usize| self.series[s].values[row].map_or(-1.0, f64::abs);
        let mut by_size: Vec<usize> = order.to_vec();
        by_size.sort_by(|&a, &b| size(b).total_cmp(&size(a)));
        by_size.truncate(max);
        if let Some(keep) = keep.filter(|k| order.contains(k) && !by_size.contains(k))
            && max > 0
        {
            by_size.pop();
            by_size.push(keep);
        }
        let listed: Vec<usize> = order
            .iter()
            .copied()
            .filter(|s| by_size.contains(s))
            .collect();
        let hidden = order.len() - listed.len();
        (listed, hidden)
    }

    /// The sum of a row's values (NULL left out), as stacked styles show it.
    pub fn row_total(&self, row: usize) -> f64 {
        self.series.iter().filter_map(|s| s.values[row]).sum()
    }

    /// The series whose stacked segment at `row` holds `value` (a value on the
    /// axis), or `None` above or below the stack. Where segments meet, the one
    /// with height wins; a zero-height segment only where no other is.
    pub fn stack_at(&self, row: usize, value: f64) -> Option<usize> {
        let stacks = self.stacks();
        let holds = |s: usize| {
            let (from, to) = stacks[s][row];
            value >= from.min(to) && value <= from.max(to)
        };
        let sized = (0..self.series.len()).find(|&s| {
            let (from, to) = stacks[s][row];
            from != to && holds(s)
        });
        sized.or_else(|| (0..self.series.len()).find(|&s| holds(s)))
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

/// One item of an `ORDER BY`: what it sorts by, as far as a chart needs it.
#[derive(Debug, PartialEq)]
struct OrderItem {
    /// A column by position (from 1).
    position: Option<usize>,
    /// A column by name (unquoted; the last part of `t.col`).
    name: Option<String>,
    desc: bool,
}

impl OrderItem {
    /// Whether it sorts by result column `position` (from 1) named `name`.
    fn names(&self, position: usize, name: &str) -> bool {
        self.position == Some(position)
            || self
                .name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
    }
}

/// The items of the last statement's outermost `ORDER BY`, or `None` without
/// one. Expressions count as items that name no column.
fn order_by(sql: &str) -> Option<Vec<OrderItem>> {
    let stmt = statements(sql)
        .into_iter()
        .map(|(s, e)| &sql[s..e])
        .rfind(|s| !s.trim().is_empty())?;
    let tokens = significant(stmt);
    let mut depth = 0usize;
    let mut start = None;
    for (i, t) in tokens.iter().enumerate() {
        if t.is_punct(stmt, '(') {
            depth += 1;
        } else if t.is_punct(stmt, ')') {
            depth = depth.saturating_sub(1);
        } else if depth == 0
            && t.is_word(stmt, "order")
            && tokens.get(i + 1).is_some_and(|n| n.is_word(stmt, "by"))
        {
            start = Some(i + 2);
        }
    }
    let start = start?;
    let mut items = Vec::new();
    let mut item: Vec<Token> = Vec::new();
    let mut depth = 0usize;
    let ends = ["limit", "offset", "fetch", "for"];
    let finish = |item: &mut Vec<Token>, items: &mut Vec<OrderItem>| {
        if item.is_empty() {
            return;
        }
        let desc = item.iter().any(|t| t.is_word(stmt, "desc"));
        // Up to ASC / DESC / NULLS: the expression.
        let expr: Vec<&Token> = item
            .iter()
            .take_while(|t| !["asc", "desc", "nulls"].iter().any(|w| t.is_word(stmt, w)))
            .collect();
        let position = match expr.as_slice() {
            [t] if t.kind == TokenKind::Number => t.text(stmt).parse().ok(),
            _ => None,
        };
        let plain = !expr.is_empty()
            && expr.iter().enumerate().all(|(i, t)| {
                if i % 2 == 0 {
                    t.is_ident()
                } else {
                    t.is_punct(stmt, '.')
                }
            });
        let name = plain
            .then(|| expr.last())
            .flatten()
            .map(|t| t.text(stmt).trim_matches(['"', '`']).to_string());
        items.push(OrderItem {
            position,
            name,
            desc,
        });
        item.clear();
    };
    for t in &tokens[start..] {
        if t.is_punct(stmt, '(') {
            depth += 1;
        } else if t.is_punct(stmt, ')') {
            depth = depth.saturating_sub(1);
        } else if depth == 0 && ends.iter().any(|w| t.is_word(stmt, w)) {
            break;
        } else if depth == 0 && t.is_punct(stmt, ',') {
            finish(&mut item, &mut items);
            continue;
        }
        item.push(*t);
    }
    finish(&mut item, &mut items);
    Some(items)
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

    /// A long result: x, series and value per row.
    fn long(rows: &[(&str, &str, Option<&str>)]) -> ResultSet {
        let rows: Vec<[Option<&str>; 3]> = rows
            .iter()
            .map(|&(x, s, v)| [Some(x), Some(s), v])
            .collect();
        let rows: Vec<&[Option<&str>]> = rows.iter().map(|r| r.as_slice()).collect();
        result(&["day", "status", "n"], &rows)
    }

    fn names(c: &ChartData) -> Vec<&str> {
        c.series.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn two_columns_are_x_and_value() {
        let r = result(
            &["status", "orders"],
            &[&[Some("paid"), Some("3")], &[None, Some("5")]],
        );
        let c = chart_data(
            "select status, count(*) orders from app.orders group by 1",
            &r,
            false,
        )
        .unwrap();
        assert!(!c.dates && c.grouped && !c.unsorted);
        // No ORDER BY: categories by their value, largest first.
        assert_eq!(c.x, ["NULL", "paid"]);
        assert_eq!(names(&c), ["orders"]);
        assert_eq!(c.series[0].values, [Some(5.0), Some(3.0)]);
        assert_eq!(c.series_name, None);
        // One series: no stacking.
        assert_eq!(c.styles(), ChartStyle::SINGLE);
        assert_eq!(c.default_view(), ChartView::new(ChartStyle::Bars));
        assert_eq!(c.view_label(c.default_view()), "Bars");
        assert_eq!(
            c.view_or_default(Some(ChartView::new(ChartStyle::StackedArea))),
            ChartView::new(ChartStyle::Area)
        );
        // With an ORDER BY: the query's order.
        let c = chart_data("select … order by orders", &r, false).unwrap();
        assert_eq!(c.x, ["paid", "NULL"]);
    }

    #[test]
    fn three_columns_are_x_series_and_value() {
        let r = long(&[
            ("2024-01-02", "paid", Some("4")),
            ("2024-01-01", "open", Some("1")),
            ("2024-01-01", "paid", Some("2")),
            ("2024-01-02", "lost", None),
        ]);
        let c = chart_data("select day, status, count(*) group by 1, 2", &r, false).unwrap();
        // Dates ascending, series by total (lost has none).
        assert!(c.dates && !c.unsorted);
        assert_eq!(c.x, ["2024-01-01", "2024-01-02"]);
        assert_eq!(names(&c), ["paid", "open", "lost"]);
        assert_eq!(c.series_name.as_deref(), Some("status"));
        // A missing combination is NULL, never 0.
        assert_eq!(c.series[0].values, [Some(2.0), Some(4.0)]);
        assert_eq!(c.series[1].values, [Some(1.0), None]);
        assert_eq!(c.series[2].values, [None, None]);
        assert_eq!(c.styles(), ChartStyle::ALL);
        assert_eq!(c.default_view(), ChartView::new(ChartStyle::Line));
        assert_eq!(
            c.view_label(ChartView::new(ChartStyle::StackedBars)),
            "Stacked bars"
        );
        // An ORDER BY on the series column, by name or position, in its direction.
        for sql in [
            "select … order by day, status desc",
            "select … order by 1, 2 desc nulls last",
            "select … order by t.\"status\" desc limit 10",
        ] {
            let c = chart_data(sql, &r, false).unwrap();
            assert_eq!(names(&c), ["paid", "open", "lost"], "{sql}");
            // x in the query's order: here not ascending.
            assert_eq!(c.x, ["2024-01-02", "2024-01-01"], "{sql}");
            assert!(c.unsorted, "{sql}");
        }
        let c = chart_data("select … order by status", &r, false).unwrap();
        assert_eq!(names(&c), ["lost", "open", "paid"]);
        // An ORDER BY on something else, or inside a window: series by total.
        for sql in [
            "select … order by day",
            "select …, rank() over (order by status) from t",
        ] {
            let c = chart_data(sql, &r, false).unwrap();
            assert_eq!(names(&c), ["paid", "open", "lost"], "{sql}");
        }
    }

    #[test]
    fn numbers_are_labels_and_sort_as_numbers() {
        // Years in the series position are labels: no type is needed.
        let r = long(&[
            ("10", "2024", Some("1")),
            ("9", "2023", Some("5")),
            ("10", "2023", Some("2")),
            ("9", "2024", Some("1")),
        ]);
        let c = chart_data("select … order by 2", &r, false).unwrap();
        assert_eq!(names(&c), ["2023", "2024"]);
        // Without ORDER BY: x as numbers (9 before 10), series by total.
        let c = chart_data("", &r, false).unwrap();
        assert_eq!(c.x, ["9", "10"]);
        assert!(!c.dates);
        assert_eq!(names(&c), ["2023", "2024"]);
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
            "The last column is the value and must be a number; b has “n/a”"
        );
        let one_row = result(&["a", "b"], &[&[Some("x"), Some("1")]]);
        assert!(chart_data("", &one_row, false).is_err());
        let nulls = result(&["a", "b"], &[&[Some("x"), None], &[Some("y"), None]]);
        assert_eq!(chart_data("", &nulls, false).unwrap_err(), "b is only NULL");
        let four = result(
            &["a", "b", "c", "d"],
            &[
                &[Some("x"), Some("y"), Some("z"), Some("1")],
                &[Some("x"), Some("y"), Some("w"), Some("2")],
            ],
        );
        assert!(
            chart_data("", &four, false)
                .unwrap_err()
                .contains("at most 3 columns")
        );
        // The same x and series twice: never added up.
        let twice = long(&[
            ("2024-01-01", "paid", Some("1")),
            ("2024-01-01", "paid", Some("2")),
        ]);
        assert_eq!(
            chart_data("", &twice, false).unwrap_err(),
            "day 2024-01-01 with status paid is in more than one row: group by day, status"
        );
    }

    #[test]
    fn the_old_layout_is_recognized() {
        let wide = result(
            &["day", "revenue", "cost"],
            &[
                &[Some("2024-01-01"), Some("5"), Some("7")],
                &[Some("2024-01-02"), None, Some("3")],
            ],
        );
        assert!(old_layout(&wide));
        let labels = long(&[("a", "x", Some("1")), ("b", "y", Some("2"))]);
        assert!(!old_layout(&labels));
        let two = result(
            &["a", "b"],
            &[&[Some("1"), Some("2")], &[Some("3"), Some("4")]],
        );
        assert!(!old_layout(&two));
        assert!(old_layout_message().starts_with("tangul has changed how charts work"));
        assert!(old_layout_message().ends_with(RULES));
    }

    #[test]
    fn beyond_the_cap_the_rest_is_grey() {
        // Series s0..s3 with totals 1, 2, 3, 4.
        let rows: Vec<(String, String, String)> = (0..4)
            .flat_map(|s| {
                ["a", "b"].map(|x| (x.to_string(), format!("s{s}"), format!("{}", s + 1)))
            })
            .collect();
        let rows: Vec<(&str, &str, Option<&str>)> = rows
            .iter()
            .map(|(x, s, v)| (x.as_str(), s.as_str(), Some(v.as_str())))
            .collect();
        let c = chart_data("select … order by status", &long(&rows), false).unwrap();
        assert_eq!(names(&c), ["s0", "s1", "s2", "s3"]);
        assert_eq!(c.drawn(ChartStyle::Line, 4), c, "all fit");
        // Lines: the largest two colored, the rest grey, in their order.
        let lines = c.drawn(ChartStyle::Line, 2);
        let grey: Vec<bool> = lines.series.iter().map(|s| s.grey).collect();
        assert_eq!(grey, [true, true, false, false]);
        // Stacked: the rest added up into "Other", on top.
        let stacked = c.drawn(ChartStyle::StackedBars, 2);
        assert_eq!(names(&stacked), ["s2", "s3", "Other"]);
        let other = &stacked.series[2];
        assert_eq!((other.grey, other.other), (true, Some(2)));
        assert_eq!(other.values, [Some(3.0), Some(3.0)]);
        // Grouped bars: left out.
        let bars = c.drawn(ChartStyle::Bars, 2);
        assert_eq!((names(&bars), bars.left_out), (vec!["s2", "s3"], 2));
    }

    #[test]
    fn size_is_no_reason_to_refuse() {
        // 10 000 rows: a chart.
        let labels: Vec<String> = (0..10_000).map(|r| format!("r{r}")).collect();
        let rows: Vec<[Option<&str>; 2]> = labels
            .iter()
            .map(|l| [Some(l.as_str()), Some("1")])
            .collect();
        let rows: Vec<&[Option<&str>]> = rows.iter().map(|r| r.as_slice()).collect();
        let c = chart_data("", &result(&["x", "n"], &rows), false).unwrap();
        assert_eq!((c.x.len(), c.series.len()), (10_000, 1));
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

    /// Series a, b, c over p, q, r: a 2, -1, 0; b 3, NULL, -2; c 1, 4, -3.
    fn three_series() -> ChartData {
        let r = long(&[
            ("p", "a", Some("2")),
            ("p", "b", Some("3")),
            ("p", "c", Some("1")),
            ("q", "a", Some("-1")),
            ("q", "b", None),
            ("q", "c", Some("4")),
            ("r", "a", Some("0")),
            ("r", "b", Some("-2")),
            ("r", "c", Some("-3")),
        ]);
        chart_data("select … order by 1, 2", &r, false).unwrap()
    }

    #[test]
    fn rows_are_listed_like_the_stack() {
        let c = three_series();
        // Stacked: the top segment first.
        for style in [ChartStyle::StackedBars, ChartStyle::StackedArea] {
            assert_eq!(c.row_order(style, 0), [2, 1, 0]);
            // Negative ones below 0, from 0 down; NULL sits with the positive ones.
            assert_eq!(c.row_order(style, 1), [2, 1, 0]);
            assert_eq!(c.row_order(style, 2), [0, 1, 2]);
            assert_eq!(c.legend_order(style), [2, 1, 0]);
        }
        // No vertical order: series order.
        for style in [ChartStyle::Line, ChartStyle::Area, ChartStyle::Bars] {
            assert_eq!(c.row_order(style, 1), [0, 1, 2]);
            assert_eq!(c.legend_order(style), [0, 1, 2]);
        }
        assert_eq!(c.row_total(0), 6.0);
        assert_eq!(c.row_total(1), 3.0, "NULL left out");
    }

    #[test]
    fn the_largest_are_listed_and_the_hovered_one_always() {
        let c = three_series();
        let order = [2, 1, 0];
        // All fit.
        assert_eq!(c.pick_listed(&order, 0, 3, None), (vec![2, 1, 0], 0));
        // The two largest, in the given order.
        assert_eq!(c.pick_listed(&order, 0, 2, None), (vec![1, 0], 1));
        // The hovered one replaces the smallest.
        assert_eq!(c.pick_listed(&order, 0, 2, Some(2)), (vec![2, 1], 1));
        // By size: -3 is larger than 0; NULL is the smallest.
        assert_eq!(c.pick_listed(&[0, 1, 2], 2, 2, None), (vec![1, 2], 1));
        assert_eq!(c.pick_listed(&[0, 1, 2], 1, 2, None), (vec![0, 2], 1));
        // A hovered one already listed changes nothing.
        assert_eq!(c.pick_listed(&order, 0, 2, Some(1)), (vec![1, 0], 1));
        assert_eq!(c.pick_listed(&order, 0, 0, Some(1)), (vec![], 3));
    }

    #[test]
    fn the_segment_under_a_value() {
        let c = three_series();
        // Row 0: a 0..2, b 2..5, c 5..6.
        assert_eq!(c.stack_at(0, 1.0), Some(0));
        assert_eq!(c.stack_at(0, 3.0), Some(1));
        assert_eq!(c.stack_at(0, 5.5), Some(2));
        assert_eq!(c.stack_at(0, 6.5), None, "above the stack");
        assert_eq!(c.stack_at(0, -0.5), None);
        // Row 1: a 0..-1 below, b NULL (0..0), c 0..4.
        assert_eq!(c.stack_at(1, -0.5), Some(0));
        assert_eq!(c.stack_at(1, 0.0), Some(0), "a segment with height first");
        assert_eq!(c.stack_at(1, 2.0), Some(2));
        // Row 2: a 0 (0..0), b 0..-2, c -2..-5.
        assert_eq!(c.stack_at(2, -4.0), Some(2));
        assert_eq!(c.stack_at(2, 0.5), None);
        // Only a zero-height segment there.
        let r = result(
            &["x", "a"],
            &[&[Some("p"), Some("0")], &[Some("q"), Some("1")]],
        );
        let zero = chart_data("", &r, false).unwrap();
        assert_eq!(zero.stack_at(0, 0.0), Some(0));
    }

    #[test]
    fn stacks_and_ranges() {
        let r = long(&[
            ("p", "a", Some("2")),
            ("p", "b", Some("3")),
            ("q", "a", Some("-1")),
            ("q", "b", None),
            ("r", "a", Some("4")),
            ("r", "b", Some("-2")),
        ]);
        let c = chart_data("select … order by 1, 2", &r, false).unwrap();
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
