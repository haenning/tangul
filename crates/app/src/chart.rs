//! One chart for a result: all series (value columns) over column 0, as lines,
//! areas, stacked areas, grouped bars or stacked bars (see `tangul_assist::chart`).
//! Drawn with gpui-kit's plot shapes; the layout math is done here.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme as _, StyledExt as _, h_flex,
    plot::{
        AXIS_GAP, AxisLabelSide, AxisText, Grid, IntoPlot, Plot, PlotAxis, StrokeStyle,
        shape::{Area, Bar, Line},
        tooltip::{CrossLine, Dot, PlotHover, Tooltip, TooltipState},
    },
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_assist::chart::{ChartData, ChartStyle, format_value, min_max_per_pixel, ticks};

/// Room left of the plot for the value labels, at the default label size.
const VALUE_GUTTER: f32 = 44.;
/// Axis label size in pixels unless the settings give one.
pub const LABEL_SIZE: f32 = 10.;
/// Space above the highest value.
const TOP_PAD: f32 = 8.;
/// Space right of the last x position.
const RIGHT_PAD: f32 = 8.;
/// Share of a row's width the bars of that row take.
const BAR_GROUP: f32 = 0.7;
/// Most series named in the legend; the rest behind "+N more".
pub const MAX_LISTED: usize = 8;
/// Height of one series' line in the tooltip.
const TIP_ROW: f32 = 18.;
/// Room in the tooltip for the title, "+N more", the total and the padding.
const TIP_FRAME: f32 = 64.;
/// Most columns the tooltip lays its series out in; what doesn't fit then is
/// the smallest values, in the pinned list (a click).
const TIP_COLUMNS: usize = 4;
/// Opacity of the other series while one is under the pointer.
const DIMMED: f32 = 0.35;
/// How close (px) the pointer has to be to a line to pick it.
const LINE_REACH: f32 = 10.;

/// A row held by a click: its values stay listed (scroll, copy) until released.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pin {
    pub row: usize,
    /// The series under the pointer when it was pinned.
    pub series: Option<usize>,
}

#[derive(IntoPlot)]
pub struct ResultChart {
    id: ElementId,
    data: Rc<ChartData>,
    style: ChartStyle,
    /// Per series.
    colors: Vec<Hsla>,
    /// Curves instead of straight segments between points (lines and areas).
    smooth: bool,
    /// Axis labels, in pixels.
    label_size: f32,
    /// The pinned row: no hover tooltip then, the row marked instead.
    pin: Option<Pin>,
    /// Where the chart was drawn, for the view to place the pinned list and map
    /// clicks to rows.
    bounds_out: Option<Rc<Cell<Bounds<Pixels>>>>,
    /// The series under the pointer and how far the hover has faded in (set by
    /// `hover`, before `paint`).
    hovered: Option<(usize, f32)>,
    /// This frame's bounds (set in `prepaint`, before `hover`).
    bounds: Bounds<Pixels>,
}

impl ResultChart {
    pub fn new(
        id: impl Into<ElementId>,
        data: Rc<ChartData>,
        style: ChartStyle,
        colors: Vec<Hsla>,
        smooth: bool,
        label_size: f32,
    ) -> Self {
        Self {
            id: id.into(),
            data,
            style,
            colors,
            smooth,
            label_size,
            pin: None,
            bounds_out: None,
            hovered: None,
            bounds: Bounds::default(),
        }
    }

    pub fn pinned(mut self, pin: Option<Pin>) -> Self {
        self.pin = pin;
        self
    }

    /// Records the bounds the chart is drawn in.
    pub fn bounds_out(mut self, cell: Rc<Cell<Bounds<Pixels>>>) -> Self {
        self.bounds_out = Some(cell);
        self
    }

    /// The row at `position` (relative to the chart's origin) and the series under
    /// it: for bars the bar or segment holding the pointer, for stacked areas the
    /// band, for lines the nearest within reach; `None` above a stack or in a gap.
    pub fn hit(
        &self,
        bounds: Bounds<Pixels>,
        position: Point<Pixels>,
    ) -> Option<(usize, Option<usize>)> {
        let l = self.layout(bounds);
        let (x, y) = (position.x.as_f32(), position.y.as_f32());
        if y > l.bottom {
            return None;
        }
        let row = l.row_at(x, self.data.x.len())?;
        Some((row, self.series_at(&l, row, x, y)))
    }

