//! Root view: title bar (database + user switchers, like Zed's project/branch),
//! schema sidebar of the active connection, query tabs, status bar.
//! Routes engine events to the tab that started the query.
//!
//! This file holds the state and the layout; each topic adds its own
//! `impl Workspace` in a submodule (connections, tabs, safety, bookmarks, …).

mod assist;
mod bars;
mod bookmarks;
mod command_palette;
mod connections;
mod empty_state;
mod history;
mod safety;
mod scheduled;
mod schema;
mod sidebar;
mod tabs;

// Shared by the status bar and the bookmarks panel.
use bars::style_icon;

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
use tangul_data::Data;
use tangul_data::local::{Bookmark, HistoryEntry, NewHistory, bare_sql};
use tangul_data::metrics;
use tangul_engine::{
    Approval, Caller, Command, Decision, EngineHandle, Event, Guard, PoolStats, QueryEvent,
    QueryOptions, QueryOutput,
};
use tangul_service::connections::{Connections, Purpose};
use tangul_service::schedule;

use crate::{
    CloseTab, NewTab, NextSidebarPanel, OpenPalette, OpenSettings, PrevSidebarPanel, ToggleSidebar,
    assets::Lucide,
    bookmark_form::BookmarkForm,
    completion::{AssistData, SharedAssist, SqlCompletion},
    connection::{ConnStatus, Connection},
    connection_form::{ConnectionForm, FormMode, FormOutput},
    monitor::{self, Monitor},
    palette::{self, PaletteAction, PaletteKind},
    query_tab::{QueryTab, TabEvent},
    settings,
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
    /// Saved databases, local.db and metrics.db.
    data: Data,
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
    /// The tabs' connections by user id; tabs point to the same entities.
    conns: HashMap<String, Entity<Connection>>,
    /// Every open connection and what it is for (tabs, scheduled runs, files).
    connections: Connections,
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
    /// Scheduled bookmark runs on hidden connections.
    monitor: Monitor,
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

        let data = Data::open();
        data.metrics
            .prune(&settings::current(cx).monitoring, now_ms());
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
        let sidebar_width = data
            .local
            .setting(SIDEBAR_WIDTH_SETTING)
            .and_then(|w| w.parse::<f32>().ok())
            .unwrap_or(SIDEBAR_WIDTH)
            .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
        let mut this = Self {
            engine,
            data,
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
            connections: Connections::default(),
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
            monitor: Monitor::default(),
            _monitor_tick: monitor_tick,
            focus_handle: cx.focus_handle(),
            _subscriptions: vec![search_sub, bookmark_sub, schema_sub],
            _pump: pump,
        };
        // Reopen the connection used last (else the first one) right away.
        let last = this.data.local.setting(LAST_USER_SETTING);
        let start = last
            .as_deref()
            .and_then(|id| this.find_user(id))
            .map(|(db, u)| (db.id.clone(), Some(u.id.clone())))
            .or_else(|| {
                let db = this.data.profiles.databases.first()?;
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
        self.data.profiles.database(self.active_db.as_deref()?)
    }

    fn active_conn<'a>(&self, cx: &'a App) -> Option<&'a Connection> {
        Some(self.conns.get(self.active_user.as_deref()?)?.read(cx))
    }

    fn find_user(&self, user_id: &str) -> Option<(&Database, &DbUser)> {
        self.data.profiles.user(user_id)
    }

    /// The connection of a user, if it is connected (or connecting).
    fn tab_conn(&self, user_id: &str) -> Option<Entity<Connection>> {
        self.conns.get(user_id).cloned()
    }
}

impl Drop for Workspace {
    /// The window closed: close its connections (the app itself keeps running).
    fn drop(&mut self) {
        for user_id in self.conns.keys() {
            if let Some(conn) = self.connections.get(Purpose::Tab, user_id) {
                self.engine.send(Command::Disconnect { conn });
            }
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
                    // No tab bar without tabs: the empty state fills the area.
                    .when(!self.tabs.is_empty(), |this| {
                        this.child(self.render_tabs(cx))
                    })
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
