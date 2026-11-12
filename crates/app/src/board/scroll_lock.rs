//! A table on a board scrolls only once it is clicked: until then, scrolling over
//! it scrolls the board, so a fast scroll down the board doesn't get caught in
//! each table it passes. One table at a time is unlocked; scrolling anywhere
//! else, a click elsewhere or Esc locks it again.

use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex};
use gpui_kit::*;

use super::BoardTab;
use crate::assets::Lucide;

/// Over a table's body: while it is locked, takes the scroll events over it
/// before the table does and scrolls the board instead; while unlocked, locks it
/// again when a scroll happens anywhere else.
pub(super) fn guard(
    unlocked: bool,
    board_scroll: ScrollHandle,
    board: WeakEntity<BoardTab>,
) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            // Only where the table is seen (the board clips its tiles).
            let seen = bounds.intersect(&window.content_mask().bounds);
            window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                let over = seen.contains(&event.position);
                if unlocked {
                    if !over {
                        board.update(cx, |board, cx| board.lock_tables(cx)).ok();
                    }
                    return;
                }
                if !over {
                    return;
                }
                let delta = event.delta.pixel_delta(window.line_height()).y;
                let offset = board_scroll.offset();
                let y = scrolled(offset.y, delta, board_scroll.max_offset().y);
                if y != offset.y {
                    board_scroll.set_offset(point(offset.x, y));
                    window.refresh();
                }
                cx.stop_propagation();
            });
        },
    )
    .absolute()
    .inset_0()
}

/// The board's offset after scrolling `delta`, kept within its content
/// (`offset` runs from 0 down to `-max`).
pub(super) fn scrolled(offset: Pixels, delta: Pixels, max: Pixels) -> Pixels {
    (offset + delta).clamp(-max.max(Pixels::ZERO), Pixels::ZERO)
}

/// Says whether the table scrolls: "Click to scroll" while locked (shown on
/// hover), "Scrolling · Esc" while unlocked.
pub(super) fn hint(unlocked: bool, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let (icon, label) = if unlocked {
        (Lucide::LockOpen, "Scrolling · Esc")
    } else {
        (Lucide::Lock, "Click to scroll")
    };
    h_flex()
        .id("tile-scroll-hint")
        .test_support()
        .absolute()
        .top_1()
        .right_3()
        .gap_1()
        .px_1p5()
        .py_0p5()
        .items_center()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(Icon::new(icon).xsmall())
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::scrolled;
    use gpui_kit::px;

    #[test]
    fn the_board_scrolls_within_its_content() {
        assert_eq!(scrolled(px(0.), px(-30.), px(100.)), px(-30.));
        assert_eq!(scrolled(px(-90.), px(-30.), px(100.)), px(-100.));
        assert_eq!(scrolled(px(-10.), px(30.), px(100.)), px(0.));
        // Nothing to scroll.
        assert_eq!(scrolled(px(0.), px(-30.), px(0.)), px(0.));
    }
}
