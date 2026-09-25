//! Root view: title bar (database + user switchers, like Zed's project/branch),
//! schema sidebar of the active connection, query tabs, status bar.
//! Routes engine events to the tab that started the query.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::MouseButton;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Root, Selectable as _, Sizable as _,
    TitleBar, WindowExt as _,
    button::{Button, ButtonVariants as _},
    command::{Command as PaletteList, CommandGroup, CommandItem, CommandState},
    h_flex,
    hover_card::HoverCard,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
    notification::Notification,
    resizable::{ResizableState, h_resizable, resizable_panel},
    spinner::Spinner,
    tab::{Tab, TabBar},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_assist::{
    chart::{ChartData, ChartStyle, ChartView},
    columns::column_facts,
    edit::row_edit,
    guard,
    lexer::fingerprint,
    related::{rank_links, related_links},
    schema::{SchemaIndex, quote_ident},
    suggest::{QuerySuggestion, one_line, suggest_for_table},
    usage::{Usage, learn},
};
use tangul_core::{
    ColumnMeta, ConnId, Database, DbUser, DriverKind, QueryId, SchemaInfo, TabId, TableInfo,
    TableKind,
};
use tangul_engine::{Command, EngineHandle, Event, ExecEvent, PoolStats, QueryEvent};

use crate::{
    CloseTab, NewTab, NextSidebarPanel, OpenPalette, OpenSettings, PrevSidebarPanel, ToggleSidebar,
    assets::Lucide,
    bookmark_form::BookmarkForm,
    completion::{AssistData, SharedAssist, SqlCompletion},
    connection_form::{ConnectionForm, FormMode, FormOutput},
    local::{Bookmark, HistoryEntry, Local, NewHistory, bare_sql},
    metrics::{self, Metrics},
    monitor::{self, Monitor},
    palette::{self, PaletteAction, PaletteKind},
    query_tab::{QueryTab, TabConn, TabEvent},
    settings,
    store::Store,
};

const SIDEBAR_WIDTH: f32 = 260.;
const SIDEBAR_MIN_WIDTH: f32 = 180.;
const SIDEBAR_MAX_WIDTH: f32 = 600.;
const ROW_HEIGHT: f32 = 26.;
/// Sidebar tree geometry: left padding, indent per level, icon width.
const TREE_PAD: f32 = 10.;
const TREE_INDENT: f32 = 18.;
const TREE_ICON: f32 = 14.;
const BOOKMARK_ROW_HEIGHT: f32 = 40.;
const HISTORY_LIMIT: usize = 300;
/// Successful runs after which the toolbar suggests bookmarking a query.
const BOOKMARK_HINT_RUNS: u32 = 3;
/// local.db setting: the user id connected last, reopened at startup.
const LAST_USER_SETTING: &str = "last_user";
const SIDEBAR_WIDTH_SETTING: &str = "sidebar_width";

#[derive(Clone, Copy, PartialEq)]
enum SidebarView {
    Schema,
    Bookmarks,
    History,
}

impl SidebarView {
    const ALL: [SidebarView; 3] = [
        SidebarView::Schema,
        SidebarView::Bookmarks,
        SidebarView::History,
    ];

    /// The panel `step` places after this one, wrapping around.
    fn cycle(self, step: isize) -> Self {
        let n = Self::ALL.len() as isize;
        Self::ALL[(self as isize + step).rem_euclid(n) as usize]
    }

    fn icon(self) -> Lucide {
        match self {
            SidebarView::Schema => Lucide::ListTree,
            SidebarView::Bookmarks => Lucide::Bookmark,
            SidebarView::History => Lucide::RotateCcwClock,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SidebarView::Schema => "Schema",
            SidebarView::Bookmarks => "Bookmarks",
            SidebarView::History => "History",
        }
    }
}

/// A query sent to the engine, kept until it finishes to record it in history.
/// Why a statement is held back before it runs.
enum RunGuard {
    /// Read-only mode: nothing changes until the user allows writes.
    ReadOnly {
        user: String,
        /// (keyword, statement on one line)
        statements: Vec<(String, String)>,
    },
    /// `UPDATE` / `DELETE` without `WHERE`.
    Unfiltered { statements: Vec<(String, String)> },
}

struct RunningQuery {
    tab: WeakEntity<QueryTab>,
    sql: String,
    user_id: String,
    /// Unix milliseconds.
    started_at: i64,
    /// A filtered, sorted or further page of an earlier result: not a run of its
    /// own (no history, usage or bookmark hint).
    derived: bool,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[derive(Clone, PartialEq)]
enum ConnStatus {
    Connecting,
    Connected,
    Failed(SharedString),
}

struct ConnState {
    id: ConnId,
    status: ConnStatus,
    /// Tables, columns and foreign keys (from the cache until the server answered).
    schema: Arc<SchemaIndex>,
    stats: PoolStats,
}

/// Flattened sidebar entry of the active connection (schema → tables/views), so the
/// tree renders through a virtualized list.
#[derive(Clone)]
enum SidebarRow {
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

pub struct Workspace {
    engine: EngineHandle,
    store: Store,
    local: Local,
    /// Schema and usage per connection, shared with the editors' completion.
    assist: SharedAssist,
    /// Learned usage by database id.
    usage: HashMap<String, Arc<Usage>>,
    history: Vec<HistoryEntry>,
    history_search: Entity<InputState>,
    bookmark_search: Entity<InputState>,
    schema_search: Entity<InputState>,
    sidebar_scroll: UniformListScrollHandle,
    /// A search hit to show in the tree on the next render: schema, table.
    reveal: Option<(SharedString, Option<String>)>,
    /// The row last revealed, highlighted until the next search.
    revealed: Option<(SharedString, Option<String>)>,
    /// Connection state by user id.
    conns: HashMap<String, ConnState>,
    conn_users: HashMap<ConnId, String>,
    /// Selected database and user; the sidebar shows this connection.
    active_db: Option<String>,
    active_user: Option<String>,
    /// Schemas opened or closed by hand: (user id, schema) → open. Unset, the tree
    /// opens until there is a choice: the only schema, a schema's only group.
    schema_open: HashMap<(String, SharedString), bool>,
    /// Groups opened or closed by hand: (user id, schema, kind) → open.
    group_open: HashMap<(String, SharedString, TableKind), bool>,
    sidebar_view: SidebarView,
    sidebar_open: bool,
    /// Sidebar/content split; the sidebar is the first panel.
    layout: Entity<ResizableState>,
    sidebar_width: f32,
    tabs: Vec<Entity<QueryTab>>,
    active_tab: usize,
    /// Tab a middle-button press started on; it closes if released on the same tab.
    middle_press: Option<TabId>,
    queries: HashMap<QueryId, RunningQuery>,
    /// Pending editor checks: request → (tab, edit generation).
    validations: HashMap<u64, (WeakEntity<QueryTab>, u64)>,
    sidebar_rows: Vec<SidebarRow>,
    /// Favorite tables and views of the active database as (schema, name).
    favorites: Vec<(String, String)>,
    /// Pending returns to read-only mode (`safety.relock_after`) by database user
    /// name; replaced to restart the wait, dropped to cancel it.
    relock: HashMap<String, Task<()>>,
    /// Duration and rows of bookmark runs.
    metrics: Metrics,
    /// Scheduled bookmark runs on hidden connections.
    monitor: Monitor,
    /// Read-only connection to `metrics.db` for query tabs, and whether it is up.
    metrics_conn: Option<(ConnId, bool)>,
    /// Tabs to run once `metrics_conn` is up.
    metrics_waiting: Vec<WeakEntity<QueryTab>>,
    _monitor_tick: Task<()>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
    _pump: Task<()>,
}

impl Workspace {
    pub fn new(
        engine: EngineHandle,
        events: async_channel::Receiver<Event>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Engine → UI: drain everything that is queued per wake-up so a stream of row
        // batches results in one re-render, not one per batch.
        let pump = cx.spawn_in(window, async move |this, cx| {
            while let Ok(first) = events.recv().await {
                let mut batch = vec![first];
                while let Ok(ev) = events.try_recv() {
                    batch.push(ev);
                }
                let alive = this.update_in(cx, |this, window, cx| {
                    for ev in batch {
                        this.on_engine_event(ev, window, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });

        let store = Store::load();
        let local = Local::open(store.dir());
        let metrics = Metrics::open(store.dir());
        metrics.prune(&settings::current(cx).monitoring, now_ms());
        let monitor_tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(monitor::TICK).await;
                if this.update(cx, |ws, cx| ws.monitor_tick(cx)).is_err() {
                    break;
                }
            }
        });
        let history_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search history"));
        let search_sub = cx.subscribe(
            &history_search,
            |this: &mut Self, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.refresh_history(cx);
                }
            },
        );
        let bookmark_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search bookmarks"));
        let schema_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let redraw = |this: &mut Self, _, ev: &InputEvent, cx: &mut Context<Self>| {
            if matches!(ev, InputEvent::Change) {
                this.revealed = None;
                cx.notify();
            }
        };
        let bookmark_sub = cx.subscribe(&bookmark_search, redraw);
        let schema_sub = cx.subscribe(&schema_search, redraw);
        let sidebar_width = local
            .setting(SIDEBAR_WIDTH_SETTING)
            .and_then(|w| w.parse::<f32>().ok())
            .unwrap_or(SIDEBAR_WIDTH)
            .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
        let mut this = Self {
            engine,
            store,
            local,
            assist: Rc::new(RefCell::new(HashMap::new())),
            usage: HashMap::new(),
            history: Vec::new(),
            history_search,
            bookmark_search,
            schema_search,
            sidebar_scroll: UniformListScrollHandle::new(),
            reveal: None,
            revealed: None,
            conns: HashMap::new(),
            conn_users: HashMap::new(),
            active_db: None,
            active_user: None,
            schema_open: HashMap::new(),
            group_open: HashMap::new(),
            sidebar_view: SidebarView::Schema,
            sidebar_open: true,
            layout: cx.new(|_| ResizableState::default()),
            sidebar_width,
            tabs: Vec::new(),
            active_tab: 0,
            middle_press: None,
            queries: HashMap::new(),
            validations: HashMap::new(),
            sidebar_rows: Vec::new(),
            favorites: Vec::new(),
            relock: HashMap::new(),
            metrics,
            monitor: Monitor::default(),
            metrics_conn: None,
            metrics_waiting: Vec::new(),
            _monitor_tick: monitor_tick,
            focus_handle: cx.focus_handle(),
            _subscriptions: vec![search_sub, bookmark_sub, schema_sub],
            _pump: pump,
        };
        // Reopen the connection used last (else the first one) right away.
        let last = this.local.setting(LAST_USER_SETTING);
        let start = last
            .as_deref()
            .and_then(|id| this.find_user(id))
            .map(|(db, u)| (db.id.clone(), Some(u.id.clone())))
            .or_else(|| {
                let db = this.store.databases.first()?;
                Some((db.id.clone(), db.users.first().map(|u| u.id.clone())))
            });
        if let Some((db_id, user_id)) = start {
            this.active_db = Some(db_id);
            if let Some(user_id) = user_id {
                this.connect(&user_id, cx);
                this.active_user = Some(user_id);
            }
            this.refresh_history(cx);
        }
        // Starts on the empty state; keys like ⌘T need a focused element.
        window.focus(&this.focus_handle, cx);
        this
    }

    /// Ids are unique for the whole app run, not per window, so a reopened window
    /// never collides with sessions or queries of a closed one.
    fn next_id(&mut self) -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    fn active_database(&self) -> Option<&Database> {
        self.store.database(self.active_db.as_deref()?)
    }

    fn active_conn(&self) -> Option<&ConnState> {
        self.conns.get(self.active_user.as_deref()?)
    }

    fn find_user(&self, user_id: &str) -> Option<(&Database, &DbUser)> {
        self.store
            .databases
            .iter()
            .find_map(|db| Some((db, db.user(user_id)?)))
    }

    /// The tab connection for a user, if it is connected (or connecting).
    fn tab_conn(&self, user_id: &str) -> Option<TabConn> {
        let (db, user) = self.find_user(user_id)?;
        let state = self.conns.get(user_id)?;
        Some(TabConn {
            id: state.id,
            name: format!("{}@{}", user.user, db.display_name()).into(),
            driver: db.driver,
        })
    }

    // ---- tabs ----------------------------------------------------------------

    fn add_tab(
        &mut self,
        sql: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<QueryTab> {
        let id = TabId(self.next_id());
        let conn = self.active_user.as_deref().and_then(|p| self.tab_conn(p));
        let engine = self.engine.clone();
        let tab = cx.new(|cx| QueryTab::new(id, conn, sql, engine, window, cx));
        let provider = Rc::new(SqlCompletion {
            data: self.assist.clone(),
            tab: tab.downgrade(),
        });
        tab.update(cx, |t, cx| t.set_completion(provider, cx));
        self._subscriptions
            .push(cx.subscribe_in(&tab, window, Self::on_tab_event));
        self.tabs.push(tab.clone());
        self.active_tab = self.tabs.len() - 1;
        QueryTab::focus_editor(&tab, window, cx);
        cx.notify();
        tab
    }

    /// Edits `settings.json` in a tab (the one already open, if any).
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = settings::ensure_file(cx) else {
            window.push_notification(
                Notification::error("Could not create the settings file"),
                cx,
            );
            return;
        };
        let open = self
            .tabs
            .iter()
            .position(|t| t.read(cx).file_path() == Some(path.as_path()));
        let tab = match open {
            Some(ix) => {
                self.active_tab = ix;
                self.tabs[ix].clone()
            }
            None => {
                let id = TabId(self.next_id());
                let engine = self.engine.clone();
                let tab = cx.new(|cx| QueryTab::new_file(id, path, engine, window, cx));
                self._subscriptions
                    .push(cx.subscribe_in(&tab, window, Self::on_tab_event));
                self.tabs.push(tab.clone());
                self.active_tab = self.tabs.len() - 1;
                tab
            }
        };
        QueryTab::focus_editor(&tab, window, cx);
        cx.notify();
    }

    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        self.add_tab("", window, cx);
    }

