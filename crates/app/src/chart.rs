//! One chart for a result: all series (value columns) over column 0, as lines,
//! areas, stacked areas, grouped bars or stacked bars (see `tangul_assist::chart`).
//! Drawn with gpui-kit's plot shapes; the layout math is done here.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use gpui_kit::component::{
    ActiveTheme as _, StyledExt as _, h_flex,
    plot::{
        AxisLabelSide, AxisText, Curve, Grid, IntoPlot, PathCaches, Plot, PlotAxis, ShapeKey,
        axis_gutter,
        shape::{Area, Line},
        tooltip::{CrossLine, Dot, PlotHover, Tooltip, TooltipState},
    },
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_assist::chart::{
    ChartData, ChartStyle, format_value, min_max_per_pixel, peak_per_pixel, simplify, ticks,
};

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
/// Height of one series' line in the tooltip.
const TIP_ROW: f32 = 18.;
/// Room in the tooltip for the title, "+N more", the total and the padding.
const TIP_FRAME: f32 = 64.;
/// Most columns the tooltip lays its series out in; what doesn't fit then is
/// the smallest values, in the pinned list (a click).
const TIP_COLUMNS: usize = 4;
/// Radius of the outer corners of a bar, or of a whole stack of them.
const BAR_RADIUS: f32 = 3.;
/// Points of a line closer than this (px) to the line without them are left out.
const LINE_TOLERANCE: f32 = 0.5;
/// Below this (narrow bars) no rounding.
const MIN_RADIUS: f32 = 1.5;
/// Points along each rounded corner.
const ARC_STEPS: usize = 6;
/// How much of their color the other series keep while one is under the pointer
/// (mixed with the background, so stacked bands and crossing lines stay opaque).
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
            bottom: (height - axis_gutter(px(LABEL_SIZE * scale))).max(TOP_PAD + 1.),
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
    /// there are many more rows than pixels (see `min_max_per_pixel`), then to
    /// those that move it by half a pixel or more (`simplify`): it looks the
    /// same, with fewer points to draw on every frame.
    fn line(&self, points: Vec<(f32, f32)>, bounds: &Bounds<Pixels>) -> Vec<(f32, f32)> {
        let points = min_max_per_pixel(&points, bounds.size.width.as_f32() - self.left);
        simplify(&points, LINE_TOLERANCE)
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

    /// A series' color as drawn: faded towards the background while another one
    /// is highlighted. Mixed, not made transparent: a transparent band would show
    /// the ones below it through, as if they weren't stacked.
    fn shown(&self, series: usize, color: Hsla, background: Hsla) -> Hsla {
        match self.highlighted() {
            Some((s, focus)) if s != series => {
                crate::theme::tint(color, background, 1. - (1. - DIMMED) * focus)
            }
            _ => color,
        }
    }

    /// The series in drawing order: the highlighted one last, on top of the others.
    fn drawing_order(&self) -> Vec<usize> {
        let top = self.highlighted().map(|(s, _)| s);
        let mut order: Vec<usize> = (0..self.data.series.len())
            .filter(|&s| Some(s) != top)
            .collect();
        order.extend(top.filter(|&s| s < self.data.series.len()));
        order
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
        // Solid and faint: a dashed line is a rectangle per dash, on every frame.
        let grid = Grid::new()
            .y(l.ticks.iter().map(|&t| px(l.y(t))))
            .stroke(crate::theme::tint(theme.border, theme.background, 0.6));
        crate::perf::span("chart.grid", || grid.paint(&plot, window));

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
        let axis = PlotAxis::new()
            .stroke(theme.border)
            .x(px(l.y(0.0)))
            .x_label(x_labels)
            .y(px(l.left - 4.))
            .y_axis(false)
            .y_label_side(AxisLabelSide::Start)
            .y_label(y_labels);
        crate::perf::span("chart.axis_labels", || axis.paint(bounds, window, cx));
    }

    fn paint_bars(&self, l: &Layout, bounds: &Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let series = self.data.series.len();
        let rows = self.data.x.len();
        let group = l.step * BAR_GROUP;
        let stacks = self.style.is_stacked().then(|| self.data.stacks());
        // Each row's stack ends, up and down: the corners round there, whichever
        // segment they fall on.
        let ends: Vec<(f64, f64)> = (0..rows)
            .map(|row| {
                stacks.iter().flatten().fold((0f64, 0f64), |(up, down), s| {
                    let (a, b) = s[row];
                    (up.max(a.max(b)), down.min(a.min(b)))
                })
            })
            .collect();
        let width = if stacks.is_some() {
            group
        } else {
            group / series as f32
        };
        let band = (width - 1.).max(1.);
        // More rows than pixels: one bar per pixel column, the tallest.
        let xs: Vec<f32> = (0..rows).map(|row| l.x(row)).collect();
        let extents: Vec<f64> = match &stacks {
            Some(_) => ends.iter().map(|(up, down)| up - down).collect(),
            None => (0..rows)
                .map(|row| {
                    let values = self.data.series.iter().filter_map(|s| s.values[row]);
                    values.map(f64::abs).fold(0., f64::max)
                })
                .collect(),
        };
        let drawn = peak_per_pixel(&xs, &extents);
        let background = cx.theme().background;
        for (s, values) in self.data.series.iter().enumerate() {
            let color = self.shown(s, self.color(s), background);
            for &row in &drawn {
                let slot = l.x(row) - group / 2.;
                // (left edge, from, to, stack end) in values.
                let (left, from, to, end) = match &stacks {
                    Some(stacks) => {
                        let (from, to) = stacks[s][row];
                        let end = if to >= from { ends[row].0 } else { ends[row].1 };
                        (slot, from, to, end)
                    }
                    None => {
                        let Some(v) = values.values[row] else {
                            continue;
                        };
                        (slot + width * s as f32, 0., v, v)
                    }
                };
                if from == to {
                    continue;
                }
                let (x0, x1) = (left, left + band);
                let (y0, y1) = (l.y(from), l.y(to));
                let at = |x: f32, y: f32| bounds.origin + point(px(x), px(y));
                match rounded_segment(x0, x1, y0, y1, l.y(end), BAR_RADIUS) {
                    Some(points) => {
                        let mut path = PathBuilder::fill();
                        path.move_to(at(points[0].0, points[0].1));
                        for &(x, y) in &points[1..] {
                            path.line_to(at(x, y));
                        }
                        path.close();
                        if let Ok(path) = path.build() {
                            window.paint_path(path, color);
                        }
                    }
                    None => window.paint_quad(fill(
                        Bounds::from_corners(at(x0, y0.min(y1)), at(x1, y0.max(y1))),
                        color,
                    )),
                }
            }
        }
    }

    /// The points of every line and area, by cache slot: stacked areas at their
    /// series; lines at theirs, areas' fills after them.
    fn shapes(&self, l: &Layout, bounds: &Bounds<Pixels>) -> Vec<Shape> {
        let rows = self.data.x.len();
        let zero = l.y(0.0);
        let curve = self.curve();
        let shape = |slot, base, points: Vec<(f32, f32)>| {
            let points = l.line(points, bounds);
            Shape {
                slot,
                key: shape_key(curve, base, &points),
                points,
            }
        };
        match self.style {
            ChartStyle::StackedArea => {
                let stacks = self.data.stacks();
                (0..stacks.len())
                    .map(|s| {
                        let points = (0..rows).map(|row| (l.x(row), l.y(stacks[s][row].1)));
                        shape(s, zero, points.collect())
                    })
                    .collect()
            }
            ChartStyle::Line | ChartStyle::Area => {
                let series_count = self.data.series.len();
                let mut shapes: Vec<Shape> = self
                    .data
                    .series
                    .iter()
                    .enumerate()
                    .map(|(s, series)| {
                        // NULL values are left out.
                        let points =
                            (0..rows).filter_map(|row| Some((l.x(row), l.y(series.values[row]?))));
                        shape(s, f32::NAN, points.collect())
                    })
                    .collect();
                if self.style == ChartStyle::Area {
                    let fills: Vec<Shape> = shapes
                        .iter()
                        .map(|line| Shape {
                            slot: series_count + line.slot,
                            key: shape_key(curve, zero, &line.points),
                            points: line.points.clone(),
                        })
                        .collect();
                    shapes.extend(fills);
                }
                shapes
            }
            ChartStyle::Bars | ChartStyle::StackedBars => Vec::new(),
        }
    }

    /// Straight segments are exact; curves can swing past the values between points.
    fn curve(&self) -> Curve {
        if self.smooth {
            Curve::Natural
        } else {
            Curve::Linear
        }
    }

    /// Lines and areas (`shapes`), through `caches`: their tessellated shapes
    /// are reused while the points stay the same (scrolling only moves them; the
    /// highlight only changes colors).
    fn paint_lines(
        &self,
        l: &Layout,
        shapes: &[Shape],
        bounds: &Bounds<Pixels>,
        caches: &mut PathCaches,
        window: &mut Window,
        cx: &App,
    ) {
        let background = cx.theme().background;
        let curve = self.curve();
        let zero = l.y(0.0);
        let xy = |p: &(f32, f32)| Some(p.0);
        let yy = |p: &(f32, f32)| Some(p.1);
        match self.style {
            ChartStyle::StackedArea => {
                // Top layer first, each filled down to 0, so every band shows between
                // its own line and the one below. Its top line is the same shape in
                // the line's color, 1px above the fill: one tessellated shape per
                // band instead of a fill and a (costlier) stroke.
                let plot = Bounds::from_corners(
                    bounds.origin,
                    bounds.origin + point(bounds.size.width, px(l.bottom)),
                );
                for shape in shapes.iter().rev() {
                    let s = shape.slot;
                    let color = self.color(s);
                    let (fill, line) = (
                        self.shown(s, crate::theme::tint(color, background, 0.45), background),
                        self.shown(s, color, background),
                    );
                    let cache = caches.slot(s);
                    let build = || area_path(&shape.points, zero, curve);
                    if let Some(path) = cache.get(shape.key, bounds.origin, build) {
                        window.paint_path(path, line);
                    }
                    let below = bounds.origin + point(px(0.), px(1.));
                    if let Some(path) = cache.get(shape.key, below, || None) {
                        window.with_content_mask(Some(ContentMask { bounds: plot }), |window| {
                            window.paint_path(path, fill)
                        });
                    }
                }
            }
            ChartStyle::Line | ChartStyle::Area => {
                let series_count = self.data.series.len();
                for s in self.drawing_order() {
                    let color = self.shown(s, self.color(s), background);
                    if self.style == ChartStyle::Area
                        && let Some(fill) = shapes.iter().find(|f| f.slot == series_count + s)
                    {
                        let (fill_cache, line_cache) = caches.slot_pair(fill.slot);
                        Area::new()
                            .data(fill.points.iter().copied())
                            .x(xy)
                            .y0(zero)
                            .y1(yy)
                            .fill(color.opacity(0.18))
                            .stroke(transparent_black())
                            .curve(curve)
                            .paint_cached(bounds, fill_cache, line_cache, window);
                    }
                    if let Some(line) = shapes.get(s) {
                        Line::new()
                            .data(line.points.iter().copied())
                            .x(xy)
                            .y(yy)
                            .stroke(color)
                            .stroke_width(2.)
                            .curve(curve)
                            .paint_cached(bounds, caches.slot(s), window);
                    }
                }
            }
            ChartStyle::Bars | ChartStyle::StackedBars => {}
        }
    }
}