    /// Horizontal center of a row, relative to the chart's origin.
    pub fn row_x(&self, bounds: Bounds<Pixels>, row: usize) -> f32 {
        self.layout(bounds).x(row)
    }

    fn series_at(&self, l: &Layout, row: usize, x: f32, y: f32) -> Option<usize> {
        let series = self.data.series.len();
        let group = l.step * BAR_GROUP;
        let left = l.x(row) - group / 2.;
        let in_group = x >= left && x <= left + group;
        match self.style {
            ChartStyle::StackedBars if !in_group => None,
            ChartStyle::StackedBars | ChartStyle::StackedArea => {
                self.data.stack_at(row, l.value(y))
            }
            ChartStyle::Bars => {
                if !in_group || series == 0 {
                    return None;
                }
                let s = (((x - left) / (group / series as f32)) as usize).min(series - 1);
                let v = self.data.series[s].values[row]?;
                let (a, b) = (l.y(0.0), l.y(v));
                (y >= a.min(b) && y <= a.max(b)).then_some(s)
            }
            ChartStyle::Line | ChartStyle::Area => self
                .data
                .series
                .iter()
                .enumerate()
                .filter_map(|(s, series)| Some((s, (l.y(series.values[row]?) - y).abs())))
                .filter(|&(_, d)| d <= LINE_REACH)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(s, _)| s),
        }
    }

    /// The series shown in full: the pinned one, else the one under the pointer
    /// (with how far the hover has faded in).
    fn highlighted(&self) -> Option<(usize, f32)> {
        match self.pin {
            Some(pin) => pin.series.map(|s| (s, 1.)),
            None => self.hovered,
        }
    }

    fn layout(&self, bounds: Bounds<Pixels>) -> Layout {
        Layout::new(bounds, &self.data, self.style, self.label_size / LABEL_SIZE)
    }
}

/// Pixel geometry for one size of the chart.
struct Layout {
    left: f32,
    top: f32,
    /// The x axis (bottom of the plot area).
    bottom: f32,
    /// Width of one row's slot.
    step: f32,
    /// Value axis domain and its ticks.
    lo: f64,
    hi: f64,
    ticks: Vec<f64>,
}

impl Layout {
    /// `scale`: label size relative to the default; the room for labels grows with it.
    fn new(bounds: Bounds<Pixels>, data: &ChartData, style: ChartStyle, scale: f32) -> Self {
        let width = bounds.size.width.as_f32();
        let height = bounds.size.height.as_f32();
        let (min, max) = data.value_range(style);
        // About one value label per 40 px of height.
        let count = ((height / 40.) as usize).clamp(2, 8);
        let ticks = ticks(min, max, count);
        let rows = data.x.len().max(1) as f32;
        let left = VALUE_GUTTER * scale;
        Self {
            left,
            top: TOP_PAD,
            bottom: (height - AXIS_GAP * scale).max(TOP_PAD + 1.),
            step: (width - left - RIGHT_PAD).max(1.) / rows,
            lo: ticks.first().copied().unwrap_or(min),
            hi: ticks.last().copied().unwrap_or(max),
            ticks,
        }
    }

    /// Center of a row's slot.
    fn x(&self, row: usize) -> f32 {
        self.left + self.step * (row as f32 + 0.5)
    }

    /// A line's points, reduced to each pixel column's lowest and highest when
    /// there are many more rows than pixels (see `min_max_per_pixel`).
    fn line(&self, points: Vec<(f32, f32)>, bounds: &Bounds<Pixels>) -> Vec<(f32, f32)> {
        min_max_per_pixel(&points, bounds.size.width.as_f32() - self.left)
    }

    fn y(&self, v: f64) -> f32 {
        let span = (self.hi - self.lo).max(f64::EPSILON);
        self.bottom - ((v - self.lo) / span) as f32 * (self.bottom - self.top)
    }

    /// The value at a vertical position (the inverse of `y`).
    fn value(&self, y: f32) -> f64 {
        let height = (self.bottom - self.top).max(f32::EPSILON);
        self.lo + ((self.bottom - y) / height) as f64 * (self.hi - self.lo)
    }

    /// The row under a horizontal position.
    fn row_at(&self, x: f32, rows: usize) -> Option<usize> {
        if rows == 0 || x < self.left {
            return None;
        }
        Some((((x - self.left) / self.step) as usize).min(rows - 1))
    }
}

