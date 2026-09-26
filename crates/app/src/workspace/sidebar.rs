//! The sidebar (left dock): which panel shows, its width, showing and hiding it,
//! and each panel's state.

use super::*;

/// The left dock and its panels' state.
pub(super) struct Sidebar {
    pub(super) open: bool,
    pub(super) view: SidebarView,
    pub(super) width: f32,
    /// Sidebar/content split; the sidebar is the first panel.
    pub(super) layout: Entity<ResizableState>,
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
    pub(super) entries: Vec<HistoryEntry>,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum SidebarView {
    Schema,
    Bookmarks,
    History,
}

impl SidebarView {
    pub(super) const ALL: [SidebarView; 3] = [
        SidebarView::Schema,
        SidebarView::Bookmarks,
        SidebarView::History,
    ];

    /// The panel `step` places after this one, wrapping around.
    pub(super) fn cycle(self, step: isize) -> Self {
        let n = Self::ALL.len() as isize;
        Self::ALL[(self as isize + step).rem_euclid(n) as usize]
    }

    pub(super) fn icon(self) -> Lucide {
        match self {
            SidebarView::Schema => Lucide::ListTree,
            SidebarView::Bookmarks => Lucide::Bookmark,
            SidebarView::History => Lucide::RotateCcwClock,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            SidebarView::Schema => "Schema",
            SidebarView::Bookmarks => "Bookmarks",
            SidebarView::History => "History",
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
    pub(super) fn save_sidebar_width(&mut self, width: f32) {
        self.sidebar.width = width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
        self.data
            .local
            .set_setting(SIDEBAR_WIDTH_SETTING, &self.sidebar.width.to_string());
    }

    pub(super) fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.sidebar.view;
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
        if self.sidebar.open && self.sidebar.view == view {
            self.sidebar.open = false;
            cx.notify();
        } else {
            self.show_panel(view, cx);
        }
    }

    pub(super) fn show_panel(&mut self, view: SidebarView, cx: &mut Context<Self>) {
        self.sidebar.open = true;
        self.sidebar.view = view;
        if view == SidebarView::History {
            self.refresh_history(cx);
        }
        cx.notify();
    }

    /// Palette Tab / Shift-Tab: opens a closed sidebar, else moves to the next or
    /// previous panel.
    pub(super) fn cycle_panel(&mut self, step: isize, cx: &mut Context<Self>) {
        let view = if self.sidebar.open {
            self.sidebar.view.cycle(step)
        } else {
            self.sidebar.view
        };
        self.show_panel(view, cx);
    }

    pub(super) fn toggle_sidebar(
        &mut self,
        _: &ToggleSidebar,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar.open = !self.sidebar.open;
        cx.notify();
    }
}
