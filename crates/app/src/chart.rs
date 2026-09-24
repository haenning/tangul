//! One chart for a result: all series (value columns) over column 0, as lines,
//! areas, stacked areas, grouped bars or stacked bars (see `rusdb_assist::chart`).
//! Drawn with gpui-kit's plot shapes; the layout math is done here.

use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme as _,
    plot::{
        AXIS_GAP, AxisLabelSide, AxisText, Grid, IntoPlot, Plot, PlotAxis, StrokeStyle,
        shape::{Area, Bar, Line},
        tooltip::{CrossLine, Tooltip, TooltipState},
    },
};
use gpui_kit::*;
use rusdb_assist::chart::{ChartData, ChartStyle, format_value, ticks};

/// Room left of the plot for the value labels.
const VALUE_GUTTER: f32 = 44.;
/// Space above the highest value.
const TOP_PAD: f32 = 8.;
/// Space right of the last x position.
const RIGHT_PAD: f32 = 8.;
/// Share of a row's width the bars of that row take.
const BAR_GROUP: f32 = 0.7;

#[derive(IntoPlot)]
pub struct ResultChart {
    id: ElementId,
    data: Rc<ChartData>,
    style: ChartStyle,
    /// Per series.
    colors: Vec<Hsla>,
    /// Curves instead of straight segments between points (lines and areas).
    smooth: bool,
}

impl ResultChart {
    pub fn new(
        id: impl Into<ElementId>,
        data: Rc<ChartData>,
        style: ChartStyle,
        colors: Vec<Hsla>,
        smooth: bool,
    ) -> Self {
        Self {
            id: id.into(),
            data,
            style,
            colors,
            smooth,
        }
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
    fn new(bounds: Bounds<Pixels>, data: &ChartData, style: ChartStyle) -> Self {
        let width = bounds.size.width.as_f32();
        let height = bounds.size.height.as_f32();
        let (min, max) = data.value_range(style);
        // About one value label per 40 px of height.
        let count = ((height / 40.) as usize).clamp(2, 8);
        let ticks = ticks(min, max, count);
        let rows = data.x.len().max(1) as f32;
        let left = VALUE_GUTTER;
        Self {
            left,
            top: TOP_PAD,
            bottom: (height - AXIS_GAP).max(TOP_PAD + 1.),
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

    fn y(&self, v: f64) -> f32 {
        let span = (self.hi - self.lo).max(f64::EPSILON);
        self.bottom - ((v - self.lo) / span) as f32 * (self.bottom - self.top)
    }

    /// The row under a horizontal position.
    fn row_at(&self, x: f32, rows: usize) -> Option<usize> {
        if rows == 0 || x < self.left {
            return None;
        }
        Some((((x - self.left) / self.step) as usize).min(rows - 1))
    }
}

/// `color` mixed into `background`: an opaque tint, so stacked layers painted over
/// each other keep their own color.
fn tint(color: Hsla, background: Hsla, amount: f32) -> Hsla {
    let (c, b) = (color.to_rgb(), background.to_rgb());
    let mix = |c: f32, b: f32| b + (c - b) * amount;
    Rgba {
        r: mix(c.r, b.r),
        g: mix(c.g, b.g),
        b: mix(c.b, b.b),
        a: 1.,
    }
    .into()
}

impl ResultChart {
    fn color(&self, series: usize) -> Hsla {
        self.colors[series % self.colors.len().max(1)]
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
            AxisText::new(self.data.x[row].clone(), px(l.x(row)), muted).align(TextAlign::Center)
        });
        let y_labels = l
            .ticks
            .iter()
            .map(|&t| AxisText::new(format_value(t), px(l.y(t)), muted).align(TextAlign::Right));
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
            let color = self.color(s);
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
                    let points: Vec<(f32, f32)> = (0..rows)
                        .map(|row| (l.x(row), l.y(stacks[s][row].1)))
                        .collect();
                    Area::new()
                        .data(points)
                        .x(|p: &(f32, f32)| Some(p.0))
                        .y0(zero)
                        .y1(|p: &(f32, f32)| Some(p.1))
                        .fill(tint(color, background, 0.45))
                        .stroke(color)
                        .stroke_style(curve)
                        .paint(bounds, window);
                }
            }
            ChartStyle::Line | ChartStyle::Area => {
                for (s, series) in self.data.series.iter().enumerate() {
                    let color = self.color(s);
                    // NULL values are left out.
                    let points: Vec<(f32, f32)> = (0..rows)
                        .filter_map(|row| Some((l.x(row), l.y(series.values[row]?))))
                        .collect();
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
    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let l = Layout::new(bounds, &self.data, self.style);
        self.paint_axes(&l, &bounds, window, cx);
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
        let l = Layout::new(bounds, &self.data, self.style);
        if position.y.as_f32() > l.bottom {
            return None;
        }
        let row = l.row_at(position.x.as_f32(), self.data.x.len())?;
        Some(TooltipState::new(
            row,
            point(px(l.x(row)), position.y),
            Vec::new(),
        ))
    }

    fn tooltip(
        &self,
        state: &TooltipState,
        cursor: Point<Pixels>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<AnyElement> {
        let l = Layout::new(bounds, &self.data, self.style);
        let row = state.index;
        let mut cross = CrossLine::new(point(px(l.x(row)), px(l.top))).height(l.bottom - l.top);
        if self.style.is_bars() {
            cross = cross.band(px(l.step));
        }
        let mut tooltip = Tooltip::new(cursor, bounds.size)
            .gap(px(8.))
            .cross_line(cross)
            .title(self.data.x.get(row)?.clone());
        for (s, series) in self.data.series.iter().enumerate() {
            let value = series.values[row].map_or("NULL".into(), format_value);
            tooltip = tooltip.row(self.color(s), series.name.clone(), value);
        }
        if self.style.is_stacked() {
            let total: f64 = self.data.series.iter().filter_map(|s| s.values[row]).sum();
            tooltip = tooltip.row(transparent_black(), "Total", format_value(total));
        }
        Some(tooltip.into_any_element())
    }
}