impl ResultChart {
    fn color(&self, series: usize) -> Hsla {
        self.colors[series % self.colors.len().max(1)]
    }

    /// A series' color as drawn: dimmed while another one is highlighted.
    fn shown(&self, series: usize, color: Hsla) -> Hsla {
        match self.highlighted() {
            Some((s, focus)) if s != series => color.opacity(1. - (1. - DIMMED) * focus),
            _ => color,
        }
    }

    /// A dot in each series' color at the hovered row's data point (lines and areas
    /// only; NULL values have none; stacked areas mark the top of their segment).
    fn hover_dots(&self, l: &Layout, row: usize, cx: &App) -> Vec<Dot> {
        let stacks = match self.style {
            ChartStyle::Line | ChartStyle::Area => None,
            ChartStyle::StackedArea => Some(self.data.stacks()),
            ChartStyle::Bars | ChartStyle::StackedBars => return Vec::new(),
        };
        let background = cx.theme().background;
        self.data
            .series
            .iter()
            .enumerate()
            .filter_map(|(s, series)| {
                let value = series.values[row]?;
                let value = stacks.as_ref().map_or(value, |stacks| stacks[s][row].1);
                Some(
                    Dot::new(point(px(l.x(row)), px(l.y(value))))
                        .size(px(7.))
                        .fill(self.color(s))
                        .stroke(background),
                )
            })
            .collect()
    }

    fn paint_axes(&self, l: &Layout, bounds: &Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        // Grid lines at the value ticks, across the plot area only.
        let plot = Bounds {
            origin: bounds.origin + point(px(l.left), px(0.)),
            size: size(bounds.size.width - px(l.left), bounds.size.height),
        };
        Grid::new()
            .y(l.ticks.iter().map(|&t| l.y(t)).collect())
            .stroke(theme.border)
            .dash_array(&[px(4.), px(2.)])
            .paint(&plot, window);

        // About one x label per 80 px, always including the first.
        let rows = self.data.x.len();
        let every = ((80. / l.step).ceil() as usize).max(1);
        let x_labels = (0..rows).step_by(every).map(|row| {
            AxisText::new(self.data.x[row].clone(), px(l.x(row)), muted)
                .align(TextAlign::Center)
                .font_size(px(self.label_size))
        });
        let y_labels = l.ticks.iter().map(|&t| {
            AxisText::new(format_value(t), px(l.y(t)), muted)
                .align(TextAlign::Right)
                .font_size(px(self.label_size))
        });
        PlotAxis::new()
            .stroke(theme.border)
            .x(px(l.y(0.0)))
            .x_label(x_labels)
            .y(px(l.left - 4.))
            .y_axis(false)
            .y_label_side(AxisLabelSide::Start)
            .y_label(y_labels)
            .paint(bounds, window, cx);
    }

    fn paint_bars(&self, l: &Layout, bounds: &Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let series = self.data.series.len();
        let group = l.step * BAR_GROUP;
        let stacks = self.style.is_stacked().then(|| self.data.stacks());
        for (s, values) in self.data.series.iter().enumerate() {
            // (left edge, base y, value y) per row.
            let bars: Vec<(f32, f32, f32)> = (0..self.data.x.len())
                .filter_map(|row| {
                    let slot = l.x(row) - group / 2.;
                    match &stacks {
                        Some(stacks) => {
                            let (from, to) = stacks[s][row];
                            Some((slot, l.y(from), l.y(to)))
                        }
                        None => {
                            let v = values.values[row]?;
                            let width = group / series as f32;
                            Some((slot + width * s as f32, l.y(0.0), l.y(v)))
                        }
                    }
                })
                .collect();
            let width = if stacks.is_some() {
                group
            } else {
                group / series as f32
            };
            let color = self.shown(s, self.color(s));
            Bar::new()
                .data(bars)
                .band_width((width - 1.).max(1.))
                .cross(|b: &(f32, f32, f32)| Some(b.0))
                .base(|b: &(f32, f32, f32)| b.1)
                .value(|b: &(f32, f32, f32)| Some(b.2))
                .fill(move |_, _, _| color)
                .paint(bounds, window, cx);
        }
    }