    fn close_active_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab(self.active_tab, window, cx);
    }

    fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix).cloned() else {
            return;
        };
        if tab.read(cx).has_unsaved_changes(cx) {
            self.ask_to_save(tab, window, cx);
            return;
        }
        self.remove_tab(ix, window, cx);
    }

    /// Closing a file tab with unsaved changes: save, discard or keep the tab.
    fn ask_to_save(&mut self, tab: Entity<QueryTab>, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = cx.entity().downgrade();
        let name = tab.read(cx).title.clone();
        let id = tab.read(cx).id;
        // Closes the tab (if it is still there) once the dialog is answered.
        let close = move |save: bool| {
            let (workspace, tab) = (workspace.clone(), tab.clone());
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                window.close_dialog(cx);
                if save && !tab.update(cx, |t, cx| t.save_file(window, cx)) {
                    return;
                }
                let _ = workspace.update(cx, |ws, cx| {
                    if let Some(ix) = ws.tab_index(id, cx) {
                        ws.remove_tab(ix, window, cx);
                    }
                });
            }
        };
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(format!("Save changes to {name}?"))
                .w(px(420.))
                .child(
                    div()
                        .text_sm()
                        .child("Your changes are lost if you don't save them."),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel-close")
                                .label("Cancel")
                                .ghost()
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("discard-close")
                                .label("Don't save")
                                .danger()
                                .on_click(close(false)),
                        )
                        .child(
                            Button::new("save-close")
                                .label("Save")
                                .primary()
                                .on_click(close(true)),
                        ),
                )
        });
    }

    /// Closes a tab without asking.
    fn remove_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(ix);
        let tab = tab.read(cx);
        if let Some(conn) = &tab.conn {
            // Drops a pinned session (open transaction is rolled back by the server).
            self.engine.send(Command::ReleaseTab {
                tab: tab.id,
                conn: conn.id,
            });
        }
        if self.tabs.is_empty() {
            // The empty state takes over.
            self.active_tab = 0;
            window.focus(&self.focus_handle, cx);
        } else {
            self.active_tab = self.active_tab.min(self.tabs.len() - 1);
        }
        cx.notify();
    }

    fn tab_index(&self, id: TabId, cx: &App) -> Option<usize> {
        self.tabs.iter().position(|t| t.read(cx).id == id)
    }

    fn close_tab_by_id(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.tab_index(id, cx) {
            self.close_tab(ix, window, cx);
        }
    }

    fn toggle_tab_pin(&mut self, id: TabId, cx: &mut Context<Self>) {
        if let Some(ix) = self.tab_index(id, cx) {
            self.tabs[ix].update(cx, |t, _| t.toggle_pin());
            cx.notify();
        }
    }

    /// Removes the tab's bookmark, or asks for a name and bookmarks its query.
    fn toggle_tab_bookmark(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tab_index(id, cx).map(|ix| self.tabs[ix].clone()) else {
            return;
        };
        if let Some(bookmark) = tab.read(cx).bookmark_id.clone() {
            self.delete_bookmark(&bookmark, window, cx);
        } else {
            let sql = tab.read(cx).sql(cx);
            if !sql.trim().is_empty() {
                self.open_bookmark_dialog(None, sql, Some(tab), window, cx);
            }
        }
    }

    fn on_tab_event(
        &mut self,
        tab: &Entity<QueryTab>,
        event: &TabEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TabEvent::Validate {
                generation,
                statements,
            } => {
                let Some(conn) = tab.read(cx).conn.clone() else {
                    return;
                };
                let request = self.next_id();
                self.validations
                    .insert(request, (tab.downgrade(), *generation));
                self.engine.send(Command::Validate {
                    request,
                    conn: conn.id,
                    statements: statements.clone(),
                });
            }
            TabEvent::OpenRelated { title, sql } => {
                // Same connection as the tab the rows came from.
                let conn = tab.read(cx).conn.clone();
                let new = self.add_tab(sql, window, cx);
                new.update(cx, |t, cx| {
                    t.title = title.clone().into();
                    if let Some(conn) = conn {
                        t.set_conn(conn, cx);
                        cx.emit(TabEvent::Run(sql.clone()));
                    }
                });
            }
            TabEvent::OpenSql { title, sql } => {
                let conn = tab.read(cx).conn.clone();
                let new = self.add_tab(sql, window, cx);
                new.update(cx, |t, cx| {
                    t.title = title.clone().into();
                    if let Some(conn) = conn {
                        t.set_conn(conn, cx);
                    }
                });
            }
            TabEvent::Bookmark(sql) => {
                let existing = tab.read(cx).bookmark_id.clone();
                self.open_bookmark_dialog(existing, sql.clone(), Some(tab.clone()), window, cx);
            }
            TabEvent::Refine { sql, append } => self.execute_derived(tab, sql, *append, cx),
            TabEvent::OpenTable { schema, table } => {
                // On the tab's connection, like related rows.
                let Some(state) = tab
                    .read(cx)
                    .conn
                    .as_ref()
                    .and_then(|c| self.conn_users.get(&c.id))
                    .and_then(|user| self.conns.get(user))
                else {
                    return;
                };
                let Some(ix) = state.schema.resolve(Some(schema), table) else {
                    return;
                };
                let event = TabEvent::OpenRelated {
                    title: table.clone(),
                    sql: format!("SELECT * FROM {} LIMIT 100", state.schema.sql_name(ix)),
                };
                self.on_tab_event(tab, &event, window, cx);
            }
            TabEvent::Run(sql) => {
                let Some(conn) = tab.read(cx).conn.clone() else {
                    return;
                };
                match self.run_guard(conn.id, sql, cx) {
                    None => self.execute(tab, sql, cx),
                    Some(guard) => self.hold_back(tab.clone(), sql.clone(), guard, window, cx),
                }
            }
        }
    }

    /// Sends `sql` to the engine on the tab's connection.
    fn execute(&mut self, tab: &Entity<QueryTab>, sql: &str, cx: &mut Context<Self>) {
        let Some(conn) = tab.read(cx).conn.clone() else {
            return;
        };
        let query = QueryId(self.next_id());
        let user_id = self.conn_users.get(&conn.id).cloned().unwrap_or_default();
        // Running a bookmarked query: the tab shows the bookmark's name, and its
        // bookmark button edits that bookmark. Only when the whole editor ran, since
        // the button saves the whole editor.
        let whole_editor = bare_sql(&tab.read(cx).sql(cx)) == bare_sql(sql);
        let bookmark = self
            .find_user(&user_id)
            .filter(|_| whole_editor)
            .and_then(|(db, _)| self.local.bookmark_for_sql(&db.id, sql));
        if let Some(bookmark) = bookmark
            && tab.read(cx).bookmark_id.as_deref() != Some(bookmark.id.as_str())
        {
            tab.update(cx, |t, _| {
                t.title = bookmark.name.into();
                t.bookmark_id = Some(bookmark.id);
            });
        }
        self.queries.insert(
            query,
            RunningQuery {
                tab: tab.downgrade(),
                sql: sql.to_string(),
                user_id,
                started_at: now_ms(),
                derived: false,
            },
        );
        // The server refuses writes, too, so read-only mode can't be sidestepped.
        let user = self.conn_user_name(conn.id);
        let read_only = user
            .as_ref()
            .is_some_and(|user| settings::current(cx).is_read_only(user));
        // A write while read-only mode is lifted restarts the wait until it is back.
        if let Some(user) = user.filter(|u| self.relock.contains_key(u))
            && guard::check(sql).iter().any(|c| c.writes)
        {
            self.schedule_relock(&user, cx);
        }
        self.engine.send(Command::Execute {
            query,
            tab: tab.read(cx).id,
            conn: conn.id,
            sql: sql.to_string(),
            read_only,
            timeout: settings::current(cx).safety.max_query_time,
        });
        tab.update(cx, |t, cx| t.started(query, sql, cx));
    }

    /// Runs a query derived from the tab's last result (filtered, sorted, next
    /// page). It only reads, so it skips the safety checks of a user run.
    fn execute_derived(
        &mut self,
        tab: &Entity<QueryTab>,
        sql: &str,
        append: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = tab.read(cx).conn.clone() else {
            return;
        };
        let query = QueryId(self.next_id());
        self.queries.insert(
            query,
            RunningQuery {
                tab: tab.downgrade(),
                sql: sql.to_string(),
                user_id: self.conn_users.get(&conn.id).cloned().unwrap_or_default(),
                started_at: now_ms(),
                derived: true,
            },
        );
        let read_only = self
            .conn_user_name(conn.id)
            .is_some_and(|user| settings::current(cx).is_read_only(&user));
        self.engine.send(Command::Execute {
            query,
            tab: tab.read(cx).id,
            conn: conn.id,
            sql: sql.to_string(),
            read_only,
            timeout: settings::current(cx).safety.max_query_time,
        });
        tab.update(cx, |t, cx| t.started_derived(query, append, cx));
    }

    // ---- safety ------------------------------------------------------------------

    /// The database user name of a connection.
    fn conn_user_name(&self, conn: ConnId) -> Option<String> {
        let user_id = self.conn_users.get(&conn)?;
        Some(self.find_user(user_id)?.1.user.clone())
    }

    /// Whether the active user is in read-only mode.
    fn active_user_read_only(&self, cx: &App) -> Option<bool> {
        let user = &self.find_user(self.active_user.as_deref()?)?.1.user;
        Some(settings::current(cx).is_read_only(user))
    }

    /// Flips read-only mode for the active user (kept in `settings.json`).
    fn toggle_read_only(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(user) = self
            .active_user
            .as_deref()
            .and_then(|id| self.find_user(id))
            .map(|(_, u)| u.user.clone())
        else {
            return;
        };
        let on = !settings::current(cx).is_read_only(&user);
        match settings::set_read_only(&user, on, cx) {
            Ok(()) if on => {
                self.relock.remove(&user);
            }
            Ok(()) => self.schedule_relock(&user, cx),
            Err(e) => window.push_notification(
                Notification::error(format!("Could not change read-only mode: {e}")),
                cx,
            ),
        }
        cx.notify();
    }

    /// Turns read-only mode back on for `user` after `safety.relock_after`, if set.
    fn schedule_relock(&mut self, user: &str, cx: &mut Context<Self>) {
        let Some(after) = settings::current(cx).safety.relock_after else {
            self.relock.remove(user);
            return;
        };
        let name = user.to_string();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(after).await;
            let _ = this.update(cx, |ws, cx| ws.relock(&name, cx));
        });
        self.relock.insert(user.to_string(), task);
    }

    fn relock(&mut self, user: &str, cx: &mut Context<Self>) {
        self.relock.remove(user);
        if settings::current(cx).is_read_only(user) {
            return;
        }
        if let Err(e) = settings::set_read_only(user, true, cx) {
            eprintln!("could not turn read-only mode back on for {user}: {e}");
        }
        cx.notify();
    }

    /// Why `sql` shouldn't run on `conn` right away: it changes data in read-only mode
    /// (which takes precedence), or it is an `UPDATE` / `DELETE` without a `WHERE`.
    fn run_guard(&self, conn: ConnId, sql: &str, cx: &App) -> Option<RunGuard> {
        let settings = settings::current(cx);
        let user = self
            .conn_user_name(conn)
            .filter(|user| settings.is_read_only(user));
        let checks = guard::check(sql);
        let described = |keep: &dyn Fn(&guard::Check) -> bool| -> Vec<(String, String)> {
            checks
                .iter()
                .filter(|c| keep(c))
                .map(|c| (c.keyword.clone(), one_line(&c.statement, 160)))
                .collect()
        };
        if let Some(user) = user {
            let statements = described(&|c| c.writes);
            if !statements.is_empty() {
                return Some(RunGuard::ReadOnly { user, statements });
            }
        }
        if settings.safety.confirm_unfiltered_writes {
            let statements = described(&|c| c.unfiltered);
            if !statements.is_empty() {
                return Some(RunGuard::Unfiltered { statements });
            }
        }
        None
    }

    /// Explains why `sql` did not run. The red button lifts the guard: "Allow writes"
    /// turns read-only mode off (the statement is not run; run it again), "Allow …"
    /// for an unfiltered statement runs it once.
    fn hold_back(
        &mut self,
        tab: Entity<QueryTab>,
        sql: String,
        guard: RunGuard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = cx.entity().downgrade();
        let (title, intro, statements, allow) = match &guard {
            RunGuard::ReadOnly { user, statements } => (
                "Read-only mode",
                format!(
                    "Read-only mode is on for {user}. Nothing was run; these statements change data:"
                ),
                statements.clone(),
                "Allow writes".to_string(),
            ),
            RunGuard::Unfiltered { statements } => {
                let has = |keyword: &str| statements.iter().any(|(k, _)| k == keyword);
                let allow = match (has("DELETE"), has("UPDATE")) {
                    (true, false) => "Allow deletes",
                    (false, true) => "Allow updates",
                    _ => "Allow updates and deletes",
                };
                (
                    "No WHERE clause",
                    "These statements have no WHERE and affect every row:".to_string(),
                    statements.clone(),
                    allow.to_string(),
                )
            }
        };
        let read_only_user = match &guard {
            RunGuard::ReadOnly { user, .. } => Some(user.clone()),
            RunGuard::Unfiltered { .. } => None,
        };
        window.open_dialog(cx, move |dialog, _, cx| {
            let theme = cx.theme();
            let body = v_flex()
                .gap_2()
                .child(div().text_sm().child(intro.clone()))
                .children(statements.iter().map(|(_, statement)| {
                    div()
                        .px_2()
                        .py_1()
                        .rounded(theme.radius)
                        .bg(theme.muted)
                        .font_family(theme.mono_font_family.clone())
                        .text_sm()
                        .child(statement.clone())
                }));
            let allow_click = {
                let (workspace, tab, sql) = (workspace.clone(), tab.clone(), sql.clone());
                let read_only_user = read_only_user.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    window.close_dialog(cx);
                    match &read_only_user {
                        // Lifts read-only mode; running the statement is a second, deliberate step.
                        Some(user) => match settings::set_read_only(user, false, cx) {
                            Ok(()) => {
                                let _ = workspace.update(cx, |ws, cx| ws.schedule_relock(user, cx));
                                window.push_notification(
                                    Notification::info(format!(
                                        "Read-only mode is off for {user}. Run the statement again."
                                    )),
                                    cx,
                                )
                            }
                            Err(e) => window.push_notification(
                                Notification::error(format!(
                                    "Could not change read-only mode: {e}"
                                )),
                                cx,
                            ),
                        },
                        None => {
                            let _ = workspace.update(cx, |ws, cx| ws.execute(&tab, &sql, cx));
                        }
                    }
                }
            };
            dialog.title(title).w(px(520.)).child(body).footer(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("cancel-run")
                            .label("Cancel")
                            .ghost()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("allow-run")
                            .label(allow.clone())
                            .danger()
                            .on_click(allow_click),
                    ),
            )
        });
    }

    // ---- scheduled bookmark runs -------------------------------------------------

    /// Starts the next due scheduled bookmark, if the app is quiet and nothing else
    /// runs. One at a time, on a hidden read-only connection.
    fn monitor_tick(&mut self, cx: &mut Context<Self>) {
        let limits = settings::current(cx).monitoring.clone();
        if self
            .monitor
            .stuck(settings::current(cx).safety.max_query_time)
        {
            self.monitor_finish(0, Some("gave up waiting for the run".into()), cx);
        }
        if self.monitor.pending.is_some() || !self.monitor.is_quiet() || !self.queries.is_empty() {
            return;
        }
        let bookmarks = self.local.scheduled_bookmarks();
        let now = now_ms();
        let Some(bookmark) = monitor::due(
            &bookmarks,
            |id| self.metrics.last_run(id),
            now,
            limits.interval(),
        )
        .cloned() else {
            return;
        };
        let db = bookmark
            .database_id
            .as_deref()
            .and_then(|id| self.store.database(id))
            .cloned();
        // The user last used on the database, else its first one.
        let user = db.as_ref().and_then(|db| {
            let last = self.local.last_user(&db.id);
            db.users
                .iter()
                .find(|u| Some(&u.id) == last.as_ref())
                .or(db.users.first())
                .cloned()
        });
        let (Some(db), Some(user)) = (db, user) else {
            let record = metrics::Run {
                bookmark_id: bookmark.id,
                bookmark_name: bookmark.name,
                started_at: now,
                duration_ms: 0,
                rows: None,
                error: Some("no database user to run it with".into()),
                scheduled: true,
            };
            self.metrics.record(&record, &limits, now);
            return;
        };
        let (conn, ready) = match self.monitor.conn_for(&user.id) {
            Some(conn) => conn,
            None => {
                let id = ConnId(self.next_id());
                self.monitor.connecting(&user.id, id);
                let password = self.store.password(&user.id);
                self.engine.send(Command::Connect {
                    conn: id,
                    driver: db.driver,
                    cfg: tangul_core::ConnectConfig::new(&db, &user, password),
                    ssh: self.store.ssh_target(&db),
                });
                (id, false)
            }
        };
        self.monitor.start(bookmark, conn, now);
        if ready {
            self.monitor_execute(cx);
        }
    }

    /// Sends the pending scheduled run's query, read-only.
    fn monitor_execute(&mut self, cx: &mut Context<Self>) {
        let timeout = settings::current(cx).safety.max_query_time;
        let query = QueryId(self.next_id());
        let Some(pending) = self.monitor.pending.as_mut() else {
            return;
        };
        pending.query = Some(query);
        self.engine.send(Command::Execute {
            query,
            // A tab of its own, so no session of a real tab is used.
            tab: TabId(query.0),
            conn: pending.conn,
            sql: pending.bookmark.sql.clone(),
            read_only: true,
            timeout,
        });
    }

    /// Records the pending scheduled run.
    fn monitor_finish(&mut self, duration_ms: i64, error: Option<String>, cx: &mut Context<Self>) {
        let Some(pending) = self.monitor.pending.take() else {
            return;
        };
        if let Some(query) = pending.query {
            self.engine.send(Command::ReleaseTab {
                tab: TabId(query.0),
                conn: pending.conn,
            });
        }
        let record = metrics::Run {
            bookmark_id: pending.bookmark.id,
            bookmark_name: pending.bookmark.name,
            started_at: pending.started_at,
            duration_ms,
            rows: pending.rows.filter(|_| error.is_none()),
            error,
            scheduled: true,
        };
        self.metrics
            .record(&record, &settings::current(cx).monitoring, now_ms());
    }

    /// Handles events of the hidden connections, scheduled runs and the metrics
    /// connection; returns every other event.
    fn monitor_event(
        &mut self,
        event: Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Event> {
        let pending_query = self.monitor.pending.as_ref().and_then(|p| p.query);
        let metrics_conn = self.metrics_conn.map(|(id, _)| id);
        match event {
            Event::Query { query, event } if Some(query) == pending_query => match event {
                QueryEvent::Exec(ExecEvent::StatementDone { rows }) => {
                    if let Some(p) = self.monitor.pending.as_mut() {
                        p.rows = Some(rows as i64);
                    }
                }
                QueryEvent::Finished { elapsed, error, .. } => {
                    self.monitor_finish(elapsed.as_millis() as i64, error, cx);
                }
                QueryEvent::Exec(_) => {}
            },
            Event::Connected { conn } if self.monitor.owns(conn) => {
                self.monitor.connected(conn);
                let waiting = self.monitor.pending.as_ref();
                if waiting.is_some_and(|p| p.conn == conn && p.query.is_none()) {
                    self.monitor_execute(cx);
                }
            }
            Event::ConnectFailed { conn, error } if self.monitor.owns(conn) => {
                self.monitor.failed(conn);
                self.monitor_finish(0, Some(error), cx);
            }
            Event::HostKeyUnknown { conn, .. } if self.monitor.owns(conn) => {
                self.monitor.failed(conn);
                let error = "unknown SSH host key: connect once to trust it".to_string();
                self.monitor_finish(0, Some(error), cx);
            }
            Event::Schema { conn, .. } | Event::PoolStats { conn, .. }
                if self.monitor.owns(conn) || Some(conn) == metrics_conn => {}
            Event::Connected { conn } if Some(conn) == metrics_conn => {
                self.metrics_conn = Some((conn, true));
                for tab in std::mem::take(&mut self.metrics_waiting) {
                    if let Some(tab) = tab.upgrade() {
                        tab.update(cx, |t, cx| cx.emit(TabEvent::Run(t.sql(cx))));
                    }
                }
            }
            Event::ConnectFailed { conn, error } if Some(conn) == metrics_conn => {
                self.metrics_conn = None;
                self.metrics_waiting.clear();
                window.push_notification(
                    Notification::error(format!("Could not open the metrics: {error}")),
                    cx,
                );
            }
            event => return Some(event),
        }
        None
    }

    /// Opens a tab querying the bookmark's runs over time in `metrics.db`
    /// (read-only), shown as a line chart of the duration.
    fn open_metrics(&mut self, bookmark_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bookmark) = self.local.bookmark(bookmark_id) else {
            return;
        };
        let Some(path) = self
            .metrics
            .path()
            .map(|p| p.to_string_lossy().into_owned())
        else {
            window.push_notification(
                Notification::error("Metrics are only kept in memory; there is no file to query."),
                cx,
            );
            return;
        };
        let (conn, ready) = match self.metrics_conn {
            Some(conn) => conn,
            None => {
                let id = ConnId(self.next_id());
                self.engine.send(Command::Connect {
                    conn: id,
                    driver: DriverKind::Sqlite,
                    cfg: tangul_core::ConnectConfig {
                        host: String::new(),
                        port: 0,
                        user: String::new(),
                        password: None,
                        database: path,
                        tls: tangul_core::TlsMode::Disable,
                    },
                    ssh: None,
                });
                self.metrics_conn = Some((id, false));
                (id, false)
            }
        };
        let sql = metrics::runs_sql(&bookmark.id);
        let tab = self.add_tab(&sql, window, cx);
        tab.update(cx, |t, cx| {
            t.title = format!("{} · runs", bookmark.name).into();
            let tab_conn = TabConn {
                id: conn,
                name: "tangul metrics".into(),
                driver: DriverKind::Sqlite,
            };
            t.set_conn(tab_conn, cx);
            // Duration over time; rows are one pick away in the chart menu.
            t.set_chart_view(
                Some(ChartView {
                    style: ChartStyle::Line,
                    series: Some(0),
                }),
                cx,
            );
            if ready {
                cx.emit(TabEvent::Run(sql.clone()));
            }
        });
        if !ready {
            self.metrics_waiting.push(tab.downgrade());
        }
    }

    // ---- bookmarks -------------------------------------------------------------

    /// Asks for a name and saves `sql` as a bookmark (updating `existing` if set).
    /// `tab` gets linked to the bookmark, so saving again from it updates it.
    fn open_bookmark_dialog(
        &mut self,
        existing: Option<String>,
        sql: String,
        tab: Option<Entity<QueryTab>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = existing.and_then(|id| self.local.bookmark(&id));
        let title = if existing.is_some() {
            "Update bookmark"
        } else {
            "Bookmark query"
        };
        let db_id = self.active_db.clone();
        // From a tab: the bookmark shows its result the way the tab does now.
        let view = tab.as_ref().map(|t| t.read(cx).chart_shown());
        let form = cx.new(|cx| BookmarkForm::new(existing.as_ref(), sql, db_id, view, window, cx));
        let workspace = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let save = {
                let (form, workspace, tab) = (form.clone(), workspace.clone(), tab.clone());
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let Some(bookmark) = form.update(cx, |f, cx| f.submit(cx)) else {
                        return;
                    };
                    if let Some(tab) = &tab {
                        tab.update(cx, |t, cx| {
                            t.bookmark_id = Some(bookmark.id.clone());
                            t.bookmark_hint = None;
                            t.title = bookmark.name.clone().into();
                            cx.notify();
                        });
                    }
                    let _ = workspace.update(cx, |ws, cx| ws.save_bookmark(bookmark, window, cx));
                    window.close_dialog(cx);
                }
            };
            dialog.title(title).w(px(460.)).child(form.clone()).footer(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .ghost()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(Button::new("save").label("Save").primary().on_click(save)),
            )
        });
    }

    fn save_bookmark(&mut self, bookmark: Bookmark, window: &mut Window, cx: &mut Context<Self>) {
        let title: SharedString = bookmark.name.clone().into();
        for tab in &self.tabs {
            if tab.read(cx).bookmark_id.as_deref() == Some(bookmark.id.as_str()) {
                tab.update(cx, |t, _| t.title = title.clone());
            }
        }
        if let Err(e) = self.local.upsert_bookmark(&bookmark) {
            window.push_notification(
                Notification::error(format!("Could not save bookmark: {e:#}")),
                cx,
            );
        }
        cx.notify();
    }

    fn delete_bookmark(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.metrics.delete_bookmark(id);
        if let Err(e) = self.local.delete_bookmark(id) {
            window.push_notification(
                Notification::error(format!("Could not delete bookmark: {e:#}")),
                cx,
            );
        }
        for tab in &self.tabs {
            if tab.read(cx).bookmark_id.as_deref() == Some(id) {
                tab.update(cx, |t, _| t.bookmark_id = None);
            }
        }
        cx.notify();
    }

    /// Opens a bookmark in a new tab, optionally running it right away.
    fn open_bookmark(&mut self, id: &str, run: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bookmark) = self.local.bookmark(id) else {
            return;
        };
        let tab = self.add_tab(&bookmark.sql, window, cx);
        tab.update(cx, |t, cx| {
            t.title = bookmark.name.clone().into();
            t.bookmark_id = Some(bookmark.id.clone());
            t.set_chart_view(bookmark.view, cx);
            if run && t.conn.is_some() {
                cx.emit(TabEvent::Run(bookmark.sql.clone()));
            }
        });
    }

    /// Opens `sql` in a new tab without running it.
    fn open_sql(&mut self, title: &str, sql: &str, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.add_tab(sql, window, cx);
        tab.update(cx, |t, _| t.title = title.to_string().into());
    }

    // ---- command palette -------------------------------------------------------

    fn open_palette(&mut self, _: &OpenPalette, window: &mut Window, cx: &mut Context<Self>) {
        let db = self.active_db.clone();
        let bookmarks = self.local.bookmarks_for(db.as_deref());
        let history = match &db {
            Some(db) => self.local.history(db, "", HISTORY_LIMIT),
            None => Vec::new(),
        };
        let groups = palette::groups(&bookmarks, &history);
        let list = cx.new(|cx| CommandState::new(window, cx));
        let workspace = cx.entity().downgrade();
        window.open_dialog(cx, {
            let list = list.clone();
            move |dialog, _, _| {
                let mut command = PaletteList::new(&list)
                    .placeholder("Search commands, bookmarks and history")
                    .bordered(false);
                for group in &groups {
                    let icon = || match group.kind {
                        PaletteKind::Command => Icon::new(IconName::Settings),
                        PaletteKind::Bookmark => Icon::new(Lucide::Bookmark),
                        PaletteKind::History => Icon::new(Lucide::RotateCcwClock),
                    };
                    command = command.group(CommandGroup::new().label(group.label).items(
                        group.items.iter().map(|item| {
                            CommandItem::new()
                                .label(item.label.clone())
                                .keywords([item.search.clone()])
                                .icon(icon())
                        }),
                    ));
                }
                // Groups and items keep their positions while the list is filtered.
                let actions: Vec<Vec<PaletteAction>> = groups
                    .iter()
                    .map(|g| g.items.iter().map(|i| i.action.clone()).collect())
                    .collect();
                let pills = {
                    let workspace = workspace.clone();
                    move |_: &CommandState, _: &mut Window, cx: &mut App| {
                        palette_pills(&workspace, cx)
                    }
                };
                let cycle = |step: isize| {
                    let workspace = workspace.clone();
                    move |window: &mut Window, cx: &mut App| {
                        let _ = workspace.update(cx, |ws, cx| ws.cycle_panel(step, cx));
                        // The dialog isn't part of the workspace view; redraw it too.
                        window.refresh();
                    }
                };
                let (next, prev) = (cycle(1), cycle(-1));
                let command = command.header(pills);
                let workspace = workspace.clone();
                dialog.w(px(560.)).close_button(false).child(
                    div()
                        .key_context("Palette")
                        .on_action(move |_: &NextSidebarPanel, window, cx| next(window, cx))
                        .on_action(move |_: &PrevSidebarPanel, window, cx| prev(window, cx))
                        .child(command.on_confirm(move |path, window, cx| {
                            let Some(action) = actions
                                .get(path.section)
                                .and_then(|items| items.get(path.row))
                                .cloned()
                            else {
                                return;
                            };
                            window.close_dialog(cx);
                            let _ =
                                workspace.update(cx, |ws, cx| ws.run_palette(action, window, cx));
                        })),
                )
            }
        });
        // The dialog is built on the next frame; focus the search field once it exists.
        window.defer(cx, move |window, cx| {
            list.update(cx, |list, cx| list.focus(window, cx))
        });
    }

    fn run_palette(&mut self, action: PaletteAction, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            PaletteAction::Settings => self.open_settings(window, cx),
            PaletteAction::Bookmark(id) => self.open_bookmark(&id, false, window, cx),
            PaletteAction::History(sql) => self.open_sql("History", &sql, window, cx),
        }
    }

    // ---- history and learning ----------------------------------------------------

    fn refresh_history(&mut self, cx: &mut Context<Self>) {
        let search = self.history_search.read(cx).value().trim().to_string();
        self.history = match &self.active_db {
            Some(db) => self.local.history(db, &search, HISTORY_LIMIT),
            None => Vec::new(),
        };
        cx.notify();
    }

    /// Records a finished query in history and, if it succeeded, learns from it.
    fn record_run(
        &mut self,
        run: RunningQuery,
        error: Option<String>,
        elapsed_ms: i64,
        cx: &mut Context<Self>,
    ) {
        let Some(db_id) = self.find_user(&run.user_id).map(|(db, _)| db.id.clone()) else {
            return;
        };
        let tab = run.tab.upgrade();
        let rows = tab
            .as_ref()
            .and_then(|t| t.read(cx).row_count(cx))
            .map(|n| n as i64);
        let fp = fingerprint(&run.sql);
        let succeeded = error.is_none();
        // A run of the tab's bookmark (the whole of it) counts for its metrics.
        let bookmark = tab
            .as_ref()
            .and_then(|t| t.read(cx).bookmark_id.clone())
            .and_then(|id| self.local.bookmark(&id))
            .filter(|b| bare_sql(&b.sql) == bare_sql(&run.sql));
        if let Some(b) = bookmark {
            let record = metrics::Run {
                bookmark_id: b.id,
                bookmark_name: b.name,
                started_at: run.started_at,
                duration_ms: elapsed_ms,
                rows,
                error: error.clone(),
                scheduled: false,
            };
            self.metrics
                .record(&record, &settings::current(cx).monitoring, now_ms());
        }
        let entry = NewHistory {
            database_id: &db_id,
            user_id: &run.user_id,
            sql: &run.sql,
            fingerprint: &fp,
            entry: HistoryEntry {
                sql: run.sql.clone(),
                started_at: run.started_at,
                duration_ms: elapsed_ms,
                rows,
                error,
            },
        };
        if let Err(e) = self.local.add_history(&entry) {
            eprintln!("could not record history: {e:#}");
        }
        if succeeded {
            if let Some(state) = self.conns.get(&run.user_id) {
                let learned = learn(&run.sql, &state.schema);
                let now = now_ms() / 1000;
                if let Err(e) = self.local.record_usage(&db_id, &learned, now) {
                    eprintln!("could not record usage: {e:#}");
                }
                let usage = Arc::new(self.local.usage(&db_id, now));
                self.usage.insert(db_id.clone(), usage.clone());
                // Related rows (foreign keys, most used first) and UPDATE / DELETE
                // statements in the result's context menu.
                if let Some(tab) = &tab {
                    let columns = tab.read(cx).result_columns(cx);
                    let mut links = related_links(&run.sql, &columns, &state.schema);
                    if links.len() > 1 {
                        let bookmarked: Vec<_> = self
                            .local
                            .bookmarks_for(Some(&db_id))
                            .iter()
                            .flat_map(|b| learn(&b.sql, &state.schema))
                            .collect();
                        rank_links(&mut links, &usage, &bookmarked);
                    }
                    let edit = row_edit(&run.sql, &columns, &state.schema);
                    let facts = column_facts(&run.sql, &columns, &state.schema);
                    tab.update(cx, |t, cx| t.set_row_actions(links, edit, facts, cx));
                }
                self.sync_assist_for_db(&db_id);
            }
            let runs = self.local.runs(&db_id, &fp);
            let hint = (runs >= BOOKMARK_HINT_RUNS && !self.local.is_bookmarked(&db_id, &fp))
                .then_some(runs);
            if let Some(tab) = tab {
                tab.update(cx, |t, cx| {
                    t.bookmark_hint = hint;
                    cx.notify();
                });
            }
        }
        if self.active_db.as_deref() == Some(db_id.as_str()) {
            self.refresh_history(cx);
        }
    }

    fn usage_for(&mut self, db_id: &str) -> Arc<Usage> {
        if let Some(u) = self.usage.get(db_id) {
            return u.clone();
        }
        let u = Arc::new(self.local.usage(db_id, now_ms() / 1000));
        self.usage.insert(db_id.to_string(), u.clone());
        u
    }

    fn schema_index(&self, user_id: &str, info: SchemaInfo) -> Arc<SchemaIndex> {
        let (driver, default) = self
            .find_user(user_id)
            .map(|(db, _)| {
                (
                    db.driver,
                    SchemaIndex::default_schema_for(db.driver, &db.database),
                )
            })
            .unwrap_or((DriverKind::Postgres, None));
        Arc::new(SchemaIndex::new(info, driver, default))
    }

    /// Publishes a connection's schema and usage to the completion providers.
    fn sync_assist(&mut self, user_id: &str) {
        let Some(db_id) = self.find_user(user_id).map(|(db, _)| db.id.clone()) else {
            return;
        };
        let usage = self.usage_for(&db_id);
        if let Some(state) = self.conns.get(user_id) {
            self.assist.borrow_mut().insert(
                state.id,
                AssistData {
                    schema: state.schema.clone(),
                    usage,
                },
            );
        }
    }

    fn sync_assist_for_db(&mut self, db_id: &str) {
        let users: Vec<String> = self
            .store
            .database(db_id)
            .into_iter()
            .flat_map(|db| &db.users)
            .filter(|u| self.conns.contains_key(&u.id))
            .map(|u| u.id.clone())
            .collect();
        for u in users {
            self.sync_assist(&u);
        }
    }

    /// Suggestions for a table of the active connection: learned from history first,
    /// then generated from its keys.
    fn table_suggestions(&self, table: &TableInfo) -> Vec<QuerySuggestion> {
        let (Some(state), Some(db)) = (self.active_conn(), self.active_db.as_deref()) else {
            return Vec::new();
        };
        let Some(ix) = state.schema.resolve(Some(&table.schema), &table.name) else {
            return Vec::new();
        };
        let patterns = self.local.patterns(db, 200);
        suggest_for_table(ix, &state.schema, &patterns)
    }

    // ---- connections -----------------------------------------------------------

    fn connect(&mut self, user_id: &str, cx: &mut Context<Self>) {
        let Some((db, user)) = self
            .find_user(user_id)
            .map(|(db, user)| (db.clone(), user.clone()))
        else {
            return;
        };
        let id = match self.conns.get(user_id) {
            Some(state) => state.id,
            None => ConnId(self.next_id()),
        };
        let password = self.store.password(user_id);
        let ssh = self.store.ssh_target(&db);
        // Show (and complete against) the cached schema until the server answers.
        let schema = match self.conns.get(user_id) {
            Some(state) => state.schema.clone(),
            None => self.schema_index(
                user_id,
                self.local.cached_schema(user_id).unwrap_or_default(),
            ),
        };
        self.conns.insert(
            user_id.to_string(),
            ConnState {
                id,
                status: ConnStatus::Connecting,
                schema,
                stats: PoolStats::default(),
            },
        );
        self.conn_users.insert(id, user_id.to_string());
        self.sync_assist(user_id);
        self.engine.send(Command::Connect {
            conn: id,
            driver: db.driver,
            cfg: tangul_core::ConnectConfig::new(&db, &user, password),
            ssh,
        });
        cx.notify();
    }

    /// Makes a user the active connection (connecting if needed) and uses it for the
    /// current tab.
    fn select_user(
        &mut self,
        db_id: String,
        user_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let needs_connect = !matches!(
            self.conns.get(&user_id).map(|c| &c.status),
            Some(ConnStatus::Connected | ConnStatus::Connecting)
        );
        if needs_connect {
            self.connect(&user_id, cx);
        }
        self.active_db = Some(db_id);
        self.active_user = Some(user_id.clone());
        self.local.set_setting(LAST_USER_SETTING, &user_id);
        self.assign_active_tab(&user_id, window, cx);
        self.refresh_history(cx);
        cx.notify();
    }

    /// Switches to a database with a user that is already connected, else its first
    /// user. A database without users asks for one.
    fn select_database(&mut self, db_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(db) = self.store.database(&db_id) else {
            return;
        };
        let user = db
            .users
            .iter()
            .find(|u| {
                matches!(
                    self.conns.get(&u.id).map(|c| &c.status),
                    Some(ConnStatus::Connected)
                )
            })
            .or(db.users.first())
            .map(|u| u.id.clone());
        match user {
            Some(user_id) => self.select_user(db_id, user_id, window, cx),
            None => {
                self.active_db = Some(db_id.clone());
                self.active_user = None;
                self.open_form(FormMode::User, Some(db_id), None, window, cx);
            }
        }
        cx.notify();
    }

    fn assign_active_tab(&mut self, user_id: &str, _: &mut Window, cx: &mut Context<Self>) {
        let Some(conn) = self.tab_conn(user_id) else {
            return;
        };
        if let Some(tab) = self.tabs.get(self.active_tab) {
            if tab.read(cx).is_running() {
                return;
            }
            tab.update(cx, |t, cx| t.set_conn(conn, cx));
        }
    }

    /// `schema.table`, quoted only where needed.
    fn qualified_name(&self, table: &TableInfo) -> Option<String> {
        let driver = self.active_database()?.driver;
        Some(format!(
            "{}.{}",
            quote_ident(driver, &table.schema),
            quote_ident(driver, &table.name)
        ))
    }

    /// The table's name for generated SQL: without the default schema (`public`, or
    /// the MySQL connection's database) and quoted only where needed.
    fn sql_name(&self, table: &TableInfo) -> Option<String> {
        let schema = &self.active_conn()?.schema;
        match schema.resolve(Some(&table.schema), &table.name) {
            Some(ix) => Some(schema.sql_name(ix)),
            None => self.qualified_name(table),
        }
    }

    fn open_table(&mut self, table: &TableInfo, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.sql_name(table) else {
            return;
        };
        self.run_in_new_tab(
            &table.name,
            format!("SELECT * FROM {name} LIMIT 100"),
            window,
            cx,
        );
    }

    fn count_table(&mut self, table: &TableInfo, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.sql_name(table) else {
            return;
        };
        self.run_in_new_tab(
            &table.name,
            format!("SELECT COUNT(*) FROM {name}"),
            window,
            cx,
        );
    }

    fn run_in_new_tab(
        &mut self,
        title: &str,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.add_tab(&sql, window, cx);
        let title: SharedString = title.to_string().into();
        tab.update(cx, |t, cx| {
            t.title = title;
            cx.emit(TabEvent::Run(sql));
        });
    }

    /// Opens the database/user dialog. `db_id` is the database to edit or add a user
    /// to, `user_id` the user to edit.
    fn open_form(
        &mut self,
        mode: FormMode,
        db_id: Option<String>,
        user_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let db = db_id
            .as_deref()
            .and_then(|id| self.store.database(id))
            .cloned();
        let user = db
            .as_ref()
            .zip(user_id.as_deref())
            .and_then(|(db, id)| db.user(id))
            .cloned();
        let title: SharedString = match (mode, &db, &user) {
            (FormMode::NewDatabase, ..) => "New database".into(),
            (FormMode::EditDatabase, ..) => "Edit database".into(),
            (FormMode::User, Some(db), Some(_)) => {
                format!("Edit user · {}", db.display_name()).into()
            }
            (FormMode::User, Some(db), None) => format!("Add user · {}", db.display_name()).into(),
            (FormMode::User, None, _) => return,
        };
        let save_label = if mode == FormMode::EditDatabase {
            "Save"
        } else {
            "Save & connect"
        };
        let form = cx.new(|cx| ConnectionForm::new(mode, db.as_ref(), user.as_ref(), window, cx));
        let workspace = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let save = {
                let form = form.clone();
                let workspace = workspace.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let Some(output) = form.update(cx, |f, cx| f.submit(cx)) else {
                        return;
                    };
                    let _ = workspace.update(cx, |ws, cx| ws.save_form(output, window, cx));
                    window.close_dialog(cx);
                }
            };
            // Too much typed in to lose by a stray click or key: only Cancel closes it.
            dialog
                .title(title.clone())
                .w(px(460.))
                .overlay_closable(false)
                .keyboard(false)
                .close_button(false)
                .child(form.clone())
                .footer(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel")
                                .label("Cancel")
                                .ghost()
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("save")
                                .label(save_label)
                                .primary()
                                .on_click(save),
                        ),
                )
        });
    }

    fn save_form(&mut self, output: FormOutput, window: &mut Window, cx: &mut Context<Self>) {
        let FormOutput {
            database,
            user,
            ssh_secret,
        } = output;
        let db_id = database.id.clone();
        let target_changed = ssh_secret.is_some()
            || self.store.database(&db_id).is_some_and(|old| {
                (
                    old.driver,
                    &old.host,
                    old.port,
                    &old.database,
                    old.tls,
                    &old.ssh,
                ) != (
                    database.driver,
                    &database.host,
                    database.port,
                    &database.database,
                    database.tls,
                    &database.ssh,
                )
            });
        let mut result = self.store.upsert_database(database);
        if let Some(secret) = ssh_secret {
            self.store.set_ssh_secret(&db_id, secret);
        }
        let user_id = user.map(|(user, password)| {
            let id = user.id.clone();
            result = std::mem::replace(&mut result, Ok(()))
                .and_then(|_| self.store.upsert_user(&db_id, user, password));
            id
        });
        if let Err(e) = result {
            window.push_notification(
                Notification::error(format!("Could not save connection: {e:#}")),
                cx,
            );
        }
        // Reconnect with the new settings. ConnIds are kept, so tabs using them keep
        // working; the engine replaces the old pool.
        if target_changed {
            let open: Vec<String> = self
                .store
                .database(&db_id)
                .into_iter()
                .flat_map(|db| &db.users)
                .filter(|u| self.conns.contains_key(&u.id))
                .map(|u| u.id.clone())
                .collect();
            for id in open {
                self.connect(&id, cx);
            }
        }
        if let Some(user_id) = user_id {
            self.connect(&user_id, cx);
            self.select_user(db_id, user_id, window, cx);
        }
        cx.notify();
    }

    fn on_engine_event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        let Some(event) = self.monitor_event(event, window, cx) else {
            return;
        };
        match event {
            Event::Query { query, event } => {
                let finished = match &event {
                    QueryEvent::Finished { elapsed, error, .. } => {
                        Some((error.clone(), elapsed.as_millis() as i64))
                    }
                    QueryEvent::Exec(_) => None,
                };
                if let Some(tab) = self.queries.get(&query).and_then(|q| q.tab.upgrade()) {
                    tab.update(cx, |t, cx| t.on_query_event(event, cx));
                }
                if let Some((error, elapsed_ms)) = finished
                    && let Some(run) = self.queries.remove(&query)
                    && !run.derived
                {
                    self.record_run(run, error, elapsed_ms, cx);
                }
            }
            Event::Connected { conn } => {
                if let Some(state) = self.conn_state_mut(conn) {
                    state.status = ConnStatus::Connected;
                }
                // Tabs without a connection pick up the newly connected one.
                if let Some(user_id) = self.conn_users.get(&conn).cloned()
                    && let Some(tc) = self.tab_conn(&user_id)
                {
                    for tab in &self.tabs {
                        if tab.read(cx).conn.is_none() {
                            tab.update(cx, |t, cx| t.set_conn(tc.clone(), cx));
                        }
                    }
                }
            }
            Event::HostKeyUnknown {
                conn,
                host,
                port,
                fingerprint,
                key,
            } => self.confirm_host_key(conn, host, port, fingerprint, key, window, cx),
            Event::ConnectFailed { conn, error } => {
                if let Some(state) = self.conn_state_mut(conn) {
                    state.status = ConnStatus::Failed(error.clone().into());
                }
                window.push_notification(Notification::error(error), cx);
            }
            Event::Schema { conn, result } => match result {
                Ok(info) => {
                    if let Some(user_id) = self.conn_users.get(&conn).cloned() {
                        if let Err(e) = self.local.save_schema(&user_id, &info) {
                            eprintln!("could not cache schema: {e:#}");
                        }
                        let schema = self.schema_index(&user_id, info);
                        if let Some(state) = self.conn_state_mut(conn) {
                            state.schema = schema;
                        }
                        self.sync_assist(&user_id);
                    }
                }
                Err(e) => window.push_notification(Notification::error(e), cx),
            },
            Event::Validated { request, problems } => {
                if let Some((tab, generation)) = self.validations.remove(&request)
                    && let Some(tab) = tab.upgrade()
                {
                    tab.update(cx, |t, cx| t.apply_validation(generation, problems, cx));
                }
            }
            Event::PoolStats { conn, stats } => {
                if let Some(state) = self.conn_state_mut(conn) {
                    state.stats = stats;
                }
            }
        }
        cx.notify();
    }

    /// Unknown SSH host key: show its fingerprint and let the user decide. Trusting
    /// adds it to `~/.ssh/known_hosts` and connects again.
    #[allow(clippy::too_many_arguments)]
    fn confirm_host_key(
        &mut self,
        conn: ConnId,
        host: String,
        port: u16,
        fingerprint: String,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(user_id) = self.conn_users.get(&conn).cloned() else {
            return;
        };
        let workspace = cx.entity().downgrade();
        let target: SharedString = if port == 22 {
            host.clone().into()
        } else {
            format!("{host}:{port}").into()
        };
        let fingerprint: SharedString = fingerprint.into();
        window.open_dialog(cx, move |dialog, _, cx| {
            let trust = {
                let (workspace, user_id, host, key) = (
                    workspace.clone(),
                    user_id.clone(),
                    host.clone(),
                    key.clone(),
                );
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let _ = workspace.update(cx, |ws, cx| {
                        ws.engine.send(Command::TrustHostKey {
                            host: host.clone(),
                            port,
                            key: key.clone(),
                        });
                        ws.connect(&user_id, cx);
                    });
                    window.close_dialog(cx);
                }
            };
            let cancel = {
                let (workspace, user_id) = (workspace.clone(), user_id.clone());
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let _ = workspace.update(cx, |ws, cx| {
                        if let Some(state) = ws.conns.get_mut(&user_id) {
                            state.status = ConnStatus::Failed("SSH host key not trusted".into());
                        }
                        cx.notify();
                    });
                    window.close_dialog(cx);
                }
            };
            let theme = cx.theme();
            dialog
                .title("Unknown SSH host")
                .w(px(480.))
                .child(
                    v_flex()
                        .gap_3()
                        .text_sm()
                        .child(format!(
                            "The authenticity of {target} can't be established. \
                             Compare the fingerprint with the one your server admin gave you."
                        ))
                        .child(
                            div()
                                .p_2()
                                .rounded(theme.radius)
                                .bg(theme.muted)
                                .font_family(theme.mono_font_family.clone())
                                .text_xs()
                                .child(fingerprint.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("Trusting adds the key to ~/.ssh/known_hosts."),
                        ),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel")
                                .label("Cancel")
                                .ghost()
                                .on_click(cancel),
                        )
                        .child(
                            Button::new("trust")
                                .label("Trust & connect")
                                .primary()
                                .on_click(trust),
                        ),
                )
        });
    }

    fn conn_state_mut(&mut self, conn: ConnId) -> Option<&mut ConnState> {
        let user_id = self.conn_users.get(&conn)?;
        self.conns.get_mut(user_id).filter(|s| s.id == conn)
    }

    // ---- rendering ---------------------------------------------------------------

    fn build_sidebar_rows(&self, search: &str) -> Vec<SidebarRow> {
        let Some((user_id, state)) = self
            .active_user
            .as_ref()
            .and_then(|id| Some((id, self.conns.get(id)?)))
        else {
            return Vec::new();
        };
        if !search.is_empty() {
            return self.search_rows(&state.schema, search);
        }
        let mut rows = Vec::new();
        let favorites: Vec<SidebarRow> = self
            .favorites
            .iter()
            .filter_map(|(schema, name)| {
                let ix = state.schema.resolve(Some(schema), name)?;
                Some(SidebarRow::Favorite {
                    table: state.schema.table(ix).clone(),
                })
            })
            .collect();
        if !favorites.is_empty() {
            rows.push(SidebarRow::Heading("Favorites".into()));
            rows.extend(favorites);
            let title = match self.active_database().map(|db| db.driver) {
                Some(DriverKind::Mysql) => "Databases",
                _ => "Schemas",
            };
            rows.push(SidebarRow::Heading(title.into()));
        }
        // Drivers return tables ordered by schema, then name.
        let schemas: Vec<&[TableInfo]> = state
            .schema
            .tables()
            .chunk_by(|a, b| a.schema == b.schema)
            .collect();
        let only_schema = schemas.len() == 1;
        for tables in schemas {
            let schema: SharedString = tables[0].schema.clone().into();
            let expanded = self
                .schema_open
                .get(&(user_id.clone(), schema.clone()))
                .copied()
                .unwrap_or(only_schema);
            rows.push(SidebarRow::Schema {
                schema: schema.clone(),
                expanded,
            });
            if !expanded {
                continue;
            }
            let only_kind = tables.iter().all(|t| t.kind == tables[0].kind);
            for kind in [TableKind::Table, TableKind::View] {
                let items: Vec<&TableInfo> = tables.iter().filter(|t| t.kind == kind).collect();
                if items.is_empty() {
                    continue;
                }
                let expanded = self
                    .group_open
                    .get(&(user_id.clone(), schema.clone(), kind))
                    .copied()
                    .unwrap_or(only_kind);
                rows.push(SidebarRow::Group {
                    schema: schema.clone(),
                    kind,
                    count: items.len(),
                    expanded,
                });
                if expanded {
                    rows.extend(
                        items
                            .into_iter()
                            .map(|t| SidebarRow::Table { table: t.clone() }),
                    );
                }
            }
        }
        rows
    }

    /// Search results: schemas and tables whose names match, then matching columns.
    /// Case-insensitive; names starting with the text come first.
    fn search_rows(&self, schema: &SchemaIndex, search: &str) -> Vec<SidebarRow> {
        let needle = search.to_lowercase();
        let rank = |name: &str| {
            let name = name.to_lowercase();
            if name == needle {
                Some(0)
            } else if name.starts_with(&needle) {
                Some(1)
            } else if name.contains(&needle) {
                Some(2)
            } else {
                None
            }
        };
        let mut schemas: Vec<(u8, SharedString)> = Vec::new();
        for tables in schema.tables().chunk_by(|a, b| a.schema == b.schema) {
            if let Some(r) = rank(&tables[0].schema) {
                schemas.push((r, tables[0].schema.clone().into()));
            }
        }
        let mut tables: Vec<(u8, &TableInfo)> = schema
            .tables()
            .iter()
            .filter_map(|t| Some((rank(&t.name)?, t)))
            .collect();
        let mut columns: Vec<(u8, &ColumnMeta)> = schema
            .info()
            .columns
            .iter()
            .filter_map(|c| Some((rank(&c.name)?, c)))
            .collect();
        // Stable: keeps schema / table order within a rank.
        schemas.sort_by_key(|(r, _)| *r);
        tables.sort_by_key(|(r, _)| *r);
        columns.sort_by_key(|(r, _)| *r);

        let mut rows = Vec::new();
        if !schemas.is_empty() || !tables.is_empty() {
            let title = match self.active_database().map(|db| db.driver) {
                Some(DriverKind::Mysql) => "Databases and tables",
                _ => "Schemas and tables",
            };
            rows.push(SidebarRow::Heading(title.into()));
            rows.extend(
                schemas
                    .into_iter()
                    .map(|(_, schema)| SidebarRow::SchemaHit { schema }),
            );
            rows.extend(
                tables
                    .into_iter()
                    .map(|(_, t)| SidebarRow::TableHit { table: t.clone() }),
            );
        }
        if !columns.is_empty() {
            rows.push(SidebarRow::Heading("Columns".into()));
            rows.extend(columns.into_iter().filter_map(|(_, c)| {
                let ix = schema.resolve(Some(&c.schema), &c.table)?;
                Some(SidebarRow::ColumnHit {
                    table: schema.table(ix).clone(),
                    column: c.name.clone().into(),
                    data_type: c.data_type.clone().into(),
                })
            }));
        }
        rows
    }

    /// Clears the search and shows `schema` (and `table` in it) in the tree.
    fn reveal(
        &mut self,
        schema: SharedString,
        table: Option<&TableInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(user_id) = self.active_user.clone() {
            self.schema_open
                .insert((user_id.clone(), schema.clone()), true);
            if let Some(t) = table {
                self.group_open
                    .insert((user_id, schema.clone(), t.kind), true);
            }
        }
        let target = (schema, table.map(|t| t.name.clone()));
        self.reveal = Some(target.clone());
        self.schema_search
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.revealed = Some(target);
        cx.notify();
    }

    fn is_favorite(&self, table: &TableInfo) -> bool {
        self.favorites
            .iter()
            .any(|(schema, name)| *schema == table.schema && *name == table.name)
    }

    fn toggle_favorite(&mut self, table: &TableInfo, window: &mut Window, cx: &mut Context<Self>) {
        let Some(db_id) = self.active_db.clone() else {
            return;
        };
        let on = !self.is_favorite(table);
        if let Err(e) = self
            .local
            .set_favorite(&db_id, &table.schema, &table.name, on)
        {
            window.push_notification(
                Notification::error(format!("Could not save favorite: {e:#}")),
                cx,
            );
        }
        cx.notify();
    }

    /// Star toggling `table` as a favorite: filled and always shown for a favorite,
    /// else only while `group` is hovered.
    fn favorite_button(
        &self,
        ix: usize,
        table: &TableInfo,
        group: SharedString,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        let favorite = self.is_favorite(table);
        let (icon, tooltip) = if favorite {
            (Lucide::StarFill, "Remove from favorites")
        } else {
            (Lucide::Star, "Add to favorites")
        };
        let table = table.clone();
        div()
            .when(!favorite, |d| {
                d.invisible().group_hover(group, |s| s.visible())
            })
            .id(("table-favorite-wrap", ix))
            // Keep the click from also opening the table.
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                Button::new(("table-favorite", ix))
                    .icon(Icon::new(icon))
                    .ghost()
                    .xsmall()
                    .tooltip(tooltip)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.toggle_favorite(&table, window, cx)
                    })),
            )
    }

    fn render_sidebar_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let user_id = self.active_user.clone().unwrap_or_default();
        let driver = self.active_database().map(|db| db.driver);
        // Zed-style tree: indentation plus one thin guide line per ancestor level,
        // expansion shown by the folder icon instead of chevrons.
        let guide = theme.sidebar_border;
        let row = |depth: usize| {
            h_flex()
                .id(("sidebar-row", ix))
                .relative()
                .w_full()
                .h(px(ROW_HEIGHT))
                .pl(px(TREE_PAD + depth as f32 * TREE_INDENT))
                .pr_2()
                .gap_1p5()
                .items_center()
                .text_sm()
                .hover(|s| s.bg(theme.sidebar_accent))
                .cursor_pointer()
                .children((0..depth).map(|level| {
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(TREE_PAD + level as f32 * TREE_INDENT + TREE_ICON / 2.))
                        .w(px(1.))
                        .bg(guide)
                }))
        };
        let folder = |expanded: bool| {
            Icon::new(if expanded {
                IconName::FolderOpen
            } else {
                IconName::FolderClosed
            })
        };

        let row_kind = self.sidebar_rows[ix].clone();
        let in_favorites = matches!(row_kind, SidebarRow::Favorite { .. });
        match row_kind {
            SidebarRow::Schema { schema, expanded } => {
                // A MySQL "schema" is a database.
                let icon: Icon = match driver {
                    Some(DriverKind::Mysql) => Icon::new(Lucide::Database),
                    _ => folder(expanded),
                };
                let key = (user_id, schema.clone());
                let revealed = self.revealed == Some((schema.clone(), None));
                row(0)
                    .when(revealed, |r| r.bg(theme.sidebar_accent))
                    .child(icon.small().text_color(theme.muted_foreground))
                    .child(div().min_w_0().truncate().child(schema))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.schema_open.insert(key.clone(), !expanded);
                        cx.notify();
                    }))
                    .into_any_element()
            }
            SidebarRow::Group {
                schema,
                kind,
                count,
                expanded,
            } => {
                let label = match kind {
                    TableKind::Table => "Tables",
                    TableKind::View => "Views",
                };
                let key = (user_id, schema, kind);
                row(1)
                    .child(folder(expanded).small().text_color(theme.muted_foreground))
                    .child(div().min_w_0().truncate().child(label))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(count.to_string()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.group_open.insert(key.clone(), !expanded);
                        cx.notify();
                    }))
                    .into_any_element()
            }
            SidebarRow::Table { table } | SidebarRow::Favorite { table } => {
                let icon = match table.kind {
                    TableKind::Table => Icon::new(Lucide::Table),
                    TableKind::View => Icon::new(IconName::Eye),
                };
                let name = table.name.clone();
                let group: SharedString = format!("table-row-{ix}").into();
                let weak = cx.entity().downgrade();
                let menu_table = table.clone();
                let suggestions = Button::new(("table-suggest", ix))
                    .icon(Icon::new(Lucide::Lightbulb))
                    .ghost()
                    .xsmall()
                    .tooltip("Suggested queries")
                    .dropdown_menu(move |menu, _, cx| {
                        let Some(ws) = weak.upgrade() else {
                            return menu;
                        };
                        let list = ws.read(cx).table_suggestions(&menu_table);
                        suggestions_menu(menu, &menu_table.name, list, &weak)
                    });
                let revealed = !in_favorites
                    && self.revealed.as_ref().is_some_and(|(s, t)| {
                        *s == table.schema && t.as_ref() == Some(&table.name)
                    });
                let favorite = self.favorite_button(ix, &table, group.clone(), cx);
                row(if in_favorites { 0 } else { 2 })
                    .when(revealed, |r| r.bg(theme.sidebar_accent))
                    .group(group.clone())
                    .child(icon.small().text_color(theme.muted_foreground))
                    .map(|r| {
                        if in_favorites {
                            r.child(div().min_w_0().truncate().child(name)).child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(table.schema.clone()),
                            )
                        } else {
                            r.child(div().flex_1().min_w_0().truncate().child(name))
                        }
                    })
                    .child(
                        div()
                            .invisible()
                            .group_hover(group, |s| s.visible())
                            .id(("table-suggest-wrap", ix))
                            // Keep the click from also opening the table.
                            .on_click(|_, _, cx| cx.stop_propagation())
                            .child(suggestions),
                    )
                    // Rightmost, so a favorite's star lines up at the edge.
                    .child(favorite)
                    .on_click({
                        let table = table.clone();
                        cx.listener(move |this, _, window, cx| this.open_table(&table, window, cx))
                    })
                    .context_menu({
                        let weak = cx.entity().downgrade();
                        move |menu, _, cx| table_menu(menu, &table, &weak, cx)
                    })
                    .into_any_element()
            }
            SidebarRow::Heading(title) => h_flex()
                .h(px(ROW_HEIGHT))
                .px_2()
                .items_end()
                .pb_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(title)
                .into_any_element(),
            SidebarRow::SchemaHit { schema } => {
                let icon = match driver {
                    Some(DriverKind::Mysql) => Icon::new(Lucide::Database),
                    _ => folder(false),
                };
                let target = schema.clone();
                row(0)
                    .child(icon.small().text_color(theme.muted_foreground))
                    .child(div().min_w_0().truncate().child(schema))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.reveal(target.clone(), None, window, cx)
                    }))
                    .into_any_element()
            }
            SidebarRow::TableHit { table } => {
                let icon = match table.kind {
                    TableKind::Table => Icon::new(Lucide::Table),
                    TableKind::View => Icon::new(IconName::Eye),
                };
                let group: SharedString = format!("table-hit-{ix}").into();
                let favorite = self.favorite_button(ix, &table, group.clone(), cx);
                row(0)
                    .group(group)
                    .child(icon.small().text_color(theme.muted_foreground))
                    .child(div().min_w_0().truncate().child(table.name.clone()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(table.schema.clone()),
                    )
                    .child(favorite)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.reveal(table.schema.clone().into(), Some(&table), window, cx)
                    }))
                    .into_any_element()
            }
            SidebarRow::ColumnHit {
                table,
                column,
                data_type,
            } => row(0)
                .child(
                    Icon::new(IconName::Minus)
                        .small()
                        .text_color(theme.muted_foreground),
                )
                .child(div().flex_none().child(column))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{}.{} · {data_type}", table.schema, table.name)),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.reveal(table.schema.clone().into(), Some(&table), window, cx)
                }))
                .into_any_element(),
        }
    }

    /// The content area without tabs: an engraved wordmark and what to do next.
    fn render_empty_state(&self, cx: &mut Context<Self>) -> AnyElement {
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
        v_flex()
            .flex_1()
            .size_full()
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

    /// Placeholder for the sidebar when there is nothing to list yet.
    fn render_sidebar_empty(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let message = |text: SharedString| {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let body = v_flex().px_3().py_2().gap_2().items_start();
        let Some(db) = self.active_database() else {
            return Some(
                body.child(message("No databases yet.".into()))
                    .child(
                        Button::new("empty-add-db")
                            .label("Add database")
                            .icon(IconName::Plus)
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_form(FormMode::NewDatabase, None, None, window, cx)
                            })),
                    )
                    .into_any_element(),
            );
        };
        let db_id = db.id.clone();
        let Some(user_id) = self.active_user.clone() else {
            return Some(
                body.child(message("No user for this database.".into()))
                    .child(
                        Button::new("empty-add-user")
                            .label("Add user")
                            .icon(IconName::Plus)
                            .small()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_form(
                                    FormMode::User,
                                    Some(db_id.clone()),
                                    None,
                                    window,
                                    cx,
                                )
                            })),
                    )
                    .into_any_element(),
            );
        };
        let connect = |label: &'static str| {
            Button::new("empty-connect")
                .label(label)
                .small()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.connect(&user_id, cx);
                    this.assign_active_tab(&user_id, window, cx);
                }))
        };
        match self
            .active_conn()
            .map(|c| (&c.status, c.schema.tables().is_empty()))
        {
            None => Some(
                body.child(message("Not connected.".into()))
                    .child(connect("Connect"))
                    .into_any_element(),
            ),
            Some((ConnStatus::Connecting, _)) => {
                Some(body.child(message("Connecting…".into())).into_any_element())
            }
            Some((ConnStatus::Failed(e), _)) => Some(
                body.child(div().text_sm().text_color(theme.danger).child(e.clone()))
                    .child(connect("Retry"))
                    .into_any_element(),
            ),
            Some((ConnStatus::Connected, true)) => Some(
                body.child(message("No tables or views.".into()))
                    .into_any_element(),
            ),
            Some((ConnStatus::Connected, false)) => None,
        }
    }

    fn save_sidebar_width(&mut self, width: f32) {
        self.sidebar_width = width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
        self.local
            .set_setting(SIDEBAR_WIDTH_SETTING, &self.sidebar_width.to_string());
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
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

    fn render_schema_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let search = self.schema_search.read(cx).value().trim().to_string();
        self.favorites = self
            .active_db
            .as_deref()
            .map(|db| self.local.favorites(db))
            .unwrap_or_default();
        self.sidebar_rows = self.build_sidebar_rows(&search);
        if let Some((schema, table)) = self.reveal.take() {
            let target = self.sidebar_rows.iter().position(|row| match row {
                SidebarRow::Table { table: t } => {
                    table.as_ref() == Some(&t.name) && t.schema == schema.as_ref()
                }
                SidebarRow::Schema { schema: s, .. } => table.is_none() && *s == schema,
                _ => false,
            });
            if let Some(ix) = target {
                self.sidebar_scroll
                    .scroll_to_item(ix, ScrollStrategy::Center);
            }
        }
        let has_schema = self
            .active_conn()
            .is_some_and(|c| !c.schema.tables().is_empty());
        let no_hits = !search.is_empty() && self.sidebar_rows.is_empty();
        let empty = self.render_sidebar_empty(cx);
        let theme = cx.theme();
        let count = self.sidebar_rows.len();
        let heading = match self.active_database().map(|db| db.driver) {
            Some(DriverKind::Mysql) => "DATABASES",
            _ => "SCHEMAS",
        };
        let refresh = self
            .active_conn()
            .filter(|c| c.status == ConnStatus::Connected)
            .map(|c| c.id);
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                h_flex()
                    .h_9()
                    .px_3()
                    .items_center()
                    .justify_between()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(heading)
                    .when_some(refresh, |this, conn| {
                        this.child(
                            Button::new("refresh-schema")
                                .icon(IconName::RotateCw)
                                .ghost()
                                .xsmall()
                                .tooltip("Refresh")
                                .on_click(cx.listener(move |this, _, _, _| {
                                    this.engine.send(Command::RefreshSchema { conn });
                                })),
                        )
                    }),
            )
            .when(has_schema, |this| {
                this.child(
                    div()
                        .px_2()
                        .pb_2()
                        .child(Input::new(&self.schema_search).small().cleanable(true)),
                )
            })
            .children(empty)
            .when(no_hits, |this| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("Nothing matches."),
                )
            })
            .child(
                uniform_list(
                    "sidebar",
                    count,
                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|ix| this.render_sidebar_row(ix, cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.sidebar_scroll)
                .px_1()
                .flex_1(),
            )
    }

    fn render_bookmarks_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let search = self.bookmark_search.read(cx).value().trim().to_lowercase();
        let all: Vec<Bookmark> = self.local.bookmarks_for(self.active_db.as_deref());
        let total = all.len();
        let bookmarks: Vec<Bookmark> = all
            .into_iter()
            .filter(|b| {
                search.is_empty()
                    || b.name.to_lowercase().contains(&search)
                    || b.sql.to_lowercase().contains(&search)
            })
            .collect();
        let count = bookmarks.len();
        let empty = match (total, count) {
            (0, _) => Some("No bookmarks yet. Press ⌘D in a query tab to save one."),
            (_, 0) => Some("No matching bookmarks."),
            _ => None,
        };
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                h_flex()
                    .h_9()
                    .px_3()
                    .items_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("BOOKMARKS"),
            )
            .when(total > 0, |this| {
                this.child(
                    div()
                        .px_2()
                        .pb_2()
                        .child(Input::new(&self.bookmark_search).small().cleanable(true)),
                )
            })
            .children(empty.map(|text| {
                div()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(text)
            }))
            .child(
                uniform_list(
                    "bookmarks",
                    count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|ix| this.render_bookmark_row(ix, &bookmarks[ix], cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .px_1()
                .flex_1(),
            )
    }

    fn render_history_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let entries = self.history.clone();
        let count = entries.len();
        let empty: Option<&str> = if self.active_db.is_none() {
            Some("Select a database to see its history.")
        } else if count == 0 && self.history_search.read(cx).value().trim().is_empty() {
            Some("Queries you run show up here.")
        } else if count == 0 {
            Some("No matching queries.")
        } else {
            None
        };
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                h_flex()
                    .h_9()
                    .px_3()
                    .items_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("HISTORY"),
            )
            .child(
                div()
                    .px_2()
                    .pb_2()
                    .child(Input::new(&self.history_search).small()),
            )
            .children(empty.map(|text| {
                div()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(text)
            }))
            .child(
                uniform_list(
                    "history",
                    count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|ix| this.render_history_row(ix, &entries[ix], cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .px_1()
                .flex_1(),
            )
    }

    fn render_history_row(
        &self,
        ix: usize,
        entry: &HistoryEntry,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let group: SharedString = format!("history-{ix}").into();
        let mut detail = format!("{} · {} ms", ago(entry.started_at), entry.duration_ms);
        if let (Some(rows), None) = (entry.rows, &entry.error) {
            detail.push_str(&format!(" · {rows} rows"));
        }
        let (open_sql, bookmark_sql) = (entry.sql.clone(), entry.sql.clone());
        h_flex()
            .id(("history-row", ix))
            .group(group.clone())
            .w_full()
            .h(px(BOOKMARK_ROW_HEIGHT))
            .px_2()
            .gap_1p5()
            .items_center()
            .rounded(theme.radius)
            .hover(|s| s.bg(theme.sidebar_accent))
            .cursor_pointer()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .child(one_line(&entry.sql, 200)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(if entry.error.is_some() {
                                theme.danger
                            } else {
                                theme.muted_foreground
                            })
                            .child(match &entry.error {
                                Some(e) => format!("{detail} · {}", one_line(e, 80)),
                                None => detail,
                            }),
                    ),
            )
            .child(
                div().invisible().group_hover(group, |s| s.visible()).child(
                    Button::new(("history-bookmark", ix))
                        .icon(Icon::new(Lucide::Bookmark))
                        .ghost()
                        .xsmall()
                        .tooltip("Bookmark")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_bookmark_dialog(None, bookmark_sql.clone(), None, window, cx)
                        })),
                ),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_sql("History", &open_sql, window, cx)
            }))
            .into_any_element()
    }

    fn render_bookmark_row(
        &self,
        ix: usize,
        bookmark: &Bookmark,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let preview: String = bookmark
            .sql
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let global = bookmark.database_id.is_none();
        let (open_id, run_id, edit_id, delete_id) = (
            bookmark.id.clone(),
            bookmark.id.clone(),
            bookmark.id.clone(),
            bookmark.id.clone(),
        );
        let edit_sql = bookmark.sql.clone();
        let metrics_id = bookmark.id.clone();
        let group: SharedString = format!("bookmark-{ix}").into();
        let scheduled = monitor::schedulable(bookmark).then(|| {
            let every = settings::current(cx).monitoring.interval();
            format!("Runs about every {}", monitor::human_duration(every))
        });
        let actions = h_flex()
            .gap_0p5()
            .invisible()
            .group_hover(group.clone(), |s| s.visible())
            .child(
                Button::new(("bookmark-metrics", ix))
                    .icon(Icon::new(Lucide::ChartLine))
                    .ghost()
                    .xsmall()
                    .tooltip("Duration and rows over time")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_metrics(&metrics_id, window, cx)
                    })),
            )
            .child(
                Button::new(("bookmark-run", ix))
                    .icon(IconName::Play)
                    .ghost()
                    .xsmall()
                    .tooltip("Open and run")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_bookmark(&run_id, true, window, cx)
                    })),
            )
            .child(
                Button::new(("bookmark-edit", ix))
                    .icon(Icon::new(Lucide::Pencil))
                    .ghost()
                    .xsmall()
                    .tooltip("Edit")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_bookmark_dialog(
                            Some(edit_id.clone()),
                            edit_sql.clone(),
                            None,
                            window,
                            cx,
                        )
                    })),
            )
            .child(
                Button::new(("bookmark-delete", ix))
                    .icon(IconName::Delete)
                    .ghost()
                    .xsmall()
                    .tooltip("Delete")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.delete_bookmark(&delete_id, window, cx)
                    })),
            );
        h_flex()
            .id(("bookmark-row", ix))
            .group(group)
            .w_full()
            .h(px(BOOKMARK_ROW_HEIGHT))
            .px_2()
            .gap_1p5()
            .items_center()
            .rounded(theme.radius)
            .hover(|s| s.bg(theme.sidebar_accent))
            .cursor_pointer()
            // How its result is shown: the grid or the chart type.
            .child(
                Icon::new(style_icon(bookmark.view.map(|v| v.style)))
                    .xsmall()
                    .text_color(theme.muted_foreground),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_1()
                            .text_sm()
                            .child(div().min_w_0().truncate().child(bookmark.name.clone()))
                            .when_some(scheduled, |this, tooltip| {
                                this.child(
                                    div()
                                        .id(("bookmark-scheduled", ix))
                                        .tooltip(move |window, cx| {
                                            Tooltip::new(tooltip.clone()).build(window, cx)
                                        })
                                        .child(
                                            Icon::new(Lucide::Clock)
                                                .xsmall()
                                                .text_color(theme.muted_foreground),
                                        ),
                                )
                            })
                            .when(global, |this| {
                                this.child(
                                    Icon::new(IconName::Globe)
                                        .xsmall()
                                        .text_color(theme.muted_foreground),
                                )
                            }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(preview),
                    ),
            )
            .child(actions)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_bookmark(&open_id, false, window, cx)
            }))
            .into_any_element()
    }

    /// Zed-style title bar content: database switcher, then user switcher.
    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let weak = cx.entity().downgrade();
        let db = self.active_database();
        let active_db = self.active_db.clone();
        let active_user = self.active_user.clone();

        let databases: Vec<(String, String)> = self
            .store
            .databases
            .iter()
            .map(|d| (d.id.clone(), d.display_name()))
            .collect();
        let db_menu = {
            let weak = weak.clone();
            Button::new("db-switcher")
                .label(db.map_or("No database".into(), |d| d.display_name()))
                .ghost()
                .small()
                .dropdown_menu(move |menu, _, _| {
                    database_menu(menu, &databases, active_db.as_deref(), &weak)
                })
        };

        let user_menu = db.map(|db| {
            let users: Vec<(String, String)> = db
                .users
                .iter()
                .map(|u| (u.id.clone(), u.user.clone()))
                .collect();
            let label = active_user
                .as_deref()
                .and_then(|id| db.user(id))
                .map_or("No user".into(), |u| u.user.clone());
            let db_id = db.id.clone();
            let active_user = active_user.clone();
            let weak = weak.clone();
            Button::new("user-switcher")
                .label(label)
                .icon(IconName::User)
                .ghost()
                .small()
                .text_color(theme.muted_foreground)
                .dropdown_menu(move |menu, _, _| {
                    user_menu(menu, &db_id, &users, active_user.as_deref(), &weak)
                })
        });

        let read_only = self.active_user_read_only(cx);
        let dot = self.active_conn().map(|c| match c.status {
            ConnStatus::Connected => theme.success,
            ConnStatus::Connecting => theme.warning,
            ConnStatus::Failed(_) => theme.danger,
        });

        h_flex()
            .gap_1()
            .items_center()
            .child(db_menu)
            .when_some(user_menu, |this, user_menu| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground.opacity(0.6))
                        .child("/"),
                )
                .child(user_menu)
            })
            .when_some(read_only, |this, on| {
                let color = if on {
                    theme.danger
                } else {
                    theme.muted_foreground
                };
                this.child(
                    Button::new("read-only")
                        .icon(
                            Icon::new(if on {
                                Lucide::PencilOff
                            } else {
                                Lucide::Pencil
                            })
                            .text_color(color),
                        )
                        .ghost()
                        .xsmall()
                        .tooltip(if on {
                            "Read-only mode is on: writes ask first. Click to turn off"
                        } else {
                            "Read-only mode is off. Click to turn on"
                        })
                        .on_click(
                            cx.listener(|this, _, window, cx| this.toggle_read_only(window, cx)),
                        ),
                )
            })
            .when_some(dot, |this, dot| {
                let weak = cx.entity().downgrade();
                this.child(
                    HoverCard::new("connection-details")
                        .anchor(Anchor::TopLeft)
                        .open_delay(Duration::from_millis(150))
                        .trigger(
                            // A larger hit area than the dot itself.
                            div()
                                .size_4()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(div().size_1p5().rounded_full().bg(dot)),
                        )
                        .content(move |_, _, cx| {
                            let sections = weak
                                .upgrade()
                                .map(|ws| ws.read(cx).connection_details())
                                .unwrap_or_default();
                            render_details(sections, cx)
                        }),
                )
            })
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.entity().downgrade();
        let tabs = self.tabs.iter().enumerate().map(|(ix, tab)| {
            let (id, pinned, bookmarked, has_sql, title, in_transaction) = {
                let t = tab.read(cx);
                (
                    t.id,
                    t.pinned,
                    t.bookmark_id.is_some(),
                    // A file tab (settings) holds no query to bookmark.
                    t.file_path().is_none() && !t.sql(cx).trim().is_empty(),
                    t.title.clone(),
                    t.in_transaction(),
                )
            };
            let unsaved = tab.read(cx).has_unsaved_changes(cx);
            let label: SharedString = if in_transaction || unsaved {
                format!("{title} •").into()
            } else {
                title
            };
            // The context menu can't wrap the `Tab` itself, so it sits on the content.
            let content = h_flex()
                .id(("tab-menu", id.0 as usize))
                .gap_1()
                .items_center()
                .when(pinned, |this| this.child(Icon::new(Lucide::Pin).xsmall()))
                .child(label.clone())
                .context_menu({
                    let weak = weak.clone();
                    move |menu, _, _| tab_menu(menu, id, pinned, bookmarked, has_sql, &weak)
                });
            Tab::new()
                .aria_label(label)
                .child(content)
                // Pinned tabs have no close button; the shortcut and menu still close them.
                .when(!pinned, |this| {
                    this.suffix(
                        Button::new(("close-tab", ix))
                            .icon(IconName::Close)
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(ix, window, cx)
                            })),
                    )
                })
                // Like browsers: closes when released on the tab it was pressed on.
                .on_mouse_down(
                    MouseButton::Middle,
                    cx.listener(move |this, _, _, _| this.middle_press = Some(id)),
                )
                .on_mouse_up(
                    MouseButton::Middle,
                    cx.listener(move |this, _, window, cx| {
                        if this.middle_press.take() == Some(id) && !pinned {
                            this.close_tab_by_id(id, window, cx);
                        }
                    }),
                )
        });
        TabBar::new("tabs")
            .children(tabs)
            .selected_index(self.active_tab)
            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                this.active_tab = *ix;
                if let Some(tab) = this.tabs.get(*ix).cloned() {
                    QueryTab::focus_editor(&tab, window, cx);
                }
                cx.notify();
            }))
            .suffix(
                Button::new("new-tab")
                    .icon(IconName::Plus)
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.add_tab("", window, cx);
                    })),
            )
    }

    /// Zed-style bottom bar: sidebar panels, then (planned) AI and schema-graph
    /// tools; the active query's timing on the right.
    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (query_text, running, chart) = self
            .tabs
            .get(self.active_tab)
            .map(|tab| {
                let t = tab.read(cx);
                let chart = (t.chart(), t.chart_shown());
                (
                    t.status_text(cx),
                    t.is_running(),
                    // File tabs (settings) have no results to chart.
                    t.file_path().is_none().then(|| (tab.clone(), chart)),
                )
            })
            .unwrap_or_default();
        let chart_button = chart.map(|(tab, (data, shown))| {
            chart_button(tab, data, shown, theme.chart_1, theme.muted_foreground)
        });
        let separator = || div().w(px(1.)).h_3p5().mx_1().bg(theme.border);
        let panels = SidebarView::ALL.map(|view| {
            let active = self.sidebar_open && self.sidebar_view == view;
            Button::new(("panel", view as usize))
                .icon(Icon::new(view.icon()))
                .ghost()
                .xsmall()
                .selected(active)
                .tooltip(view.label())
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_panel(view, cx)))
        });
        let planned = |id: &'static str, icon: Lucide, tooltip: &'static str| {
            Button::new(id)
                .icon(Icon::new(icon))
                .ghost()
                .xsmall()
                .disabled(true)
                .tooltip(tooltip)
        };
        h_flex()
            .h_7()
            .px_1()
            .gap_0p5()
            .items_center()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .text_xs()
            .text_color(theme.muted_foreground)
            .children(panels)
            .child(separator())
            .child(planned("ai-agent", Lucide::Bot, "Agent via ACP — planned"))
            .child(planned(
                "ai-assist",
                Lucide::Sparkles,
                "AI query help — planned",
            ))
            .child(separator())
            .child(planned(
                "schema-graph",
                Lucide::Network,
                "Schema graph (tables and their relations) — planned",
            ))
            .child(div().flex_1())
            .when(running, |this| this.child(Spinner::new().xsmall()))
            .child(div().pr_1().child(query_text))
            .children(chart_button)
            .child(div().w_1())
    }

    /// Bottom-bar panel button: shows that panel, or hides the sidebar if it is the
    /// one already shown (like Zed's dock buttons).
    fn toggle_panel(&mut self, view: SidebarView, cx: &mut Context<Self>) {
        if self.sidebar_open && self.sidebar_view == view {
            self.sidebar_open = false;
            cx.notify();
        } else {
            self.show_panel(view, cx);
        }
    }

    fn show_panel(&mut self, view: SidebarView, cx: &mut Context<Self>) {
        self.sidebar_open = true;
        self.sidebar_view = view;
        if view == SidebarView::History {
            self.refresh_history(cx);
        }
        cx.notify();
    }

    /// Palette Tab / Shift-Tab: opens a closed sidebar, else moves to the next or
    /// previous panel.
    fn cycle_panel(&mut self, step: isize, cx: &mut Context<Self>) {
        let view = if self.sidebar_open {
            self.sidebar_view.cycle(step)
        } else {
            self.sidebar_view
        };
        self.show_panel(view, cx);
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }

    /// The status dot's hover card: live state only (status, pool, what the schema
    /// holds); connection settings are in the database dialog.
    fn connection_details(&self) -> Vec<(&'static str, Vec<(&'static str, String)>)> {
        let Some(state) = self.active_conn() else {
            return Vec::new();
        };
        let mut sections = Vec::new();
        let status = match &state.status {
            ConnStatus::Connected => "Connected".to_string(),
            ConnStatus::Connecting => "Connecting…".to_string(),
            ConnStatus::Failed(e) => format!("Failed: {e}"),
        };
        sections.push(("Status", vec![("State", status)]));
        let s = state.stats;
        sections.push((
            "Pool",
            vec![
                ("Open", format!("{} of {}", s.open, s.max)),
                ("Idle", s.idle.to_string()),
                ("In transaction", s.pinned.to_string()),
            ],
        ));
        let tables = state.schema.tables();
        let views = tables.iter().filter(|t| t.kind == TableKind::View).count();
        sections.push((
            "Schema",
            vec![
                ("Tables", (tables.len() - views).to_string()),
                ("Views", views.to_string()),
                (
                    "Foreign keys",
                    state.schema.foreign_keys().len().to_string(),
                ),
            ],
        ));
        sections
    }
}

