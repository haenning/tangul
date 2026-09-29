//! The sidebars left and right of the content: which panel each shows, their width,
//! showing and hiding them; and the panels' own state. Both sides are the same
//! [`Sidebar`], each with its own list of panels.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Side {
    Left,
    Right,
}

/// One sidebar: the panels it hosts and which one shows. Knows nothing about the
/// panels' content, so moving a panel to the other side is a change to the lists.
pub(super) struct Sidebar {
    /// What it hosts, in order (status bar buttons, palette cycling).
    pub(super) panels: &'static [Panel],
    pub(super) open: bool,
    /// `None` only while it hosts nothing.
    pub(super) active: Option<Panel>,
    pub(super) width: f32,
    /// local.db setting the width is kept in.
    width_setting: &'static str,
}

impl Sidebar {
    pub(super) fn new(
        panels: &'static [Panel],
        open: bool,
        width_setting: &'static str,
        local: &Local,
    ) -> Self {
        let width = local
            .setting(width_setting)
            .and_then(|w| w.parse::<f32>().ok())
            .unwrap_or(SIDEBAR_WIDTH)
            .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
        Self {
            panels,
            open: open && !panels.is_empty(),
            active: panels.first().copied(),
            width,
            width_setting,
        }
    }

    pub(super) fn hosts(&self, panel: Panel) -> bool {
        self.panels.contains(&panel)
    }

    /// Whether `panel` is the one showing.
    pub(super) fn shows(&self, panel: Panel) -> bool {
        self.open && self.active == Some(panel)
    }

    pub(super) fn show(&mut self, panel: Panel) {
        debug_assert!(self.hosts(panel));
        self.open = true;
        self.active = Some(panel);
    }

    /// Shows `panel`, or hides the sidebar if it is the one already shown (like
    /// Zed's dock buttons). Whether it shows now.
    pub(super) fn toggle(&mut self, panel: Panel) -> bool {
        if self.shows(panel) {
            self.open = false;
        } else {
            self.show(panel);
        }
        self.open
    }

    /// Opens a closed sidebar on its last panel, else moves `step` panels on,
    /// wrapping around. The panel shown, if it hosts any.
    pub(super) fn cycle(&mut self, step: isize) -> Option<Panel> {
        let active = self.active?;
        let panel = if self.open {
            let n = self.panels.len() as isize;
            let at = self.panels.iter().position(|p| *p == active).unwrap_or(0) as isize;
            self.panels[(at + step).rem_euclid(n) as usize]
        } else {
            active
        };
        self.show(panel);
        Some(panel)
    }

    pub(super) fn toggle_open(&mut self) {
        self.open = !self.open && self.active.is_some();
    }

    /// Keeps a width the user dragged to (clamped) and saves it.
    fn set_width(&mut self, width: f32, local: &Local) {
        self.width = width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
        local.set_setting(self.width_setting, &self.width.to_string());
    }
}

/// The panels' own state, whichever sidebar shows them.
pub(super) struct Panels {
    pub(super) value: Entity<ValuePanel>,
    /// The value viewer with its header (key context "ValueViewer": Esc closes it).
    pub(super) value_focus: FocusHandle,
    pub(super) schema: SchemaTree,
    pub(super) bookmark_search: Entity<InputState>,
    pub(super) history: HistoryList,
}

/// The schema panel: the active connection's tree, search and favorites.
pub(super) struct SchemaTree {
    pub(super) search: Entity<InputState>,
    pub(super) scroll: UniformListScrollHandle,
    /// A search hit to show in the tree on the next render: schema, table.
    pub(super) reveal: Option<(SharedString, Option<String>)>,
    /// The row last revealed, highlighted until the next search.
    pub(super) revealed: Option<(SharedString, Option<String>)>,
    /// Schemas opened or closed by hand: (user id, schema) → open. Unset, the tree
    /// opens until there is a choice: the only schema, a schema's only group.
    pub(super) open_schemas: HashMap<(String, SharedString), bool>,
    /// Groups opened or closed by hand: (user id, schema, kind) → open.
    pub(super) open_groups: HashMap<(String, SharedString, TableKind), bool>,
    /// The tree as rendered (see [`SidebarRow`]).
    pub(super) rows: Vec<SidebarRow>,
    /// Favorite tables and views of the active database as (schema, name).
    pub(super) favorites: Vec<(String, String)>,
}