    fn paint_lines(&self, l: &Layout, bounds: &Bounds<Pixels>, window: &mut Window, cx: &App) {
        let background = cx.theme().background;
        // Straight segments are exact; curves can swing past the values between points.
        let curve = if self.smooth {
            StrokeStyle::Natural
        } else {
            StrokeStyle::Linear
        };
        let rows = self.data.x.len();
        let zero = l.y(0.0);
        match self.style {
            ChartStyle::StackedArea => {
                // Top layer first, each filled down to 0, so every band shows between
                // its own line and the one below.
                let stacks = self.data.stacks();
                for s in (0..stacks.len()).rev() {
                    let color = self.color(s);
                    let (fill, color) = (
                        self.shown(s, crate::theme::tint(color, background, 0.45)),
                        self.shown(s, color),
                    );
                    let points: Vec<(f32, f32)> = (0..rows)
                        .map(|row| (l.x(row), l.y(stacks[s][row].1)))
                        .collect();
                    let points = l.line(points, bounds);
                    Area::new()
                        .data(points)
                        .x(|p: &(f32, f32)| Some(p.0))
                        .y0(zero)
                        .y1(|p: &(f32, f32)| Some(p.1))
                        .fill(fill)
                        .stroke(color)
                        .stroke_style(curve)
                        .paint(bounds, window);
                }
            }
            ChartStyle::Line | ChartStyle::Area => {
                for (s, series) in self.data.series.iter().enumerate() {
                    let color = self.shown(s, self.color(s));
                    // NULL values are left out.
                    let points: Vec<(f32, f32)> = (0..rows)
                        .filter_map(|row| Some((l.x(row), l.y(series.values[row]?))))
                        .collect();
                    let points = l.line(points, bounds);
                    if self.style == ChartStyle::Area {
                        Area::new()
                            .data(points.clone())
                            .x(|p: &(f32, f32)| Some(p.0))
                            .y0(zero)
                            .y1(|p: &(f32, f32)| Some(p.1))
                            .fill(color.opacity(0.18))
                            .stroke(transparent_black())
                            .stroke_style(curve)
                            .paint(bounds, window);
                    }
                    Line::new()
                        .data(points)
                        .x(|p: &(f32, f32)| Some(p.0))
                        .y(|p: &(f32, f32)| Some(p.1))
                        .stroke(color)
                        .stroke_width(2.)
                        .stroke_style(curve)
                        .paint(bounds, window);
                }
            }
            ChartStyle::Bars | ChartStyle::StackedBars => {}
        }
    }
}

impl Plot for ResultChart {
    fn prepaint(&mut self, bounds: Bounds<Pixels>, _: &mut Window, _: &mut App) -> Vec<AnyElement> {
        self.bounds = bounds;
        if let Some(cell) = &self.bounds_out {
            cell.set(bounds);
        }
        Vec::new()
    }

    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let l = self.layout(bounds);
        self.paint_axes(&l, &bounds, window, cx);
        if let Some(pin) = self.pin.filter(|p| p.row < self.data.x.len()) {
            // The pinned row, marked like the hover's band.
            let width = if self.style.is_bars() { l.step } else { 1. };
            window.paint_quad(fill(
                Bounds {
                    origin: bounds.origin + point(px(l.x(pin.row) - width / 2.), px(l.top)),
                    size: size(px(width), px(l.bottom - l.top)),
                },
                cx.theme().border,
            ));
        }
        if self.style.is_bars() {
            self.paint_bars(&l, &bounds, window, cx);
        } else {
            self.paint_lines(&l, &bounds, window, cx);
        }
    }

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn tooltip_state(
        &self,
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _: &App,
    ) -> Option<TooltipState> {
        // A pinned row lists its values itself.
        if self.pin.is_some() {
            return None;
        }
        let l = self.layout(bounds);
        if position.y.as_f32() > l.bottom {
            return None;
        }
        let row = l.row_at(position.x.as_f32(), self.data.x.len())?;
        // The pointer, not the row's center, so `hover` and `tooltip` can find the
        // series under it (the cross line is placed by `tooltip`).
        Some(TooltipState::new(row, position, Vec::new()))
    }

    fn hover(&mut self, hover: Option<&PlotHover>, _: &mut Window, _: &mut App) {
        self.hovered = None;
        let Some(hover) = hover else {
            return;
        };
        let state = hover.state();
        let l = self.layout(self.bounds);
        let p = state.cross_line;
        self.hovered = self
            .series_at(&l, state.index, p.x.as_f32(), p.y.as_f32())
            .map(|s| (s, hover.focus()));
    }

    fn tooltip(
        &self,
        state: &TooltipState,
        cursor: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let l = self.layout(bounds);
        let row = state.index;
        // `span` sets where the line starts; `height` alone would start it at the plot's top edge.
        let mut cross =
            CrossLine::new(point(px(l.x(row)), px(l.top))).span(l.top, l.bottom - l.top);
        if self.style.is_bars() {
            cross = cross.band(px(l.step));
        }
        let p = state.cross_line;
        let under = self.series_at(&l, row, p.x.as_f32(), p.y.as_f32());
        // As many lines per column as the plot is high, up to a few columns.
        let per_column = (((bounds.size.height.as_f32() - TIP_FRAME) / TIP_ROW) as usize).max(3);
        let order = self.data.row_order(self.style, row);
        let (listed, hidden) = self
            .data
            .pick_listed(&order, row, per_column * TIP_COLUMNS, under);
        let columns = listed.len().div_ceil(per_column).max(1);
        let per_column = listed.len().div_ceil(columns).max(1);
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let content = v_flex()
            .id("chart-tip")
            .test_support()
            .text_xs()
            .gap_1()
            .child(div().font_semibold().child(self.data.x.get(row)?.clone()))
            .child(
                h_flex()
                    .items_start()
                    .gap_4()
                    .children(listed.chunks(per_column).map(|chunk| {
                        v_flex().children(
                            chunk
                                .iter()
                                .map(|&s| self.value_row(s, row, under == Some(s), cx)),
                        )
                    })),
            )
            .when(hidden > 0, |this| {
                this.child(
                    div()
                        .id("chart-tip-more")
                        .test_support()
                        .text_color(muted)
                        .child(format!("+{hidden} more, smallest · click to see all")),
                )
            })
            .when(self.style.is_stacked(), |this| {
                this.child(total_row(self.data.row_total(row), cx))
            });
        let tooltip = Tooltip::new(cursor, bounds.size)
            .gap(px(8.))
            .cross_line(cross)
            .dots(self.hover_dots(&l, row, cx))
            .child(content);
        Some(tooltip.into_any_element())
    }
}