fn render_details(sections: Vec<(&'static str, Vec<(&'static str, String)>)>, cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .w(px(300.))
        .gap_3()
        .text_xs()
        .children(sections.into_iter().map(|(title, rows)| {
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(title.to_uppercase()),
                )
                .children(rows.into_iter().map(|(label, value)| {
                    h_flex()
                        .gap_2()
                        .items_start()
                        .child(
                            div()
                                .w(px(96.))
                                .flex_shrink_0()
                                .text_color(theme.muted_foreground)
                                .child(label),
                        )
                        .child(div().flex_1().min_w_0().child(value))
                }))
        }))
}

impl Drop for Workspace {
    /// The window closed: close its connections (the app itself keeps running).
    fn drop(&mut self) {
        for state in self.conns.values() {
            self.engine.send(Command::Disconnect { conn: state.id });
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.tabs.get(self.active_tab).cloned();
        v_flex()
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            // Any input postpones scheduled bookmark runs until it's quiet again.
            .capture_key_down(cx.listener(|this, _, _, _| this.monitor.touch()))
            .capture_any_mouse_down(cx.listener(|this, _, _, _| this.monitor.touch()))
            .on_mouse_move(cx.listener(|this, _, _, _| this.monitor.touch()))
            .on_scroll_wheel(cx.listener(|this, _, _, _| this.monitor.touch()))
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::close_active_tab))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::open_palette))
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(TitleBar::new().child(self.render_title_bar(cx)))
            .child({
                let content = v_flex()
                    .size_full()
                    .min_w_0()
                    .child(self.render_tabs(cx))
                    .child(match active {
                        Some(tab) => div().flex_1().min_h_0().child(tab).into_any_element(),
                        None => self.render_empty_state(cx),
                    });
                let body = if self.sidebar_open {
                    let this = cx.entity().downgrade();
                    h_resizable("layout")
                        .with_state(&self.layout)
                        .on_resize(move |state, _, cx| {
                            let width = state.read(cx).sizes().first().copied();
                            let _ = this.update(cx, |ws, _| {
                                if let Some(width) = width {
                                    ws.save_sidebar_width(width.as_f32());
                                }
                            });
                        })
                        .child(
                            resizable_panel()
                                .size(px(self.sidebar_width))
                                .size_range(px(SIDEBAR_MIN_WIDTH)..px(SIDEBAR_MAX_WIDTH))
                                .flex_none()
                                .child(self.render_sidebar(cx)),
                        )
                        .child(resizable_panel().child(content))
                        .into_any_element()
                } else {
                    content.into_any_element()
                };
                div().flex_1().min_h_0().child(body)
            })
            .child(self.render_status_bar(cx))
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