impl SchemaTree {
    pub(super) fn new(search: Entity<InputState>) -> Self {
        Self {
            search,
            scroll: UniformListScrollHandle::new(),
            reveal: None,
            revealed: None,
            open_schemas: HashMap::new(),
            open_groups: HashMap::new(),
            rows: Vec::new(),
            favorites: Vec::new(),
        }
    }
}

/// The history panel: the search and what it found.
pub(super) struct HistoryList {
    pub(super) search: Entity<InputState>,
    /// Whose runs are shown; none chosen: everyone's.
    pub(super) sources: Vec<Source>,
    pub(super) entries: Vec<HistoryEntry>,
}

/// A panel a sidebar can show.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Panel {
    Schema,
    Bookmarks,
    History,
    Value,
}

impl Panel {
    /// The left sidebar's panels.
    pub(super) const LEFT: &'static [Panel] = &[Panel::Schema, Panel::Bookmarks, Panel::History];
    /// The right sidebar's panels (the agent, #37, goes here too).
    pub(super) const RIGHT: &'static [Panel] = &[Panel::Value];

    pub(super) fn icon(self) -> Lucide {
        match self {
            Panel::Schema => Lucide::ListTree,
            Panel::Bookmarks => Lucide::Bookmark,
            Panel::History => Lucide::RotateCcwClock,
            Panel::Value => Lucide::Braces,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Panel::Schema => "Schema",
            Panel::Bookmarks => "Bookmarks",
            Panel::History => "History",
            Panel::Value => "Value viewer",
        }
    }
}

/// Flattened sidebar entry of the active connection (schema → tables/views), so the
/// tree renders through a virtualized list.
#[derive(Clone)]
pub(super) enum SidebarRow {
    Schema {
        schema: SharedString,
        expanded: bool,
    },
    Group {
        schema: SharedString,
        kind: TableKind,
        count: usize,
        expanded: bool,
    },
    Table {
        table: TableInfo,
    },
    /// Above the tree: a favorite table or view.
    Favorite {
        table: TableInfo,
    },
    /// While searching: a group title, then the hits.
    Heading(SharedString),
    SchemaHit {
        schema: SharedString,
    },
    TableHit {
        table: TableInfo,
    },
    ColumnHit {
        table: TableInfo,
        column: SharedString,
        data_type: SharedString,
    },
}

impl Workspace {
    pub(super) fn sidebar(&self, side: Side) -> &Sidebar {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    /// The sidebar that hosts `panel`.
    fn host(&mut self, panel: Panel) -> &mut Sidebar {
        if self.right.hosts(panel) {
            &mut self.right
        } else {
            &mut self.left
        }
    }

    pub(super) fn save_sidebar_width(&mut self, side: Side, width: f32) {
        let local = &self.data.local;
        match side {
            Side::Left => self.left.set_width(width, local),
            Side::Right => self.right.set_width(width, local),
        }
    }

    /// The sidebar on `side` with the panel it shows.
    pub(super) fn render_sidebar(
        &mut self,
        side: Side,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let body = match self.sidebar(side).active {
            Some(Panel::Schema) => self.render_schema_panel(cx).into_any_element(),
            Some(Panel::Bookmarks) => self.render_bookmarks_panel(cx).into_any_element(),
            Some(Panel::History) => self.render_history_panel(cx).into_any_element(),
            Some(Panel::Value) => self.render_value_viewer(cx).into_any_element(),
            None => div().into_any_element(),
        };
        let theme = cx.theme();
        // The divider next to it (the resize handle) draws the border.
        v_flex()
            .id(("sidebar", side as usize))
            .test_support()
            .size_full()
            .overflow_hidden()
            .bg(theme.sidebar)
            .child(body)
    }

    /// The value viewer under a header with a close button.
    fn render_value_viewer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .id("value-viewer")
            .test_support()
            .key_context("ValueViewer")
            .track_focus(&self.panels.value_focus)
            .size_full()
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
            .child(div().flex_1().min_h_0().child(self.panels.value.clone()))
    }

