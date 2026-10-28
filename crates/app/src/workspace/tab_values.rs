//! The value viewer of each query tab: below the tab bar, beside the tab's editor
//! and result. A tab keeps its own A, B and whether its viewer is open for as long
//! as it is open; other tabs don't show it.

use super::*;

/// local.db setting the viewer's width is kept in.
const VALUE_VIEWER_WIDTH_SETTING: &str = "value_viewer_width";
const VALUE_VIEWER_WIDTH: f32 = 420.;
const VALUE_VIEWER_MIN_WIDTH: f32 = 240.;
const VALUE_VIEWER_MAX_WIDTH: f32 = 900.;

/// One query tab's value viewer.
pub(super) struct TabValues {
    pub(super) panel: Entity<ValuePanel>,
    pub(super) open: bool,
    /// The viewer with its header (key context "ValueViewer": Esc closes it).
    pub(super) focus: FocusHandle,
    _events: Subscription,
}

/// The width every tab's viewer has, and the split it sits in.
pub(super) struct ValueSplit {
    pub(super) state: Entity<ResizableState>,
    pub(super) width: f32,
}

impl ValueSplit {
    pub(super) fn new(local: &Local, cx: &mut App) -> Self {
        let width = local
            .setting(VALUE_VIEWER_WIDTH_SETTING)
            .and_then(|w| w.parse::<f32>().ok())
            .unwrap_or(VALUE_VIEWER_WIDTH)
            .clamp(VALUE_VIEWER_MIN_WIDTH, VALUE_VIEWER_MAX_WIDTH);
        Self {
            state: cx.new(|_| ResizableState::default()),
            width,
        }
    }
}

impl Workspace {
    /// The viewer of query tab `id`, made the first time it is asked for.
    pub(super) fn tab_values(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> &mut TabValues {
        self.values.entry(id).or_insert_with(|| {
            let panel = cx.new(|cx| ValuePanel::new(window, cx));
            let events = cx.subscribe_in(&panel, window, Self::on_value_event);
            TabValues {
                panel,
                open: false,
                focus: cx.focus_handle(),
                _events: events,
            }
        })
    }

    /// The active tab's viewer, if it is a query tab that has one.
    pub(super) fn active_values(&self, cx: &App) -> Option<&TabValues> {
        let tab = self.tabs.active_query()?;
        self.values.get(&tab.read(cx).id)
    }

    /// Whether the active tab shows its viewer.
    pub(super) fn value_viewer_open(&self, cx: &App) -> bool {
        self.active_values(cx).is_some_and(|v| v.open)
    }

    /// Opens tab `id`'s viewer.
    pub(super) fn open_value_viewer(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tab_values(id, window, cx).open = true;
        cx.notify();
    }

    /// The status bar button: opens or closes the active query tab's viewer.
    pub(super) fn toggle_value_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.tabs.active_query().map(|t| t.read(cx).id) else {
            return;
        };
        if self.value_viewer_open(cx) {
            self.close_value_viewer(&CloseValueViewer, window, cx);
        } else {
            self.open_value_viewer(id, window, cx);
        }
    }

    /// Esc in the grid or the viewer, or its close button: closes the active tab's
    /// viewer and returns to the grid. With the viewer closed, the grid's own Esc runs.
    pub(super) fn close_value_viewer(
        &mut self,
        _: &CloseValueViewer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.tabs.active_query().map(|t| t.read(cx).id) else {
            cx.propagate();
            return;
        };
        match self.values.get_mut(&id).filter(|v| v.open) {
            Some(values) => values.open = false,
            None => {
                cx.propagate();
                return;
            }
        }
        self.focus_grid(window, cx);
        cx.notify();
    }

    /// Back to the active tab's grid (the viewer closed, maybe with the focus in it).
    fn focus_grid(&self, window: &mut Window, cx: &mut App) {
        if let Some(tab) = self.tabs.active().and_then(TabItem::query) {
            let grid = tab.read(cx).results_focus(cx);
            window.focus(&grid, cx);
        }
    }

    fn save_value_viewer_width(&mut self, width: f32) {
        self.value_split.width = width.clamp(VALUE_VIEWER_MIN_WIDTH, VALUE_VIEWER_MAX_WIDTH);
        self.data.local.set_setting(
            VALUE_VIEWER_WIDTH_SETTING,
            &self.value_split.width.to_string(),
        );
    }

    /// The active tab (`tab`) with its viewer beside it, when that is open.
    pub(super) fn render_tab_with_values(
        &mut self,
        tab: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(values) = self.active_values(cx).filter(|v| v.open) else {
            return tab;
        };
        let viewer = self.render_value_viewer(values, cx);
        let this = cx.entity().downgrade();
        h_resizable("value-split")
            .with_state(&self.value_split.state)
            .on_resize(move |state, _, cx| {
                if let Some(width) = state.read(cx).sizes().last().copied() {
                    let _ = this.update(cx, |ws, _| ws.save_value_viewer_width(width.as_f32()));
                }
            })
            .child(resizable_panel().child(tab))
            .child(
                resizable_panel()
                    .size(px(self.value_split.width))
                    .size_range(px(VALUE_VIEWER_MIN_WIDTH)..px(VALUE_VIEWER_MAX_WIDTH))
                    .flex_none()
                    .child(viewer),
            )
            .into_any_element()
    }

    /// The value viewer under a header with a close button.
    fn render_value_viewer(&self, values: &TabValues, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        v_flex()
            .id("value-viewer")
            .test_support()
            .key_context("ValueViewer")
            .track_focus(&values.focus)
            // Space closes it here too; the viewer's (read-only) editor would take
            // it as text before any key binding.
            .capture_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                let k = &e.keystroke;
                if k.key == "space" && !k.modifiers.modified() {
                    cx.stop_propagation();
                    this.close_value_viewer(&CloseValueViewer, window, cx);
                }
            }))
            .size_full()
            .bg(theme.sidebar)
            .child(
                h_flex()
                    .id("value-viewer-header")
                    .test_support()
                    .h_9()
                    .pl_3()
                    .pr_1()
                    .items_center()
                    .justify_between()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .tooltip(|window, cx| {
                        Tooltip::new("Space opens and closes the viewer").build(window, cx)
                    })
                    .child("VALUE")
                    .child(
                        Button::new("close-value-viewer")
                            .icon(IconName::Close)
                            .xsmall()
                            .ghost()
                            .tooltip("Close (Esc)")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_value_viewer(&CloseValueViewer, window, cx)
                            })),
                    ),
            )
            .child(div().flex_1().min_h_0().child(values.panel.clone()))
            .into_any_element()
    }
}
