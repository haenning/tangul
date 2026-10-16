//! `make demo`: the README's animation (`docs/demo.webp`). The real workspace in a
//! headless window on the dev Postgres, driven like a user would: run a query,
//! inspect a cell, show it as a chart, add it to a board. Each step is rendered by
//! Metal and saved as a PNG with a drawn mouse pointer; `scripts/demo.sh` joins
//! the frames. Only built with the `demo` feature.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::{ActiveTheme as _, Root, Theme, ThemeMode};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Entity, HeadlessAppContext, Hsla, Modifiers,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, PlatformInput, Point,
    Window, point, px, size,
};
use image::{Rgba, RgbaImage};
use tangul_core::chart::{ChartStyle, ChartView};
use tangul_data::Data;

use super::Workspace;
use crate::connection::ConnStatus;
use crate::query_tab::QueryTab;

const WIDTH: f32 = 1280.;
const HEIGHT: f32 = 800.;
/// One frame of pointer movement or typing, in ms.
const FRAME: u32 = 40;
const QUERY: &str = "select date_trunc('month', ordered)::date as month, sum(total) as revenue \
from app.orders group by 1 order by 1";

/// Records the demo's frames into `out` (emptied first) and writes `frames.txt`:
/// one line per frame, its duration in ms and its file.
pub fn record(out: &Path) {
    let _ = std::fs::remove_dir_all(out);
    std::fs::create_dir_all(out).expect("frames directory");
    let mut demo = Demo::open(out.to_path_buf());
    demo.script();
    demo.finish();
    // Not dropped: the headless app checks for leaked entities on drop, and the
    // workspace's tasks still hold some.
    std::process::exit(0);
}

struct Demo {
    cx: HeadlessAppContext,
    window: AnyWindowHandle,
    workspace: Entity<Workspace>,
    out: PathBuf,
    frames: Vec<(u32, String)>,
    pointer: Point<Pixels>,
    /// A ring around the pointer for a few frames after a click: frames left.
    ring: u32,
}

