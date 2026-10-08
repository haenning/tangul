//! Small UI pieces shared by views: text in tooltips and cards that wraps instead
//! of running across the window, warnings and errors as cards, and menu rows with
//! a title and a short description.

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::PopupMenuItem,
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

/// How serious an [`alert_card`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Something to look at (yellow, a triangle).
    Warning,
    /// Something that doesn't work (red, an "i").
    Error,
}

/// A warning or error as a card: border and icon in its color, the background a
/// tenth of it, the icon on top and the reason below in the text color. The one
/// way warnings look, wherever they are.
pub fn alert_card(severity: Severity, text: impl Into<SharedString>, cx: &App) -> Div {
    let theme = cx.theme();
    let (color, icon) = match severity {
        Severity::Warning => (theme.warning, IconName::TriangleAlert),
        Severity::Error => (theme.danger, IconName::Info),
    };
    // Opaque underneath, so the tint looks the same over anything.
    div().rounded(theme.radius).bg(theme.popover).child(
        v_flex()
            .gap_1()
            .px_2()
            .py_1p5()
            .rounded(theme.radius)
            .border_1()
            .border_color(color)
            .bg(color.opacity(0.1))
            .child(Icon::new(icon).small().text_color(color))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.foreground)
                    .whitespace_normal()
                    .child(text.into()),
            ),
    )
}

/// A tooltip that is an [`alert_card`], wrapped at [`TEXT_WIDTH`].
pub fn alert_tooltip(
    severity: Severity,
    text: impl Into<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| {
        let text = text.clone();
        cx.new(|_| AlertTip { severity, text }).into()
    }
}

struct AlertTip {
    severity: Severity,
    text: SharedString,
}

impl Render for AlertTip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        alert_card(self.severity, self.text.clone(), cx)
            .max_w(px(TEXT_WIDTH))
            .shadow_md()
    }
}

/// A menu row: a short title and, below it, what it means in a few words. Give it
/// `.checked()` and `.on_click()` like any item.
pub fn menu_choice(
    title: impl Into<SharedString>,
    description: Option<&'static str>,
) -> PopupMenuItem {
    let title = title.into();
    PopupMenuItem::element(move |_, cx| {
        v_flex()
            .child(title.clone())
            .when_some(description, |this, d| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(d),
                )
            })
    })
}

/// A menu section's heading; with `info`, an (i) at the right that explains it on
/// hover.
pub fn menu_section(title: &'static str, info: Option<&'static str>) -> PopupMenuItem {
    PopupMenuItem::element(move |_, cx| {
        let muted = cx.theme().muted_foreground;
        h_flex()
            .w(px(TEXT_WIDTH))
            .gap_2()
            .items_center()
            .text_xs()
            .text_color(muted)
            .child(div().flex_1().child(title))
            .when_some(info, |this, info| {
                this.child(
                    div()
                        .id(SharedString::from(format!("info-{title}")))
                        .test_support()
                        .child(Icon::new(IconName::Info).xsmall().text_color(muted))
                        .tooltip(wrapped_tooltip(info)),
                )
            })
    })
    .disabled(true)
}

/// A warning or error card as a menu row (not clickable).
pub fn menu_alert(severity: Severity, text: impl Into<SharedString>) -> PopupMenuItem {
    let text = text.into();
    PopupMenuItem::element(move |_, cx| {
        div()
            .id("menu-alert")
            .test_support()
            .w(px(TEXT_WIDTH))
            .child(alert_card(severity, text.clone(), cx))
    })
    .disabled(true)
}

/// The AI menus (title bar, AI settings): "Your user" as the AI's user, in a few
/// words.
pub const YOUR_USER: &str = "Reads what you can read";

/// The (i) of "AI connects as".
pub const USER_INFO: &str = "The database user the AI's queries run as. A user that can only read on the \
     server is the safest.";

/// The (i) of the column values: what they don't limit.
pub const VALUES_INFO: &str = "Profiles the AI may take of a column's values. They don't limit the AI's own \
     queries; the rights of its database user do.";

/// Width of a form's labels, so that its controls line up.
pub const FORM_LABEL_WIDTH: f32 = 112.;

/// One choice out of a few as a segmented control: a muted track, the chosen
/// option raised on the background color. The accent stays for what needs it. The
/// options get the ids `(id, index)`.
pub fn segmented<T: Copy + PartialEq + 'static>(
    id: &'static str,
    options: impl IntoIterator<Item = (T, SharedString)>,
    chosen: T,
    on_select: impl Fn(&T, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    let (fg, muted, raised) = (theme.foreground, theme.muted_foreground, theme.background);
    let on_select = std::rc::Rc::new(on_select);
    h_flex()
        .flex_wrap()
        .gap_0p5()
        .p_0p5()
        .rounded(theme.radius)
        .bg(theme.tab_bar_segmented)
        .children(options.into_iter().enumerate().map(|(ix, (value, label))| {
            let on_select = on_select.clone();
            div()
                .id((id, ix))
                .test_support()
                .px_2p5()
                .py_0p5()
                .rounded(theme.radius)
                .text_xs()
                .map(|d| {
                    if value == chosen {
                        d.bg(raised).text_color(fg).shadow_xs()
                    } else {
                        d.text_color(muted)
                            .cursor_pointer()
                            .hover(move |s| s.text_color(fg))
                    }
                })
                .child(label)
                .on_click(move |_, window, cx| on_select(&value, window, cx))
        }))
}

/// A form section: its heading in the muted color, with a control at the right if
/// one turns the section on (a switch), then its rows. Sections of a form keep
/// `gap_5` between them, rows `gap_2`.
pub fn form_section(title: &'static str, toggle: Option<AnyElement>, cx: &App) -> Div {
    v_flex().gap_2().child(
        h_flex()
            .h(px(20.))
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(cx.theme().muted_foreground)
                    .child(title),
            )
            .children(toggle),
    )
}

/// A form row: the label at the left (with an (i) if `info` explains it), the
/// control after it.
pub fn form_row(
    label: &'static str,
    info: Option<&'static str>,
    control: impl IntoElement,
    cx: &App,
) -> Div {
    let muted = cx.theme().muted_foreground;
    h_flex()
        .gap_3()
        .items_center()
        .child(
            h_flex()
                .w(px(FORM_LABEL_WIDTH))
                .flex_none()
                .gap_1()
                .items_center()
                .text_sm()
                .child(label)
                .when_some(info, |this, info| {
                    this.child(
                        div()
                            .id(SharedString::from(format!("info-{label}")))
                            .test_support()
                            .child(Icon::new(IconName::Info).xsmall().text_color(muted))
                            .tooltip(wrapped_tooltip(info)),
                    )
                }),
        )
        .child(h_flex().flex_1().min_w_0().gap_2().child(control))
}

/// A few words under a form row's control, in the muted color, lined up with it
/// and closer to it than the next row.
pub fn form_note(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .mt(px(-4.))
        .pl(px(FORM_LABEL_WIDTH + 12.))
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .whitespace_normal()
        .child(text.into())
}

/// A form dialog's Enter: does what its main button does. The dialog's default
/// (`on_ok` unset) only closes it, dropping what was typed. `save` closes the
/// dialog itself once it succeeds, so a form with an error stays open.
pub fn enter_saves(
    save: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) -> bool + 'static {
    move |event, window, cx| {
        save(event, window, cx);
        false
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
