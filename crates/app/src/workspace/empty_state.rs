//! The content area without tabs.

use super::*;

impl Workspace {
    /// The content area without tabs: an engraved wordmark and what to do next.
    pub(super) fn render_empty_state(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        // Engraved: a faint fill between a dark edge above and a light edge below.
        let crate::theme::Engraving { fill, shade, light } = crate::theme::engraving(cx);
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
        let version = div()
            .id("empty-version")
            .test_support()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(version_label());
        let connected = self.active_conn(cx).is_some_and(Connection::is_connected);
        let (hover, muted) = (theme.accent, theme.muted_foreground);
        let action = |id: &'static str, icon: Icon, label: &'static str, keys: &'static str| {
            h_flex()
                .id(id)
                .test_support()
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
            .child(
                v_flex()
                    .items_center()
                    .gap_1()
                    .child(wordmark)
                    .child(version),
            )
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
                        .child(
                            action(
                                "empty-new-board",
                                Icon::new(Lucide::LayoutDashboard),
                                "New board",
                                "",
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.new_board(window, cx)),
                            ),
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

/// The app's version as built, the one `Info.plist` gets; a debug build says so,
/// so it isn't taken for a release.
fn version_label() -> String {
    let version = env!("CARGO_PKG_VERSION");
    if cfg!(debug_assertions) {
        format!("{version} dev")
    } else {
        version.to_string()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_version_is_the_crate_version() {
        let label = super::version_label();
        assert!(label.starts_with(env!("CARGO_PKG_VERSION")));
        assert_eq!(label.ends_with(" dev"), cfg!(debug_assertions));
    }
}