fn database_menu(
    mut menu: PopupMenu,
    databases: &[(String, String)],
    active: Option<&str>,
    workspace: &WeakEntity<Workspace>,
) -> PopupMenu {
    if !databases.is_empty() {
        menu = menu.label("Databases");
    }
    for (id, name) in databases {
        let (ws, id) = (workspace.clone(), id.clone());
        menu = menu.item(
            PopupMenuItem::new(name.clone())
                .checked(active == Some(id.as_str()))
                .on_click(move |_, window, cx| {
                    let _ = ws.update(cx, |ws, cx| ws.select_database(id.clone(), window, cx));
                }),
        );
    }
    if !databases.is_empty() {
        menu = menu.separator();
    }
    if let Some(id) = active {
        let (ws, id) = (workspace.clone(), id.to_string());
        menu = menu.item(
            PopupMenuItem::new("Edit database…")
                .icon(IconName::Settings)
                .on_click(move |_, window, cx| {
                    let _ = ws.update(cx, |ws, cx| {
                        ws.open_form(FormMode::EditDatabase, Some(id.clone()), None, window, cx)
                    });
                }),
        );
    }
    let ws = workspace.clone();
    menu.item(
        PopupMenuItem::new("Add database…")
            .icon(IconName::Plus)
            .on_click(move |_, window, cx| {
                let _ = ws.update(cx, |ws, cx| {
                    ws.open_form(FormMode::NewDatabase, None, None, window, cx)
                });
            }),
    )
}

