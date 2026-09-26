//! The main area's tabs: what kinds there are ([`TabItem`]) and the open ones
//! ([`Tabs`]): order, the active one, pinning. Opening, closing and running belong
//! to the workspace.

use gpui_kit::*;
use tangul_core::TabId;

use crate::file_tab::FileTab;
use crate::query_tab::QueryTab;

/// What a tab shows.
#[derive(Clone, PartialEq)]
pub enum TabItem {
    Query(Entity<QueryTab>),
    File(Entity<FileTab>),
    // Boards (#97) become a kind of their own here, `Board(Entity<BoardTab>)`: a
    // grid of `ResultView`s, one per query.
}

impl TabItem {
    pub fn id(&self, cx: &App) -> TabId {
        match self {
            TabItem::Query(t) => t.read(cx).id,
            TabItem::File(t) => t.read(cx).id,
        }
    }

    pub fn title(&self, cx: &App) -> SharedString {
        match self {
            TabItem::Query(t) => t.read(cx).title.clone(),
            TabItem::File(t) => t.read(cx).title.clone(),
        }
    }

    pub fn query(&self) -> Option<&Entity<QueryTab>> {
        match self {
            TabItem::Query(t) => Some(t),
            TabItem::File(_) => None,
        }
    }

    pub fn file(&self) -> Option<&Entity<FileTab>> {
        match self {
            TabItem::File(t) => Some(t),
            TabItem::Query(_) => None,
        }
    }

    /// Changes that would be lost on closing (asks first).
    pub fn has_unsaved_changes(&self, cx: &App) -> bool {
        match self {
            TabItem::Query(_) => false,
            TabItem::File(t) => t.read(cx).has_unsaved_changes(cx),
        }
    }

    /// Marked with a dot in the tab bar: unsaved, or a transaction is open.
    pub fn is_dirty(&self, cx: &App) -> bool {
        match self {
            TabItem::Query(t) => t.read(cx).in_transaction(),
            TabItem::File(t) => t.read(cx).has_unsaved_changes(cx),
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        match self {
            TabItem::Query(t) => QueryTab::focus_editor(t, window, cx),
            TabItem::File(t) => FileTab::focus(t, window, cx),
        }
    }

    pub fn view(&self) -> AnyView {
        match self {
            TabItem::Query(t) => t.clone().into(),
            TabItem::File(t) => t.clone().into(),
        }
    }
}

struct OpenTab {
    item: TabItem,
    /// No close button, and middle-click doesn't close it; the menu and the shortcut
    /// still do.
    pinned: bool,
    /// The workspace's subscription to the tab's events; dropped with the tab.
    _events: Option<Subscription>,
}

/// The open tabs, left to right.
#[derive(Default)]
pub struct Tabs {
    open: Vec<OpenTab>,
    active: usize,
    /// Tab a middle-button press started on; it closes if released on the same tab.
    middle_press: Option<TabId>,
}

impl Tabs {
    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    pub fn get(&self, ix: usize) -> Option<&TabItem> {
        self.open.get(ix).map(|t| &t.item)
    }

    pub fn items(&self) -> impl Iterator<Item = &TabItem> {
        self.open.iter().map(|t| &t.item)
    }

    pub fn queries(&self) -> impl Iterator<Item = &Entity<QueryTab>> {
        self.items().filter_map(TabItem::query)
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active(&self) -> Option<&TabItem> {
        self.get(self.active)
    }

    pub fn active_query(&self) -> Option<&Entity<QueryTab>> {
        self.active().and_then(TabItem::query)
    }

    pub fn set_active(&mut self, ix: usize) {
        if ix < self.open.len() {
            self.active = ix;
        }
    }

    /// Adds `item` at the end and makes it the active tab. `events` is the
    /// workspace's subscription to it, kept as long as the tab is open.
    pub fn push(&mut self, item: TabItem, events: Option<Subscription>) {
        self.open.push(OpenTab {
            item,
            pinned: false,
            _events: events,
        });
        self.active = self.open.len() - 1;
    }

    /// Removes the tab at `ix`; the active one stays, or the one left of it.
    pub fn remove(&mut self, ix: usize) -> Option<TabItem> {
        if ix >= self.open.len() {
            return None;
        }
        let tab = self.open.remove(ix);
        if ix < self.active || self.active >= self.open.len() {
            self.active = self.active.saturating_sub(1);
        }
        Some(tab.item)
    }

    pub fn index_of(&self, id: TabId, cx: &App) -> Option<usize> {
        self.open.iter().position(|t| t.item.id(cx) == id)
    }

    pub fn is_pinned(&self, ix: usize) -> bool {
        self.open.get(ix).is_some_and(|t| t.pinned)
    }

    pub fn toggle_pin(&mut self, ix: usize) {
        if let Some(t) = self.open.get_mut(ix) {
            t.pinned = !t.pinned;
        }
    }

    pub fn middle_pressed(&mut self, id: TabId) {
        self.middle_press = Some(id);
    }

    /// Whether a middle-button release on `id` completes a press on the same tab.
    pub fn middle_released(&mut self, id: TabId) -> bool {
        self.middle_press.take() == Some(id)
    }
}
