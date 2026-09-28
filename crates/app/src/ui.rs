//! Small UI pieces shared by views: text in tooltips and cards that wraps instead
//! of running across the window.

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// Widest a tooltip, card or menu note gets; longer text wraps.
pub const TEXT_WIDTH: f32 = 320.;
/// Lines of a long text shown before "Show all".
const LINES: usize = 3;
/// Characters of a line at the cards' size, to tell whether a text needs more than
/// [`LINES`] lines (an estimate: a clamped text still shows "Show all").
const LINE_CHARS: usize = 48;

/// A tooltip whose text wraps at [`TEXT_WIDTH`].
pub fn wrapped_tooltip(
    text: impl Into<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |window, cx| {
        let text = text.clone();
        Tooltip::element(move |_, _| {
            div()
                .max_w(px(TEXT_WIDTH))
                .whitespace_normal()
                .child(text.clone())
        })
        .build(window, cx)
    }
}

/// Whether `text` takes more than [`LINES`] lines in a card.
pub fn is_long(text: &str) -> bool {
    text.lines().count() > LINES
        || text
            .lines()
            .map(|l| l.chars().count().div_ceil(LINE_CHARS).max(1))
            .sum::<usize>()
            > LINES
}

/// A hover card for text of any length (a table's comment): wrapped at
/// [`TEXT_WIDTH`], at most three lines, the rest behind "Show all". Use it with
/// `hoverable_tooltip`, so the pointer can reach the button.
pub struct TextCard {
    text: SharedString,
    expanded: bool,
}

impl TextCard {
    /// Builds a new card each time it opens (collapsed again).
    pub fn builder(
        text: impl Into<SharedString>,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        let text = text.into();
        move |_, cx| {
            let text = text.clone();
            cx.new(|_| TextCard {
                text,
                expanded: false,
            })
            .into()
        }
    }
}

impl Render for TextCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let clamped = !self.expanded && is_long(&self.text);
        v_flex()
            .id("text-card")
            .test_support()
            .max_w(px(TEXT_WIDTH))
            .px_2()
            .py_1p5()
            .gap_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .text_xs()
            .shadow_md()
            .child(
                div()
                    .whitespace_normal()
                    .when(clamped, |d| d.line_clamp(LINES))
                    .child(self.text.clone()),
            )
            .when(clamped, |this| {
                this.child(
                    Button::new("show-all")
                        .label("Show all")
                        .xsmall()
                        .ghost()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.expanded = true;
                            cx.notify();
                        })),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::is_long;

    #[test]
    fn long_texts() {
        assert!(!is_long("Customer orders; totals in EUR"));
        assert!(!is_long(&"x".repeat(144)));
        assert!(is_long(&"x".repeat(145)));
        assert!(is_long("a\nb\nc\nd"));
        assert!(!is_long("a\nb\nc"));
    }
}