fn user_menu(
    mut menu: PopupMenu,
    db_id: &str,
    users: &[(String, String)],
    active: Option<&str>,
    workspace: &WeakEntity<Workspace>,
) -> PopupMenu {
    if !users.is_empty() {
        menu = menu.label("Users");
    }
    for (id, name) in users {
        let (ws, db_id, id) = (workspace.clone(), db_id.to_string(), id.clone());
        menu = menu.item(
            PopupMenuItem::new(name.clone())
                .checked(active == Some(id.as_str()))
                .on_click(move |_, window, cx| {
                    let _ = ws.update(cx, |ws, cx| {
                        ws.select_user(db_id.clone(), id.clone(), window, cx)
                    });
                }),
        );
    }
    if !users.is_empty() {
        menu = menu.separator();
    }
    if let Some(id) = active {
        let (ws, db_id, id) = (workspace.clone(), db_id.to_string(), id.to_string());
        menu = menu.item(
            PopupMenuItem::new("Edit user…")
                .icon(IconName::Settings)
                .on_click(move |_, window, cx| {
                    let _ = ws.update(cx, |ws, cx| {
                        ws.open_form(
                            FormMode::User,
                            Some(db_id.clone()),
                            Some(id.clone()),
                            window,
                            cx,
                        )
                    });
                }),
        );
    }
    let (ws, db_id) = (workspace.clone(), db_id.to_string());
    menu.item(
        PopupMenuItem::new("Add user…")
            .icon(IconName::Plus)
            .on_click(move |_, window, cx| {
                let _ = ws.update(cx, |ws, cx| {
                    ws.open_form(FormMode::User, Some(db_id.clone()), None, window, cx)
                });
            }),
    )
}