    /// Esc in the grid or the viewer, or its close button: closes the viewer and
    /// returns to the grid. With the viewer closed, the grid's own Esc runs.
    pub(super) fn close_value_viewer(
        &mut self,
        _: &CloseValueViewer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let host = self.host(Panel::Value);
        if !host.shows(Panel::Value) {
            cx.propagate();
            return;
        }
        host.open = false;
        if let Some(tab) = self.tabs.active().and_then(TabItem::query) {
            let grid = tab.read(cx).results_focus(cx);
            window.focus(&grid, cx);
        }
        cx.notify();
    }

    /// Status bar button: shows that panel, or hides its sidebar if it is the one
    /// already shown.
    pub(super) fn toggle_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if self.host(panel).toggle(panel) {
            self.panel_shown(panel, cx);
        }
        cx.notify();
    }

    pub(super) fn show_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        self.host(panel).show(panel);
        self.panel_shown(panel, cx);
        cx.notify();
    }

    /// What a panel needs when it comes into view.
    fn panel_shown(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if panel == Panel::History {
            self.refresh_history(cx);
        }
    }

    /// Palette Tab / Shift-Tab: opens a closed left sidebar, else moves to its next
    /// or previous panel.
    pub(super) fn cycle_panel(&mut self, step: isize, cx: &mut Context<Self>) {
        if let Some(panel) = self.left.cycle(step) {
            self.panel_shown(panel, cx);
        }
        cx.notify();
    }

    pub(super) fn toggle_sidebar(
        &mut self,
        _: &ToggleSidebar,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.left.toggle_open();
        cx.notify();
    }

    pub(super) fn toggle_right_sidebar(
        &mut self,
        _: &ToggleRightSidebar,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.right.toggle_open();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{Panel, Sidebar};

    fn left() -> Sidebar {
        Sidebar {
            panels: Panel::LEFT,
            open: true,
            active: Some(Panel::Schema),
            width: 260.,
            width_setting: "test",
        }
    }

    #[test]
    fn toggle_hides_the_panel_shown() {
        let mut s = left();
        assert!(!s.toggle(Panel::Schema));
        assert!(!s.open);
        // Its button opens it again, on that panel.
        assert!(s.toggle(Panel::Schema));
        assert!(s.shows(Panel::Schema));
        // Another panel's button switches.
        assert!(s.toggle(Panel::History));
        assert!(s.shows(Panel::History) && !s.shows(Panel::Schema));
    }

    #[test]
    fn cycle_wraps_within_its_panels() {
        let mut s = left();
        assert_eq!(s.cycle(1), Some(Panel::Bookmarks));
        assert_eq!(s.cycle(1), Some(Panel::History));
        assert_eq!(s.cycle(1), Some(Panel::Schema));
        assert_eq!(s.cycle(-1), Some(Panel::History));
        // Closed: opens on the panel it had.
        s.open = false;
        assert_eq!(s.cycle(1), Some(Panel::History));
        assert!(s.open);
    }

    #[test]
    fn a_sidebar_without_panels_stays_closed() {
        let mut s = Sidebar {
            panels: &[],
            open: false,
            active: None,
            width: 260.,
            width_setting: "test",
        };
        s.toggle_open();
        assert!(!s.open);
        assert_eq!(s.cycle(1), None);
        assert!(!s.hosts(Panel::Schema));
    }
}