/// The outline of an area under `points` down to `base` (relative to the
/// chart's origin), straight or as `Curve::Natural` draws it (Catmull-Rom).
fn area_path(points: &[(f32, f32)], base: f32, curve: Curve) -> Option<Path<Pixels>> {
    let at = |p: (f32, f32)| point(px(p.0), px(p.1));
    let (&first, &last) = (points.first()?, points.last()?);
    let mut path = PathBuilder::fill();
    path.move_to(at(first));
    match curve {
        Curve::Natural if points.len() > 2 => {
            let n = points.len();
            for i in 0..n - 1 {
                let p0 = points[i.saturating_sub(1)];
                let (p1, p2) = (points[i], points[i + 1]);
                let p3 = points[(i + 2).min(n - 1)];
                let c1 = (p1.0 + (p2.0 - p0.0) / 6., p1.1 + (p2.1 - p0.1) / 6.);
                let c2 = (p2.0 - (p3.0 - p1.0) / 6., p2.1 - (p3.1 - p1.1) / 6.);
                path.cubic_bezier_to(at(p2), at(c1), at(c2));
            }
        }
        _ => {
            for &p in &points[1..] {
                path.line_to(at(p));
            }
        }
    }
    path.line_to(at((last.0, base)));
    path.line_to(at((first.0, base)));
    path.close();
    path.build().ok()
}