impl Demo {
    fn open(out: PathBuf) -> Self {
        let mut cx = HeadlessAppContext::with_platform(
            gpui_kit::platform::current_platform(true).text_system(),
            Arc::new(crate::assets::AppAssets),
            gpui_kit::platform::current_headless_renderer,
        );
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::syntax::init();
            Theme::change(ThemeMode::Dark, None, cx);
            crate::settings::init(cx);
            crate::bind_keys(cx);
        });
        let (engine, events) = tangul_engine::start();
        let mut workspace = None;
        let handle = cx
            .open_window(size(px(WIDTH), px(HEIGHT)), |window, cx| {
                let view = cx.new(|cx| Workspace::new(engine, events, Data::open(), window, cx));
                workspace = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("headless window");
        let mut demo = Self {
            cx,
            window: handle.into(),
            workspace: workspace.expect("workspace opened"),
            out,
            frames: Vec::new(),
            pointer: point(px(WIDTH * 0.62), px(HEIGHT * 0.7)),
            ring: 0,
        };
        // Connected, and the schema in the sidebar.
        demo.wait_until("connected", |ws, cx| {
            ws.active_user
                .as_ref()
                .and_then(|u| ws.conns.get(u))
                .is_some_and(|c| matches!(c.read(cx).status, ConnStatus::Connected))
                && ws
                    .active_user
                    .as_ref()
                    .is_some_and(|u| ws.knowledge.schema(u).is_some())
        });
        demo.idle(800);
        demo
    }

    fn script(&mut self) {
        // A new tab; the query typed into its editor.
        let tab = self.update(|ws, window, cx| {
            let tab = ws.add_tab("", window, cx);
            QueryTab::focus_editor(&tab, window, cx);
            tab
        });
        self.shot(900);
        self.type_text(QUERY);
        self.shot(500);
        // Run (formats first), then wait for the rows.
        self.in_window(|window, cx| window.press("secondary-enter", cx));
        let ready = tab.clone();
        self.wait_until("result", move |_, cx| {
            let t = ready.read(cx);
            !t.is_running(cx) && t.row_count(cx).is_some()
        });
        self.shot(1400);

        // Inspect a revenue cell.
        let cell = self.bounds_within(("row", 4usize), ("cell", 1usize));
        self.move_to(cell.center(), 600);
        self.double_click_here();
        self.shot(2000);
        // Esc closes the viewer again (like Quick Look).
        self.in_window(|window, cx| window.press("escape", cx));
        self.shot(400);

        // Show it as a chart: the status bar's chart menu, bars of revenue.
        let button = self.bounds("chart");
        self.move_to(button.center(), 700);
        self.click_here();
        self.shot(700);
        let menu = self.bounds("popup-menu");
        // "Bars" is the menu's last item.
        let bars = point(
            menu.origin.x + px(60.),
            menu.origin.y + menu.size.height - px(18.),
        );
        self.move_to(bars, 450);
        self.hover_here();
        self.shot(350);
        self.click_here();
        self.wait_until("chart", move |_, cx| {
            tab.read(cx).chart_view(cx)
                == Some(ChartView {
                    style: ChartStyle::Bars,
                    series: Some(0),
                })
        });
        self.shot(1800);

        // Add to board: the tab's menu, a new board.
        let tab_title = self.active_tab_bounds();
        self.move_to(tab_title.center(), 700);
        let at = self.pointer;
        self.in_window(move |window, cx| right_click(window, at, cx));
        self.ring = 4;
        self.shot(700);
        let menu = self.bounds("popup-menu");
        let add = point(
            menu.origin.x + px(60.),
            menu.origin.y + menu.size.height - px(18.),
        );
        self.move_to(add, 450);
        self.hover_here();
        self.shot(350);
        self.click_here();
        // The dialog slides in: measure it once it stands.
        self.idle(400);
        self.shot(900);
        // The dialog: the tile's title, a name for the new board, then Add.
        let title = self.bounds("add-title");
        self.move_to(title.center(), 500);
        self.click_here();
        self.in_window(|window, cx| window.press("secondary-a", cx));
        self.type_text("Revenue per month");
        let name = self.bounds("new-board-name");
        self.move_to(name.center(), 450);
        self.click_here();
        self.type_text("Sales");
        self.shot(500);
        let add = self.bounds("add");
        self.move_to(add.center(), 550);
        self.click_here();
        self.wait_until("board", |ws, _| {
            ws.tabs.active().is_some_and(|t| t.board().is_some())
        });
        self.idle(1500);
        self.shot(3000);
    }

    /// Types `text` into what has the focus, a frame per keystroke or two.
    fn type_text(&mut self, text: &str) {
        for chunk in typed(text) {
            self.in_window(move |window, cx| window.input(&chunk, cx));
            self.shot(FRAME + 25);
        }
    }

    /// Lets the app run for `ms`: its clock and the engine's real time.
    fn idle(&mut self, ms: u32) {
        let until = Instant::now() + Duration::from_millis(ms.into());
        while Instant::now() < until {
            self.tick(Duration::from_millis(20));
        }
    }

    fn tick(&mut self, step: Duration) {
        std::thread::sleep(step);
        self.cx.advance_clock(step);
        self.cx.run_until_parked();
    }

    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&Workspace, &App) -> bool + 'static) {
        let workspace = self.workspace.clone();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.tick(Duration::from_millis(20));
            if self.cx.update(|cx| done(workspace.read(cx), cx)) {
                return;
            }
            assert!(Instant::now() < deadline, "demo: no {what} after 30 s");
        }
    }

    fn update<R>(
        &mut self,
        f: impl FnOnce(&mut Workspace, &mut Window, &mut gpui_kit::Context<Workspace>) -> R,
    ) -> R {
        let workspace = self.workspace.clone();
        let out = self
            .cx
            .update_window(self.window, |_, window, cx| {
                workspace.update(cx, |ws, cx| f(ws, window, cx))
            })
            .expect("window");
        self.cx.run_until_parked();
        out
    }

    fn in_window<R>(&mut self, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        let out = self
            .cx
            .update_window(self.window, |_, window, cx| {
                window.render_frame(cx);
                f(window, cx)
            })
            .expect("window");
        self.cx.run_until_parked();
        out
    }

    fn bounds(&mut self, id: impl Into<gpui_kit::ElementId>) -> Bounds<Pixels> {
        let id = id.into();
        self.in_window(move |window, _| window.find(id).bounds())
    }

    fn bounds_within(
        &mut self,
        scope: impl Into<gpui_kit::ElementId>,
        id: impl Into<gpui_kit::ElementId>,
    ) -> Bounds<Pixels> {
        let (scope, id) = (scope.into(), id.into());
        self.in_window(move |window, _| window.within(scope).find(id).bounds())
    }

    /// The active tab's title in the tab bar.
    fn active_tab_bounds(&mut self) -> Bounds<Pixels> {
        let id = self.update(|ws, _, cx| ws.tabs.active_query().map(|t| t.read(cx).id.0));
        self.bounds(("tab-menu", id.expect("a query tab") as usize))
    }

    /// Moves the pointer to `to` over `ms`, eased, one frame per [`FRAME`].
    fn move_to(&mut self, to: Point<Pixels>, ms: u32) {
        let from = self.pointer;
        let steps = (ms / FRAME).max(1);
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let t = t * t * (3. - 2. * t);
            self.pointer = point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
            self.hover_here();
            self.shot(FRAME);
        }
    }

    fn hover_here(&mut self) {
        let at = self.pointer;
        self.in_window(move |window, cx| {
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: at,
                    pressed_button: None,
                    modifiers: Modifiers::default(),
                }),
                cx,
            );
        });
    }

    fn click_here(&mut self) {
        self.press_here(1);
    }

    fn double_click_here(&mut self) {
        self.press_here(1);
        self.press_here(2);
    }

    fn press_here(&mut self, click_count: usize) {
        let position = self.pointer;
        self.in_window(move |window, cx| {
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Modifiers::default(),
                    click_count,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Modifiers::default(),
                    click_count,
                }),
                cx,
            );
        });
        self.ring = 4;
    }

    /// Renders the window and keeps the frame for `ms`.
    fn shot(&mut self, ms: u32) {
        self.tick(Duration::from_millis(FRAME.into()));
        self.in_window(|window, cx| window.render_frame(cx));
        let mut image = self
            .cx
            .capture_screenshot(self.window)
            .expect("Metal frame");
        let (fill, outline, ring) = self.cx.update(|cx| {
            let theme = cx.theme();
            (theme.background, theme.foreground, theme.primary)
        });
        let scale = image.width() as f32 / WIDTH;
        let at = (
            f32::from(self.pointer.x) * scale,
            f32::from(self.pointer.y) * scale,
        );
        if self.ring > 0 {
            draw_ring(&mut image, at, scale, 5 - self.ring, ring);
            self.ring -= 1;
        }
        draw_pointer(&mut image, at, scale, fill, outline);
        let name = format!("{:04}.png", self.frames.len());
        image.save(self.out.join(&name)).expect("frame saved");
        self.frames.push((ms, name));
    }

    fn finish(&self) {
        let list: String = self
            .frames
            .iter()
            .map(|(ms, name)| format!("{ms} {name}\n"))
            .collect();
        std::fs::write(self.out.join("frames.txt"), list).expect("frames.txt");
    }
}

