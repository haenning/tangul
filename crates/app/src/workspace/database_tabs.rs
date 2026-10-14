//! Tabs belong to their database: showing another database shows its tabs, and
//! the previous database's tabs wait (queries in them run on) until it is shown
//! again. Tabs that belong to no database (settings, files, AI settings) go along.
//! Pinned query and board tabs are kept per database in `local.db` and come back
//! the first time the database is shown in a run.

use std::collections::HashMap;
use std::time::Duration;

use tangul_core::pinned::PinnedTab;

use super::*;

/// Pause after an edit in a pinned tab before it is saved.
const SAVE_AFTER: Duration = Duration::from_secs(1);

pub(super) struct DatabaseTabs {
    /// The tabs of the databases not shown now, by database id (`None`: none).
    parked: HashMap<Option<String>, Tabs>,
    save: Task<()>,
}

impl Default for DatabaseTabs {
    fn default() -> Self {
        Self {
            parked: HashMap::new(),
            save: Task::ready(()),
        }
    }
}

impl Workspace {
    /// `active_db` changed from `old`: its tabs are parked (pinned ones saved) and
    /// the new database's are shown, its pinned ones restored the first time.
    pub(super) fn database_shown(
        &mut self,
        old: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if old == self.active_db {
            return;
        }
        self.save_pinned_tabs_of(old.as_deref(), &self.tabs, cx);
        let (unbound, unbound_active) = self.tabs.take_unbound(cx);
        let next = self.active_db.clone();
        let first_visit = !self.db_tabs.parked.contains_key(&next);
        let shown = self.db_tabs.parked.remove(&next).unwrap_or_default();
        let parked = std::mem::replace(&mut self.tabs, shown);
        self.db_tabs.parked.insert(old, parked);
        if first_visit && let Some(db) = next {
            self.restore_pinned_tabs(&db, window, cx);
        }
        self.tabs
            .adopt(unbound, unbound_active || self.tabs.is_empty());
        if let Some(tab) = self.tabs.active().cloned() {
            tab.focus(window, cx);
        }
        cx.notify();
    }

    /// Drops database `db`'s waiting tabs (it was removed) and doesn't save its
    /// pinned ones any more.
    pub(super) fn forget_database_tabs(&mut self, db: &str) -> Option<Tabs> {
        self.db_tabs.save = Task::ready(());
        self.db_tabs.parked.remove(&Some(db.to_string()))
    }

    /// Opens the tabs pinned for `db` when it was last shown, pinned, in order.
    /// Boards that are gone are skipped; a query whose user is gone takes the
    /// active one.
    fn restore_pinned_tabs(&mut self, db: &str, window: &mut Window, cx: &mut Context<Self>) {
        let pinned = self.data.local.pinned_tabs(db);
        let first = self.tabs.items().count();
        for tab in pinned {
            match tab {
                PinnedTab::Query {
                    title,
                    sql,
                    user_id,
                    bookmark_id,
                    view,
                } => {
                    let tab = self.add_tab(&sql, window, cx);
                    let user = user_id.filter(|u| {
                        self.find_user(u).is_some_and(|(d, _)| d.id == db)
                            && self.active_user.as_deref() != Some(u.as_str())
                    });
                    if let Some(conn) = user.and_then(|u| self.tab_conn(&u)) {
                        tab.update(cx, |t, cx| t.set_conn(conn, cx));
                    }
                    let bookmark = bookmark_id.filter(|id| self.data.local.bookmark(id).is_some());
                    tab.update(cx, |t, cx| {
                        t.title = title.into();
                        t.bookmark_id = bookmark;
                        t.set_chart_view(view, cx);
                    });
                }
                PinnedTab::Board { board_id } => {
                    let here = self
                        .data
                        .boards
                        .get(&board_id)
                        .is_some_and(|b| b.database_id == db);
                    if !here {
                        continue;
                    }
                    self.open_board(&board_id, window, cx);
                }
            }
            let ix = self.tabs.active_index();
            self.tabs.pin(ix);
        }
        self.tabs.set_active(first);
    }

    /// Saves the shown database's pinned tabs now.
    pub(super) fn save_pinned_tabs(&mut self, cx: &mut Context<Self>) {
        self.db_tabs.save = Task::ready(());
        self.save_pinned_tabs_of(self.active_db.as_deref(), &self.tabs, cx);
    }

    /// Saves the shown database's pinned tabs after a pause (while typing).
    pub(super) fn save_pinned_tabs_soon(&mut self, cx: &mut Context<Self>) {
        self.db_tabs.save = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_AFTER).await;
            let _ = this.update(cx, |ws, cx| ws.save_pinned_tabs(cx));
        });
    }

    fn save_pinned_tabs_of(&self, db: Option<&str>, tabs: &Tabs, cx: &App) {
        let Some(db) = db else {
            return;
        };
        let pinned: Vec<PinnedTab> = tabs
            .pinned()
            .filter_map(|tab| self.pinned_form(tab, cx))
            .collect();
        self.data.local.set_pinned_tabs(db, &pinned);
    }

    /// How `tab` is kept between runs; `None` for tabs that aren't kept.
    fn pinned_form(&self, tab: &TabItem, cx: &App) -> Option<PinnedTab> {
        match tab {
            TabItem::Query(t) => {
                let t = t.read(cx);
                let user_id = t
                    .conn
                    .as_ref()
                    .and_then(|c| self.connections.tab_user(c.read(cx).id))
                    .map(str::to_string);
                Some(PinnedTab::Query {
                    title: t.title.to_string(),
                    // Passwords (`CREATE ROLE … PASSWORD '…'`) are never kept.
                    sql: tangul_assist::redact::redact(&t.sql(cx)).into_owned(),
                    user_id,
                    bookmark_id: t.bookmark_id.clone(),
                    view: t.chart_view(cx),
                })
            }
            TabItem::Board(b) => Some(PinnedTab::Board {
                board_id: b.read(cx).board().id.clone(),
            }),
            TabItem::File(_) | TabItem::AiSettings(_) => None,
        }
    }

    /// Every query tab, of the shown database and the others.
    pub(super) fn all_query_tabs(&self) -> Vec<Entity<QueryTab>> {
        self.tabs
            .queries()
            .chain(self.db_tabs.parked.values().flat_map(Tabs::queries))
            .cloned()
            .collect()
    }
}