/// One line or area as drawn: its points (relative to the chart's origin), the
/// cache slot it is painted through and what it is built from.
struct Shape {
    slot: usize,
    points: Vec<(f32, f32)>,
    key: u64,
}

/// A chart's layout and shapes, kept while its data, size, style and curves
/// stay: scrolling and hovering don't compute them again.
#[derive(Default)]
struct Geometry {
    made_of: Option<(Rc<ChartData>, ChartStyle, bool, u32, u32, u32)>,
    layout: Option<Rc<Layout>>,
    shapes: Rc<Vec<Shape>>,
}

impl Geometry {
    fn for_paint(window: &mut Window, cx: &mut App) -> Entity<Self> {
        window.use_keyed_state("chart-geometry", cx, |_, _| Self::default())
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
        // Asked for on every paint, also out of view, so the shapes stay cached
        // while a board scrolls a tile out and back in.
        let caches = PathCaches::for_paint("chart-shapes", window, cx);
        let last_keys = crate::perf::recording().then(|| ShapeKeys::for_paint(window, cx));
        // Scrolled out of view (a board's other tiles): nothing to paint.
        if !window.content_mask().bounds.intersects(&bounds) {
            return;
        }
        let geometry = Geometry::for_paint(window, cx);
        let made_of = (
            self.data.clone(),
            self.style,
            self.smooth,
            bounds.size.width.as_f32().to_bits(),
            bounds.size.height.as_f32().to_bits(),
            self.label_size.to_bits(),
        );
        let (l, shapes) = geometry.update(cx, |g, _| {
            let same = g.made_of.as_ref().is_some_and(|m| {
                Rc::ptr_eq(&m.0, &made_of.0)
                    && (m.1, m.2, m.3, m.4, m.5)
                        == (made_of.1, made_of.2, made_of.3, made_of.4, made_of.5)
            });
            if !same || g.layout.is_none() {
                crate::perf::span("chart.geometry", || {
                    let l = self.layout(bounds);
                    g.shapes = Rc::new(self.shapes(&l, &bounds));
                    g.layout = Some(Rc::new(l));
                });
                g.made_of = Some(made_of);
            }
            (g.layout.clone().unwrap(), g.shapes.clone())
        });
        crate::perf::span("chart.axes", || self.paint_axes(&l, &bounds, window, cx));
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
        let start = Instant::now();
        if self.style.is_bars() {
            self.paint_bars(&l, &bounds, window, cx);
            crate::perf::chart_painted(start.elapsed(), None);
            return;
        }
        let keys: Vec<(usize, u64)> = shapes.iter().map(|s| (s.slot, s.key)).collect();
        crate::perf::span("chart.paint_shapes", || {
            caches.update(cx, |caches, cx| {
                self.paint_lines(&l, &shapes, &bounds, caches, window, cx)
            })
        });
        let took = start.elapsed();
        if let Some(last) = last_keys {
            // The shapes whose points changed since the last paint were built
            // (the caches' rule: a shape is rebuilt when its points change).
            let built = last.update(cx, |last, _| {
                let built = keys
                    .iter()
                    .filter(|(slot, key)| last.0.get(slot) != Some(key))
                    .count();
                last.0 = keys.iter().copied().collect();
                built
            });
            crate::perf::chart_painted(took, Some((built, keys.len() - built)));
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
            .map(|s| (s, hover.progress()));
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
                            .child(series_label(series)),
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

/// A bar segment from `x0` to `x1` and `y0` to `y1` (pixels) whose bar or stack
/// ends at `tip`, rounded there with `radius`: its outline, or `None` when the
/// rounding doesn't reach it (a plain rectangle). The rounding belongs to the
/// whole stack: thin segments at its end share the corners.
fn rounded_segment(
    x0: f32,
    x1: f32,
    y0: f32,
    y1: f32,
    tip: f32,
    radius: f32,
) -> Option<Vec<(f32, f32)>> {
    let r = radius.min((x1 - x0) / 2.);
    // Too thin to show a rounding: a rectangle, cheaper to draw.
    if r < MIN_RADIUS {
        return None;
    }
    // Distances from the tip, nearer first; the segment lies on one side of it.
    let (near, far) = {
        let (a, b) = ((y0 - tip).abs(), (y1 - tip).abs());
        (a.min(b), a.max(b))
    };
    if near >= r {
        return None;
    }
    let dir = if y0 + y1 >= 2. * tip { 1. } else { -1. };
    let inset = |t: f32| r - (r * r - (r - t).powi(2)).max(0.).sqrt();
    let arc_end = far.min(r);
    let arc: Vec<f32> = (0..=ARC_STEPS)
        .map(|i| near + (arc_end - near) * i as f32 / ARC_STEPS as f32)
        .collect();
    let mut points: Vec<(f32, f32)> = arc
        .iter()
        .map(|&t| (x0 + inset(t), tip + dir * t))
        .collect();
    if far > r {
        points.push((x0, tip + dir * far));
        points.push((x1, tip + dir * far));
    }
    points.extend(arc.iter().rev().map(|&t| (x1 - inset(t), tip + dir * t)));
    Some(points)
}

/// Each cached shape's points at the last paint, by cache slot (to count what
/// was built, see `crate::perf`).
#[derive(Default)]
struct ShapeKeys(std::collections::HashMap<usize, u64>);

impl ShapeKeys {
    fn for_paint(window: &mut Window, cx: &mut App) -> Entity<Self> {
        window.use_keyed_state("chart-shape-keys", cx, |_, _| Self::default())
    }
}

/// What a cached shape is built from: its points, curve and base line.
fn shape_key(curve: Curve, base: f32, points: &[(f32, f32)]) -> u64 {
    let mut key = ShapeKey::new((curve, base.to_bits()));
    for &(x, y) in points {
        key.f32(x).f32(y);
    }
    key.finish()
}

/// A series' name; "Other" says how many it adds up.
pub fn series_label(series: &tangul_assist::chart::Series) -> SharedString {
    match series.other {
        Some(n) => format!("Other ({n} series)").into(),
        None => series.name.clone().into(),
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

#[cfg(test)]
mod tests {
    use super::rounded_segment;

    #[test]
    fn a_stack_rounds_as_one_bar() {
        // Far from the tip: a plain rectangle.
        assert_eq!(rounded_segment(0., 10., 50., 100., 40., 3.), None);
        // A bar reaching up to its tip: the top corners cut, the bottom square.
        let bar = rounded_segment(0., 10., 100., 40., 40., 3.).unwrap();
        assert_eq!(bar.first(), Some(&(3., 40.)));
        assert!(bar.contains(&(0., 100.)) && bar.contains(&(10., 100.)));
        assert_eq!(bar.last(), Some(&(7., 40.)));
        // A 1px segment at the tip of a stack is narrower than the bar…
        let top = rounded_segment(0., 10., 41., 40., 40., 3.).unwrap();
        assert!(top.iter().all(|&(x, _)| x > 0. && x < 10.));
        // …and the one below it continues the same curve.
        let next = rounded_segment(0., 10., 42., 41., 40., 3.).unwrap();
        assert_eq!(top[top.len() / 2 - 1].0, next[0].0);
        // Negative bars round at their bottom.
        let down = rounded_segment(0., 10., 50., 80., 80., 3.).unwrap();
        assert_eq!(down.first(), Some(&(3., 80.)));
        // Narrow bars: never more than half their width, none when too thin.
        let thin = rounded_segment(0., 4., 100., 40., 40., 3.).unwrap();
        assert_eq!(thin.first(), Some(&(2., 40.)));
        assert_eq!(rounded_segment(0., 2., 100., 40., 40., 3.), None);
    }
}
