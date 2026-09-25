//! The content area without tabs.

use super::*;

impl Workspace {
    /// The content area without tabs: an engraved wordmark and what to do next.
    pub(super) fn render_empty_state(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        // Engraved: a faint fill between a dark edge above and a light edge below.
        let (fill, shade, light) = if theme.mode.is_dark() {
            (
                white().opacity(0.05),
                black().opacity(0.9),
                white().opacity(0.09),
            )
        } else {
            (
                black().opacity(0.08),
                black().opacity(0.12),
                white().opacity(0.95),
            )
        };
        let word = |color: Hsla| {
            div()
                .text_size(px(72.))
                .font_weight(FontWeight::BOLD)
                .text_color(color)
                .child("tangul")
        };
        let wordmark = div()
            .relative()
            .child(div().absolute().top(px(-1.5)).left_0().child(word(shade)))
            .child(div().absolute().top(px(1.5)).left_0().child(word(light)))
            .child(word(fill));
        let connected = self
            .active_conn()
            .is_some_and(|c| c.status == ConnStatus::Connected);
        let (hover, muted) = (theme.accent, theme.muted_foreground);
        let action = |id: &'static str, icon: Icon, label: &'static str, keys: &'static str| {
            h_flex()
                .id(id)
                .w(px(280.))
                .px_3()
                .py_1p5()
                .gap_2()
                .items_center()
                .rounded(theme.radius)
                .text_sm()
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .child(icon.small().text_color(muted))
                .child(div().flex_1().child(label))
                .child(div().text_xs().text_color(muted).child(keys))
        };
        // The top bar's colour: a shade lighter than the editor in dark mode.
        v_flex()
            .flex_1()
            .size_full()
            .bg(theme.title_bar)
            .items_center()
            .justify_center()
            .gap_8()
            .child(wordmark)
            .child(
                v_flex()
                    .gap_0p5()
                    .when(connected, |this| {
                        this.child(
                            action("empty-new-tab", Icon::new(IconName::Plus), "New tab", "⌘T")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.add_tab("", window, cx);
                                })),
                        )
                    })
                    .child(
                        action(
                            "empty-new-db",
                            Icon::new(Lucide::Database),
                            "New database connection",
                            "",
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_form(FormMode::NewDatabase, None, None, window, cx)
                        })),
                    )
                    .child(
                        action(
                            "empty-settings",
                            Icon::new(IconName::Settings),
                            "Settings",
                            "⌘,",
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_settings(window, cx)),
                        ),
                    ),
            )
            .into_any_element()
    }
}