fn suggestions_menu(
    mut menu: PopupMenu,
    table: &str,
    list: Vec<QuerySuggestion>,
    workspace: &WeakEntity<Workspace>,
) -> PopupMenu {
    let (learned, generated): (Vec<_>, Vec<_>) = list.into_iter().partition(|q| q.learned);
    for (heading, items) in [("Your queries", learned), ("Suggested", generated)] {
        if items.is_empty() {
            continue;
        }
        menu = menu.label(heading);
        for q in items {
            let (ws, table, sql) = (workspace.clone(), table.to_string(), q.sql);
            menu = menu.item(PopupMenuItem::new(q.title).on_click(move |_, window, cx| {
                let _ = ws.update(cx, |ws, cx| ws.open_sql(&table, &sql, window, cx));
            }));
        }
    }
    menu.max_w(px(420.))
}

/// "5 min ago"-style age of a unix-millisecond timestamp.
fn ago(ms: i64) -> String {
    let secs = (now_ms() - ms).max(0) / 1000;
    match secs {
        0..60 => "just now".into(),
        60..3_600 => format!("{} min ago", secs / 60),
        3_600..86_400 => format!("{} h ago", secs / 3_600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

/// Right-click menu of a table or view in the sidebar.
fn tab_menu(
    menu: PopupMenu,
    tab: TabId,
    pinned: bool,
    bookmarked: bool,
    has_sql: bool,
    workspace: &WeakEntity<Workspace>,
) -> PopupMenu {
    let item = |label: &str, f: fn(&mut Workspace, TabId, &mut Window, &mut Context<Workspace>)| {
        let ws = workspace.clone();
        PopupMenuItem::new(label.to_string()).on_click(move |_, window, cx| {
            let _ = ws.update(cx, |ws, cx| f(ws, tab, window, cx));
        })
    };
    let menu = menu
        .item(item("Close", Workspace::close_tab_by_id))
        .item(item(
            if pinned { "Unpin" } else { "Pin" },
            |ws, tab, _, cx| ws.toggle_tab_pin(tab, cx),
        ));
    if bookmarked {
        menu.item(item("Remove bookmark", Workspace::toggle_tab_bookmark))
    } else if has_sql {
        menu.item(item("Bookmark query…", Workspace::toggle_tab_bookmark))
    } else {
        menu
    }
}

fn table_menu(
    menu: PopupMenu,
    table: &TableInfo,
    workspace: &WeakEntity<Workspace>,
    cx: &App,
) -> PopupMenu {
    let item =
        |label: &str, f: fn(&mut Workspace, &TableInfo, &mut Window, &mut Context<Workspace>)| {
            let (ws, table) = (workspace.clone(), table.clone());
            PopupMenuItem::new(label.to_string()).on_click(move |_, window, cx| {
                let _ = ws.update(cx, |ws, cx| f(ws, &table, window, cx));
            })
        };
    let name = workspace
        .upgrade()
        .and_then(|ws| ws.read(cx).qualified_name(table))
        .unwrap_or_else(|| table.name.clone());
    let copy = |label: &str, text: String| {
        PopupMenuItem::new(label.to_string()).on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
        })
    };
    menu.item(item("Select first 100 rows", Workspace::open_table))
        .item(item("Count rows", Workspace::count_table))
        .separator()
        .item(copy("Copy name", table.name.clone()))
        .item(copy("Copy qualified name", name))
}

/// Status-bar button of the active tab's result: tinted when the result can be
/// charted, with a menu of the table, charts of all value columns and charts of a
/// single one; the chosen chart's name next to the icon. Disabled, with the reason,
/// otherwise.
fn chart_button(
    tab: Entity<QueryTab>,
    data: Result<Rc<ChartData>, SharedString>,
    shown: Option<ChartView>,
    available: Hsla,
    muted: Hsla,
) -> AnyElement {
    let data = match data {
        Ok(data) => data,
        Err(reason) => {
            return Button::new("chart")
                .icon(Icon::new(Lucide::ChartColumn).text_color(muted))
                .ghost()
                .xsmall()
                .disabled(true)
                .tooltip(reason)
                .into_any_element();
        }
    };
    let button = Button::new("chart")
        .icon(Icon::new(style_icon(shown.map(|v| v.style))).text_color(available))
        .xsmall()
        .selected(shown.is_some())
        .when_some(shown, |b, view| b.label(data.view_label(view)));
    let button = if data.grouped && shown.is_none() {
        button.outline().tooltip("Show as chart (grouped query)")
    } else {
        button.ghost().tooltip("Show as chart")
    };
    button
        .dropdown_menu(move |mut menu, window, cx| {
            let item = |label: String, view: Option<ChartView>| {
                let tab = tab.clone();
                PopupMenuItem::new(label)
                    .icon(Icon::new(style_icon(view.map(|v| v.style))))
                    .checked(shown == view)
                    .on_click(move |_, _, cx| tab.update(cx, |t, cx| t.set_chart_view(view, cx)))
            };
            menu = menu.item(item("Table".into(), None));
            let multi = data.multi_styles();
            if !multi.is_empty() {
                menu = menu.separator().label("All columns");
                for style in multi {
                    let view = ChartView {
                        style,
                        series: None,
                    };
                    menu = menu.item(item(style.label().into(), Some(view)));
                }
            }
            menu = menu.separator().label("Single column");
            for style in data.single_styles() {
                let label = style.single_label();
                if data.series.len() < 2 {
                    let view = ChartView {
                        style,
                        series: Some(0),
                    };
                    menu = menu.item(item(label.into(), Some(view)));
                    continue;
                }
                // Several value columns: pick which one.
                let columns: Vec<(String, ChartView)> = data
                    .series
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        let view = ChartView {
                            style,
                            series: Some(i),
                        };
                        (s.name.clone(), view)
                    })
                    .collect();
                let tab = tab.clone();
                menu = menu.submenu_with_icon(
                    Some(Icon::new(style_icon(Some(style)))),
                    label,
                    window,
                    cx,
                    move |menu, _, _| {
                        columns.iter().fold(menu, |menu, (name, view)| {
                            let (tab, view) = (tab.clone(), *view);
                            menu.item(
                                PopupMenuItem::new(name.clone())
                                    .checked(shown == Some(view))
                                    .on_click(move |_, _, cx| {
                                        tab.update(cx, |t, cx| t.set_chart_view(Some(view), cx))
                                    }),
                            )
                        })
                    },
                );
            }
            menu
        })
        .into_any_element()
}