impl ResultChart {
    /// One series' line in a list of a row's values: its color, name and value
    /// (NULL as "NULL"); the highlighted one stands out.
    pub fn value_row(&self, s: usize, row: usize, highlighted: bool, cx: &App) -> AnyElement {
        let series = &self.data.series[s];
        let value = series.values[row].map_or("NULL".into(), format_value);
        let theme = cx.theme();
        let id = if highlighted {
            ElementId::from(("chart-row-hovered", s))
        } else {
            ElementId::from(("chart-row", s))
        };
        h_flex()
            .id(id)
            .test_support()
            .h(px(TIP_ROW))
            .px_1()
            .gap_3()
            .justify_between()
            .rounded(theme.radius.half())
            .when(highlighted, |this| this.bg(theme.secondary).font_semibold())
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        div()
                            .size_2()
                            .flex_none()
                            .rounded(theme.radius.half())
                            .bg(self.color(s)),
                    )
                    .child(
                        div()
                            .when(!highlighted, |this| this.text_color(theme.muted_foreground))
                            .child(series.name.clone()),
                    ),
            )
            .child(value)
            .into_any_element()
    }

    pub fn pin(&self) -> Option<Pin> {
        self.pin
    }

    /// A copy that only maps positions to rows and series (`hit`).
    pub fn for_hits(&self) -> ResultChart {
        ResultChart::new(
            self.id.clone(),
            self.data.clone(),
            self.style,
            self.colors.clone(),
            self.smooth,
            self.label_size,
        )
    }

    /// Where `value` of `row` is drawn, in window coordinates (tests point there).
    #[cfg(test)]
    pub fn point_of(&self, bounds: Bounds<Pixels>, row: usize, value: f64) -> Point<Pixels> {
        let l = self.layout(bounds);
        bounds.origin + point(px(l.x(row)), px(l.y(value)))
    }

    /// The data, for the pinned list.
    pub fn data(&self) -> &ChartData {
        &self.data
    }

    pub fn style(&self) -> ChartStyle {
        self.style
    }
}

/// The "Total" line of stacked styles.
pub fn total_row(total: f64, cx: &App) -> impl IntoElement {
    h_flex()
        .h(px(TIP_ROW))
        .px_1()
        .gap_3()
        .justify_between()
        .border_t_1()
        .border_color(cx.theme().border)
        .child("Total")
        .child(format_value(total))
}
