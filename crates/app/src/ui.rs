//! Small UI pieces shared by views: text in tooltips and cards that wraps instead
//! of running across the window, warnings and errors as cards, error messages to
//! select and copy, and menu rows with a title and a short description.

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Textarea, TextareaState},
    menu::PopupMenuItem,
    switch::Switch,
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

/// How often a running time is redrawn while it counts up.
const COUNT_UP_EVERY: std::time::Duration = std::time::Duration::from_millis(50);

/// Redraws the view every [`COUNT_UP_EVERY`] as long as `running` says so: a time
/// counting up while something runs. Ends by itself; dropping the task ends it too.
pub fn count_up<T: 'static>(cx: &mut Context<T>, running: fn(&T) -> bool) -> Task<()> {
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(COUNT_UP_EVERY).await;
            let more = this.update(cx, |this, cx| {
                let more = running(this);
                if more {
                    cx.notify();
                }
                more
            });
            if !matches!(more, Ok(true)) {
                break;
            }
        }
    })
}

/// A time as the status bar shows it: `12 ms`.
pub fn millis(time: std::time::Duration) -> SharedString {
    format!("{} ms", time.as_millis()).into()
}

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

/// How long "Copied" shows after an [`ErrorText`] was copied.
const COPIED_FOR: std::time::Duration = std::time::Duration::from_millis(1500);

/// An error message as the server sent it, to select and copy: drag to select part
/// of it (double click a word, triple click a line, ⌘/Ctrl+C), a click without a
/// drag or the Copy button copies all of it. The one way error messages are shown
/// where the user may want to pass them on.
pub struct ErrorText {
    /// The message; shown in `state`, which only takes it while drawing (it needs
    /// the window).
    text: SharedString,
    state: Entity<TextareaState>,
    /// "Copied" shows until this runs out.
    copied: Option<Task<()>>,
}

impl ErrorText {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx).auto_grow(1, 12);
            state.set_readonly(true, cx);
            state
        });
        Self {
            text: SharedString::default(),
            state,
            copied: None,
        }
    }

    /// Shows `text` (from the next frame on).
    pub fn set_text(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let text = text.into();
        if text != self.text {
            self.text = text;
            self.copied = None;
            cx.notify();
        }
    }

    /// The part the user selected; empty when nothing is.
    pub fn selected(&self, cx: &App) -> SharedString {
        self.state.read(cx).selected_value()
    }

    /// Puts the whole message on the clipboard and says so for a moment.
    pub fn copy(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.text.to_string()));
        self.copied = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FOR).await;
            let _ = this.update(cx, |this, cx| {
                this.copied = None;
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl Render for ErrorText {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.state.read(cx).value() != self.text {
            let text = self.text.clone();
            self.state.update(cx, |s, cx| s.set_value(text, window, cx));
        }
        let theme = cx.theme();
        let copied = self.copied.is_some();
        h_flex()
            .id("error-text")
            .test_support()
            .w_full()
            .items_start()
            .gap_2()
            .child(
                div()
                    .id("error-message")
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    // A click copies everything; a drag selects instead (the
                    // selection is there when the button comes up).
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            if this.selected(cx).is_empty() {
                                this.copy(cx);
                            }
                        }),
                    )
                    .child(
                        Textarea::new(&self.state)
                            .appearance(false)
                            .font_family(theme.mono_font_family.clone())
                            .text_sm()
                            .text_color(theme.danger),
                    ),
            )
            .child(
                Button::new("copy-error")
                    .icon(if copied {
                        IconName::Check
                    } else {
                        IconName::Copy
                    })
                    .label(if copied { "Copied" } else { "Copy" })
                    .xsmall()
                    .ghost()
                    .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
            )
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

/// An on/off switch, on as its off color tinted with a fifth of the accent: calm,
/// yet clearly on.
pub fn switch(id: impl Into<ElementId>, cx: &App) -> Switch {
    let theme = cx.theme();
    Switch::new(id).color(theme.switch.blend(theme.primary.opacity(0.2)))
}

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

/// A form row: the label at the left (with an (i) if `explained`), the control
/// after it.
pub fn form_row(
    label: &'static str,
    explained: Option<&'static str>,
    control: impl IntoElement,
    cx: &App,
) -> Div {
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
                .when_some(explained, |this, text| {
                    this.child(info(SharedString::from(format!("info-{label}")), text, cx))
                }),
        )
        .child(h_flex().flex_1().min_w_0().gap_2().child(control))
}

/// An (i) that explains `text` on hover: longer explanations, never paragraphs.
pub fn info(id: impl Into<ElementId>, text: impl Into<SharedString>, cx: &App) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .flex_none()
        .child(
            Icon::new(IconName::Info)
                .xsmall()
                .text_color(cx.theme().muted_foreground),
        )
        .tooltip(wrapped_tooltip(text))
        .into_any_element()
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

/// Buttons over the right end of a row, shown while `group` is hovered: they take
/// no room from the name the rest of the time. `bg` is the hovered row's
/// background, so they cover the name's end cleanly. The row must be `.relative()`
/// with `.group(group)` and a right padding of `px_2`.
pub fn hover_actions(group: SharedString, bg: Hsla) -> Div {
    h_flex()
        .absolute()
        .top_0()
        .bottom_0()
        // The row's right padding (`pr_2`).
        .right(px(8.))
        .pl_1()
        .gap_0p5()
        .items_center()
        .bg(bg)
        .invisible()
        .group_hover(group, |s| s.visible())
}

/// A form as a dialog's body. The dialog clips its body at its edges, right under
/// the title and above the footer, which cut off the focus ring of a first or last
/// text field; a little room keeps it whole.
pub fn dialog_form(form: impl IntoElement) -> Div {
    div().py(px(6.)).child(form)
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
