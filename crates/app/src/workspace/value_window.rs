//! The value viewer popped out into a window of its own, for a second screen. It
//! shows the active query tab's viewer and follows the tabs; while it is open no tab
//! shows its viewer inline. Minify, or closing the window, hands it back to the tab.
//! Its editors belong to its own window, so the state moves between it and the tabs'
//! viewers ([`ValueState`]).

use std::cell::RefCell;

use super::*;
use value_panel::ValueState;

/// The pop-out, as the workspace keeps it.
pub(super) struct ValueWindow {
    pub(super) handle: WindowHandle<Root>,
    pub(super) panel: Entity<ValuePanel>,
    /// The tab whose viewer it shows; meanwhile that tab's state is here.
    pub(super) tab: Option<TabId>,
    _events: Subscription,
}

/// The pop-out window: a title bar like the main window's, the viewer's header with
/// Minify, the viewer.
pub(super) struct ValueWindowView {
    workspace: WeakEntity<Workspace>,
    panel: Entity<ValuePanel>,
    focus: FocusHandle,
}

impl ValueWindowView {
    /// Closes the window and hands the viewer back to the tab.
    fn minify(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.remove_window();
        let _ = self
            .workspace
            .update(cx, |ws, cx| ws.value_window_closed(true, cx));
    }
}

impl Render for ValueWindowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .panel
            .read(cx)
            .source()
            .unwrap_or_else(|| "Value viewer".into());
        let theme = cx.theme();
        v_flex()
            .id("value-window")
            .test_support()
            .key_context("ValueViewer")
            .track_focus(&self.focus)
            // Esc, and Space like in the tab, hand the viewer back.
            .on_action(
                cx.listener(|this, _: &CloseValueViewer, window, cx| this.minify(window, cx)),
            )
            .capture_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                let k = &e.keystroke;
                if k.key == "space" && !k.modifiers.modified() {
                    cx.stop_propagation();
                    this.minify(window, cx);
                }
            }))
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.foreground)
            .child(
                TitleBar::new()
                    .when(cfg!(target_os = "macos") && window.is_fullscreen(), |bar| {
                        bar.pl_0()
                    })
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(title),
                    ),
            )
            .child(
                h_flex()
                    .id("value-window-header")
                    .test_support()
                    .h_9()
                    .pl_3()
                    .pr_1()
                    .items_center()
                    .justify_between()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("VALUE")
                    .child(
                        Button::new("minify-value-window")
                            .icon(Icon::new(Lucide::Minimize2))
                            .xsmall()
                            .ghost()
                            .tooltip("Back into the tab (Esc)")
                            .on_click(cx.listener(|this, _, window, cx| this.minify(window, cx))),
                    ),
            )
            .child(div().flex_1().min_h_0().child(self.panel.clone()))
    }
}

impl Workspace {
    /// The viewer's header button: moves the active tab's viewer into a window of
    /// its own, or brings that window to the front when it is open already.
    pub(super) fn pop_out_value_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.raise_value_window(cx) {
            return;
        }
        let tab = self.tabs.active_query().map(|t| t.read(cx).id);
        let state = match tab {
            Some(id) => self.tab_values(id, window, cx).panel.read(cx).state(),
            None => ValueState::default(),
        };
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("tangul · value".into()),
                ..TitleBar::title_bar_options()
            }),
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(640.), px(720.)),
                cx,
            ))),
            ..Default::default()
        };
        let workspace = cx.entity().downgrade();
        let made = Rc::new(RefCell::new(None));
        let opened = cx.open_window(options, {
            let made = made.clone();
            move |window, cx| {
                let panel = cx.new(|cx| {
                    let mut panel = ValuePanel::new(window, cx);
                    panel.set_state(state, cx);
                    panel
                });
                // Its own close control does what Minify does.
                window.on_window_should_close(cx, {
                    let workspace = workspace.clone();
                    move |_, cx| {
                        let _ = workspace.update(cx, |ws, cx| ws.value_window_closed(true, cx));
                        true
                    }
                });
                *made.borrow_mut() = Some(panel.clone());
                let view = cx.new(|cx| ValueWindowView {
                    workspace,
                    panel,
                    focus: cx.focus_handle(),
                });
                cx.new(|cx| Root::new(view, window, cx))
            }
        });
        let (Ok(handle), Some(panel)) = (opened, made.borrow_mut().take()) else {
            window.push_notification(
                Notification::error("Could not open the value viewer's window"),
                cx,
            );
            return;
        };
        let events = cx.subscribe_in(&panel, window, Self::on_value_event);
        self.value_window = Some(ValueWindow {
            handle,
            panel,
            tab,
            _events: events,
        });
        cx.notify();
    }

    /// Brings the pop-out to the front (and to its screen). False without one.
    pub(super) fn raise_value_window(&self, cx: &mut App) -> bool {
        let Some(handle) = self.value_window.as_ref().map(|v| v.handle) else {
            return false;
        };
        handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    }

    /// The pop-out is gone (Minify or its close control): its state goes back to its
    /// tab, which shows the viewer again (`reopen`).
    pub(super) fn value_window_closed(&mut self, reopen: bool, cx: &mut Context<Self>) {
        let Some(popped) = self.value_window.take() else {
            return;
        };
        let state = popped.panel.read(cx).state();
        if let Some(values) = popped.tab.and_then(|id| self.values.get_mut(&id)) {
            values.panel.update(cx, |p, cx| p.set_state(state, cx));
            values.open |= reopen;
        }
        cx.notify();
    }

    /// The pop-out shows the active tab's viewer: after a tab switch the old tab
    /// gets its state back and the new one's moves in.
    pub(super) fn follow_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active = self.tabs.active_query().map(|t| t.read(cx).id);
        let Some(popped) = self.value_window.as_ref().filter(|v| v.tab != active) else {
            return;
        };
        let (old, panel) = (popped.tab, popped.panel.clone());
        let state = panel.read(cx).state();
        if let Some(values) = old.and_then(|id| self.values.get(&id)) {
            values.panel.update(cx, |p, cx| p.set_state(state, cx));
        }
        let new = match active {
            Some(id) => self.tab_values(id, window, cx).panel.read(cx).state(),
            None => ValueState::default(),
        };
        panel.update(cx, |p, cx| p.set_state(new, cx));
        if let Some(popped) = self.value_window.as_mut() {
            popped.tab = active;
        }
    }

    /// A cell picked in tab `id` as A (or B, `compared`): into the pop-out while it
    /// shows that tab, else into the tab's viewer. The tab A (or B) was in before,
    /// if it was another one.
    pub(super) fn pick_value(
        &mut self,
        id: TabId,
        picked: Picked,
        compared: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<TabId> {
        let set = move |p: &mut ValuePanel, window: &mut Window, cx: &mut Context<ValuePanel>| {
            if compared {
                p.set_b(picked, window, cx)
            } else {
                p.set_a(picked, window, cx)
            }
        };
        match self.value_window.as_ref().filter(|v| v.tab == Some(id)) {
            Some(popped) => {
                let panel = popped.panel.clone();
                popped
                    .handle
                    .update(cx, |_, window, cx| {
                        panel.update(cx, |p, cx| set(p, window, cx))
                    })
                    .ok()
                    .flatten()
            }
            None => {
                let panel = self.tab_values(id, window, cx).panel.clone();
                panel.update(cx, |p, cx| set(p, window, cx))
            }
        }
    }
}
