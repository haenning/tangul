//! The main area's tabs: what kinds there are ([`TabItem`]) and the open ones
//! ([`Tabs`]): order, the active one, pinning. Query and board tabs belong to a
//! database (the workspace keeps one `Tabs` per database); the others go along
//! with whichever database is shown. Opening, closing and running belong to the
//! workspace.

use gpui_kit::*;
use tangul_core::TabId;

use crate::ai_settings::AiSettingsTab;
use crate::board::BoardTab;
use crate::file_tab::FileTab;
use crate::query_tab::QueryTab;

/// What a tab shows.
#[derive(Clone, PartialEq)]
pub enum TabItem {
    Query(Entity<QueryTab>),
    File(Entity<FileTab>),
    /// The AI settings (assistant and MCP); one at a time.
    AiSettings(Entity<AiSettingsTab>),
    /// A board: its tiles' results, refreshed together; one tab per board.
    Board(Entity<BoardTab>),
}

impl TabItem {
    pub fn id(&self, cx: &App) -> TabId {
        match self {
            TabItem::Query(t) => t.read(cx).id,
            TabItem::File(t) => t.read(cx).id,
            TabItem::AiSettings(t) => t.read(cx).id,
            TabItem::Board(t) => t.read(cx).id,
        }
    }

    pub fn title(&self, cx: &App) -> SharedString {
        match self {
            TabItem::Query(t) => t.read(cx).title.clone(),
            TabItem::File(t) => t.read(cx).title.clone(),
            TabItem::AiSettings(t) => t.read(cx).title.clone(),
            TabItem::Board(t) => t.read(cx).title.clone(),
        }
    }

    pub fn query(&self) -> Option<&Entity<QueryTab>> {
        match self {
            TabItem::Query(t) => Some(t),
            _ => None,
        }
    }

    pub fn file(&self) -> Option<&Entity<FileTab>> {
        match self {
            TabItem::File(t) => Some(t),
            _ => None,
        }
    }

    pub fn ai_settings(&self) -> Option<&Entity<AiSettingsTab>> {
        match self {
            TabItem::AiSettings(t) => Some(t),
            _ => None,
        }
    }

    pub fn board(&self) -> Option<&Entity<BoardTab>> {
        match self {
            TabItem::Board(t) => Some(t),
            _ => None,
        }
    }

    /// Belongs to a database (query and board tabs); the others go along when
    /// another database is shown, and are never kept between runs.
    pub fn of_database(&self) -> bool {
        matches!(self, TabItem::Query(_) | TabItem::Board(_))
    }

    /// Changes that would be lost on closing (asks first).
    pub fn has_unsaved_changes(&self, cx: &App) -> bool {
        match self {
            TabItem::Query(_) | TabItem::AiSettings(_) | TabItem::Board(_) => false,
            TabItem::File(t) => t.read(cx).has_unsaved_changes(cx),
        }
    }

    /// Marked with a dot in the tab bar: unsaved (a board tile's too), or a
    /// transaction is open.
    pub fn is_dirty(&self, cx: &App) -> bool {
        match self {
            TabItem::Query(t) => {
                let t = t.read(cx);
                t.in_transaction() || t.tile_changed(cx)
            }
            TabItem::File(t) => t.read(cx).has_unsaved_changes(cx),
            TabItem::AiSettings(_) | TabItem::Board(_) => false,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        match self {
            TabItem::Query(t) => QueryTab::focus_editor(t, window, cx),
            TabItem::File(t) => FileTab::focus(t, window, cx),
            TabItem::AiSettings(t) => AiSettingsTab::focus(t, window, cx),
            TabItem::Board(t) => BoardTab::focus(t, window, cx),
        }
    }

    pub fn view(&self) -> AnyView {
        match self {
            TabItem::Query(t) => t.clone().into(),
            TabItem::File(t) => t.clone().into(),
            TabItem::AiSettings(t) => t.clone().into(),
            TabItem::Board(t) => t.clone().into(),
        }
    }
}

/// A tab that belongs to no database, on its way to another database's tabs.
pub struct UnboundTab(OpenTab);

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

    pub fn pin(&mut self, ix: usize) {
        if let Some(t) = self.open.get_mut(ix) {
            t.pinned = true;
        }
    }

    /// The pinned tabs, in order.
    pub fn pinned(&self) -> impl Iterator<Item = &TabItem> {
        self.open.iter().filter(|t| t.pinned).map(|t| &t.item)
    }

    /// Takes out the tabs that belong to no database, to go along to another
    /// database's tabs ([`Tabs::adopt`]); whether the active tab was one of them.
    pub fn take_unbound(&mut self, cx: &App) -> (Vec<UnboundTab>, bool) {
        let active_unbound = self.active().is_some_and(|t| !t.of_database());
        let active_id = self.active().map(|t| t.id(cx));
        let (unbound, bound): (Vec<_>, Vec<_>) = std::mem::take(&mut self.open)
            .into_iter()
            .partition(|t| !t.item.of_database());
        self.open = bound;
        self.active = active_id
            .and_then(|id| self.open.iter().position(|t| t.item.id(cx) == id))
            .unwrap_or(0);
        (
            unbound.into_iter().map(UnboundTab).collect(),
            active_unbound,
        )
    }

    /// Adds `tabs` (taken from another database's tabs) at the end; with
    /// `activate`, the first of them becomes the active tab.
    pub fn adopt(&mut self, tabs: Vec<UnboundTab>, activate: bool) {
        let first = self.open.len();
        let any = !tabs.is_empty();
        self.open.extend(tabs.into_iter().map(|t| t.0));
        if activate && any {
            self.active = first;
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