fn style_icon(style: Option<ChartStyle>) -> Lucide {
    match style {
        None => Lucide::Table,
        Some(ChartStyle::Line) => Lucide::ChartLine,
        Some(ChartStyle::Area | ChartStyle::StackedArea) => Lucide::ChartArea,
        Some(ChartStyle::Bars) => Lucide::ChartColumn,
        Some(ChartStyle::StackedBars) => Lucide::ChartColumnStacked,
    }
}

/// Command palette header: one pill per sidebar panel, the shown one selected.
/// A click (or Tab / Shift-Tab) switches the sidebar; the palette stays open.
fn palette_pills(workspace: &WeakEntity<Workspace>, cx: &App) -> AnyElement {
    let Some(ws) = workspace.upgrade() else {
        return div().into_any_element();
    };
    let ws = ws.read(cx);
    let (open, current) = (ws.sidebar_open, ws.sidebar_view);
    let theme = cx.theme();
    h_flex()
        .px_3()
        .pt_2()
        .gap_1()
        .items_center()
        .children(SidebarView::ALL.map(|view| {
            let workspace = workspace.clone();
            Button::new(("palette-panel", view as usize))
                .icon(Icon::new(view.icon()))
                .label(view.label())
                .xsmall()
                .ghost()
                .selected(open && view == current)
                .on_click(move |_, window, cx| {
                    let _ = workspace.update(cx, |ws, cx| ws.show_panel(view, cx));
                    window.refresh();
                })
        }))
        .child(div().flex_1())
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("Tab to switch"),
        )
        .into_any_element()
}