fn right_click(window: &mut Window, position: Point<Pixels>, cx: &mut App) {
    window.dispatch_event(
        PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Right,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        }),
        cx,
    );
    window.dispatch_event(
        PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Right,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
        }),
        cx,
    );
}

/// `text` in the pieces a person types: a character at a time, words of SQL
/// keywords a little faster.
fn typed(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let mut piece = c.to_string();
        if c.is_alphabetic() && chars.peek().is_some_and(|n| n.is_alphabetic()) {
            piece.push(chars.next().unwrap_or_default());
        }
        out.push(piece);
    }
    out
}

/// The arrow pointer (tip at 0, 0), in points.
const ARROW: [(f32, f32); 7] = [
    (0., 0.),
    (0., 17.),
    (4.2, 13.2),
    (7., 19.5),
    (9.6, 18.4),
    (6.9, 12.2),
    (12.2, 12.2),
];

fn draw_pointer(image: &mut RgbaImage, at: (f32, f32), scale: f32, fill: Hsla, outline: Hsla) {
    let poly: Vec<(f32, f32)> = ARROW
        .iter()
        .map(|(x, y)| (at.0 + x * scale, at.1 + y * scale))
        .collect();
    let edge = 1.4 * scale;
    let (fill, outline) = (channels(fill), channels(outline));
    let x0 = (at.0 - edge).floor().max(0.) as u32;
    let y0 = (at.1 - edge).floor().max(0.) as u32;
    let x1 = ((at.0 + 14. * scale) as u32).min(image.width());
    let y1 = ((at.1 + 22. * scale) as u32).min(image.height());
    for y in y0..y1 {
        for x in x0..x1 {
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let inside = contains(&poly, p);
            let d = distance(&poly, p);
            let color = if inside && d > edge {
                Some((fill, 1.))
            } else if inside || d <= edge {
                Some((outline, 1.))
            } else if d <= edge + 1. {
                Some((outline, edge + 1. - d))
            } else {
                None
            };
            if let Some((c, alpha)) = color {
                blend(image.get_pixel_mut(x, y), c, alpha);
            }
        }
    }
}

/// A ring that grows from the pointer's tip after a click (`step` 1 to 4).
fn draw_ring(image: &mut RgbaImage, at: (f32, f32), scale: f32, step: u32, color: Hsla) {
    let radius = (6. + 4. * step as f32) * scale;
    let width = 2. * scale;
    let alpha = 0.7 - 0.15 * step as f32;
    let color = channels(color);
    let r = (radius + width) as i64;
    for dy in -r..=r {
        for dx in -r..=r {
            let (x, y) = (at.0 as i64 + dx, at.1 as i64 + dy);
            if x < 0 || y < 0 || x >= image.width() as i64 || y >= image.height() as i64 {
                continue;
            }
            let d = ((dx * dx + dy * dy) as f32).sqrt();
            let off = (d - radius).abs();
            if off < width {
                blend(
                    image.get_pixel_mut(x as u32, y as u32),
                    color,
                    alpha * (1. - off / width),
                );
            }
        }
    }
}

fn channels(color: Hsla) -> [f32; 3] {
    let c = color.to_rgb();
    [c.r * 255., c.g * 255., c.b * 255.]
}

fn blend(pixel: &mut Rgba<u8>, color: [f32; 3], alpha: f32) {
    for (channel, target) in pixel.0.iter_mut().zip(color) {
        let old = *channel as f32;
        *channel = (old + (target - old) * alpha).round() as u8;
    }
}

fn contains(poly: &[(f32, f32)], p: (f32, f32)) -> bool {
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[j]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn distance(poly: &[(f32, f32)], p: (f32, f32)) -> f32 {
    let mut best = f32::MAX;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy)).clamp(0., 1.);
        let (x, y) = (a.0 + t * dx - p.0, a.1 + t * dy - p.1);
        best = best.min((x * x + y * y).sqrt());
    }
    best
}
