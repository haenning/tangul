//! The sidebar frame: which panel shows, its width, showing and hiding it.

use super::*;

impl Workspace {
    pub(super) fn save_sidebar_width(&mut self, width: f32) {
        self.sidebar_width = width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
        self.data
            .local
            .set_setting(SIDEBAR_WIDTH_SETTING, &self.sidebar_width.to_string());
    }

    pub(super) fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.sidebar_view;
        let body = match view {
            SidebarView::Schema => self.render_schema_panel(cx).into_any_element(),
            SidebarView::Bookmarks => self.render_bookmarks_panel(cx).into_any_element(),
            SidebarView::History => self.render_history_panel(cx).into_any_element(),
        };
        let theme = cx.theme();
        // The divider next to it (the resize handle) draws the border.
        v_flex()
            .size_full()
            .overflow_hidden()
            .bg(theme.sidebar)
            .child(body)
    }

    /// Bottom-bar panel button: shows that panel, or hides the sidebar if it is the
    /// one already shown (like Zed's dock buttons).
    pub(super) fn toggle_panel(&mut self, view: SidebarView, cx: &mut Context<Self>) {
        if self.sidebar_open && self.sidebar_view == view {
            self.sidebar_open = false;
            cx.notify();
        } else {
            self.show_panel(view, cx);
        }
    }

    pub(super) fn show_panel(&mut self, view: SidebarView, cx: &mut Context<Self>) {
        self.sidebar_open = true;
        self.sidebar_view = view;
        if view == SidebarView::History {
            self.refresh_history(cx);
        }
        cx.notify();
    }

    /// Palette Tab / Shift-Tab: opens a closed sidebar, else moves to the next or
    /// previous panel.
    pub(super) fn cycle_panel(&mut self, step: isize, cx: &mut Context<Self>) {
        let view = if self.sidebar_open {
            self.sidebar_view.cycle(step)
        } else {
            self.sidebar_view
        };
        self.show_panel(view, cx);
    }

    pub(super) fn toggle_sidebar(
        &mut self,
        _: &ToggleSidebar,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }
}
