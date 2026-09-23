//! Root view: sidebar (connections + schema), query tabs, status bar.
//! Routes engine events to the tab that started the query.

use std::collections::{HashMap, HashSet};

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Root, Sizable as _, TitleBar, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    notification::Notification,
    tab::{Tab, TabBar},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rusdb_core::{ConnId, DriverKind, Profile, QueryId, TabId, TableInfo, TableKind};
use rusdb_engine::{Command, EngineHandle, Event, PoolStats, QueryEvent};

use crate::{
    CloseTab, NewTab,
    connection_form::ConnectionForm,
    query_tab::{QueryTab, TabConn, TabEvent},
    store::Store,
};

const SIDEBAR_WIDTH: f32 = 260.;
const ROW_HEIGHT: f32 = 26.;

#[derive(Clone, PartialEq)]
enum ConnStatus {
    Connecting,
    Connected,
    Failed(SharedString),
}

struct ConnState {
    id: ConnId,
    status: ConnStatus,
    tables: Vec<TableInfo>,
    stats: PoolStats,
}

/// Flattened sidebar entry, so the tree renders through a virtualized list.
#[derive(Clone)]
enum SidebarRow {
    Profile {
        profile_ix: usize,
    },
    Schema {
        profile_ix: usize,
        schema: SharedString,
        expanded: bool,
    },
    Table {
        profile_ix: usize,
        table: TableInfo,
    },
}

pub struct Workspace {
    engine: EngineHandle,
    store: Store,
    /// Connection state by profile id.
    conns: HashMap<String, ConnState>,
    conn_profiles: HashMap<ConnId, String>,
    expanded_profiles: HashSet<String>,
    expanded_schemas: HashSet<(String, SharedString)>,
    active_profile: Option<String>,
    tabs: Vec<Entity<QueryTab>>,
    active_tab: usize,
    queries: HashMap<QueryId, WeakEntity<QueryTab>>,
    sidebar_rows: Vec<SidebarRow>,
    next_id: u64,
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

        let mut this = Self {
            engine,
            store: Store::load(),
            conns: HashMap::new(),
            conn_profiles: HashMap::new(),
            expanded_profiles: HashSet::new(),
            expanded_schemas: HashSet::new(),
            active_profile: None,
            tabs: Vec::new(),
            active_tab: 0,
            queries: HashMap::new(),
            sidebar_rows: Vec::new(),
            next_id: 0,
            focus_handle: cx.focus_handle(),
            _subscriptions: Vec::new(),
            _pump: pump,
        };
        this.active_profile = this.store.profiles.first().map(|p| p.id.clone());
        this.add_tab("", window, cx);
        this
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn profile(&self, id: &str) -> Option<&Profile> {
        self.store.profiles.iter().find(|p| p.id == id)
    }

    /// The tab connection for a profile, if it is connected (or connecting).
    fn tab_conn(&self, profile_id: &str) -> Option<TabConn> {
        let profile = self.profile(profile_id)?;
        let state = self.conns.get(profile_id)?;
        Some(TabConn {
            id: state.id,
            name: display_name(profile).into(),
            driver: profile.driver,
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
        let conn = self
            .active_profile
            .as_deref()
            .and_then(|p| self.tab_conn(p));
        let engine = self.engine.clone();
        let tab = cx.new(|cx| QueryTab::new(id, conn, sql, engine, window, cx));
        self._subscriptions
            .push(cx.subscribe_in(&tab, window, Self::on_tab_event));
        self.tabs.push(tab.clone());
        self.active_tab = self.tabs.len() - 1;
        QueryTab::focus_editor(&tab, window, cx);
        cx.notify();
        tab
    }

    fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        self.add_tab("", window, cx);
    }

    fn close_active_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab(self.active_tab, window, cx);
    }

    fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
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
            self.add_tab("", window, cx);
        } else {
            self.active_tab = self.active_tab.min(self.tabs.len() - 1);
        }
        cx.notify();
    }

    fn on_tab_event(
        &mut self,
        tab: &Entity<QueryTab>,
        event: &TabEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TabEvent::Run(sql) => {
                let Some(conn) = tab.read(cx).conn.clone() else {
                    return;
                };
                let query = QueryId(self.next_id());
                self.queries.insert(query, tab.downgrade());
                self.engine.send(Command::Execute {
                    query,
                    tab: tab.read(cx).id,
                    conn: conn.id,
                    sql: sql.clone(),
                });
                tab.update(cx, |t, cx| t.started(query, cx));
            }
        }
    }

    // ---- connections -----------------------------------------------------------

    fn connect(&mut self, profile_id: &str, cx: &mut Context<Self>) {
        let Some(profile) = self.profile(profile_id).cloned() else {
            return;
        };
        let id = match self.conns.get(profile_id) {
            Some(state) => state.id,
            None => ConnId(self.next_id()),
        };
        let password = self.store.password(profile_id);
        self.conns.insert(
            profile_id.to_string(),
            ConnState {
                id,
                status: ConnStatus::Connecting,
                tables: Vec::new(),
                stats: PoolStats::default(),
            },
        );
        self.conn_profiles.insert(id, profile_id.to_string());
        self.engine.send(Command::Connect {
            conn: id,
            driver: profile.driver,
            cfg: rusdb_core::ConnectConfig::from_profile(&profile, password),
        });
        cx.notify();
    }

    /// Sidebar click on a profile: make it active, connect if needed, and use it for
    /// the current tab.
    fn select_profile(&mut self, profile_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let needs_connect = !matches!(
            self.conns.get(&profile_id).map(|c| &c.status),
            Some(ConnStatus::Connected | ConnStatus::Connecting)
        );
        if needs_connect {
            self.connect(&profile_id, cx);
        }
        if !self.expanded_profiles.insert(profile_id.clone()) && !needs_connect {
            self.expanded_profiles.remove(&profile_id);
        }
        self.active_profile = Some(profile_id.clone());
        self.assign_active_tab(&profile_id, window, cx);
        cx.notify();
    }

    fn assign_active_tab(&mut self, profile_id: &str, _: &mut Window, cx: &mut Context<Self>) {
        let Some(conn) = self.tab_conn(profile_id) else {
            return;
        };
        if let Some(tab) = self.tabs.get(self.active_tab) {
            if tab.read(cx).is_running() {
                return;
            }
            tab.update(cx, |t, cx| t.set_conn(conn, cx));
        }
    }

    fn open_table(
        &mut self,
        profile_ix: usize,
        table: &TableInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = self.store.profiles.get(profile_ix).cloned() else {
            return;
        };
        self.active_profile = Some(profile.id.clone());
        let q = |s: &str| match profile.driver {
            DriverKind::Postgres => format!("\"{}\"", s.replace('"', "\"\"")),
            DriverKind::Mysql => format!("`{}`", s.replace('`', "``")),
        };
        let sql = format!(
            "SELECT * FROM {}.{} LIMIT 100",
            q(&table.schema),
            q(&table.name)
        );
        let tab = self.add_tab(&sql, window, cx);
        tab.update(cx, |t, cx| {
            t.title = table.name.clone().into();
            cx.emit(TabEvent::Run(sql));
        });
    }

    fn open_connection_dialog(
        &mut self,
        profile_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let profile = profile_id
            .as_deref()
            .and_then(|id| self.profile(id))
            .cloned();
        let form = cx.new(|cx| ConnectionForm::new(profile.as_ref(), window, cx));
        let workspace = cx.entity().downgrade();
        let title = if profile.is_some() {
            "Edit connection"
        } else {
            "New connection"
        };
        window.open_dialog(cx, move |dialog, _, _| {
            let save = {
                let form = form.clone();
                let workspace = workspace.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let Some((profile, password)) = form.update(cx, |f, cx| f.submit(cx)) else {
                        return;
                    };
                    let _ = workspace
                        .update(cx, |ws, cx| ws.save_profile(profile, password, window, cx));
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
                    .child(
                        Button::new("save")
                            .label("Save & connect")
                            .primary()
                            .on_click(save),
                    ),
            )
        });
    }

    fn save_profile(
        &mut self,
        profile: Profile,
        password: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = profile.id.clone();
        if let Err(e) = self.store.upsert(profile, password) {
            window.push_notification(
                Notification::error(format!("Could not save profile: {e:#}")),
                cx,
            );
        }
        // (Re)connect with the new settings. The ConnId is kept, so tabs using this
        // profile keep working; the engine replaces the old pool.
        self.connect(&id, cx);
        self.expanded_profiles.insert(id.clone());
        self.active_profile = Some(id.clone());
        self.assign_active_tab(&id, window, cx);
    }

    fn on_engine_event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            Event::Query { query, event } => {
                let finished = matches!(event, QueryEvent::Finished { .. });
                if let Some(tab) = self.queries.get(&query).and_then(|t| t.upgrade()) {
                    tab.update(cx, |t, cx| t.on_query_event(event, cx));
                }
                if finished {
                    self.queries.remove(&query);
                }
            }
            Event::Connected { conn } => {
                if let Some(state) = self.conn_state_mut(conn) {
                    state.status = ConnStatus::Connected;
                }
                // Tabs without a connection pick up the newly connected one.
                if let Some(profile_id) = self.conn_profiles.get(&conn).cloned()
                    && let Some(tc) = self.tab_conn(&profile_id)
                {
                    for tab in &self.tabs {
                        if tab.read(cx).conn.is_none() {
                            tab.update(cx, |t, cx| t.set_conn(tc.clone(), cx));
                        }
                    }
                }
            }
            Event::ConnectFailed { conn, error } => {
                if let Some(state) = self.conn_state_mut(conn) {
                    state.status = ConnStatus::Failed(error.clone().into());
                }
                window.push_notification(Notification::error(error), cx);
            }
            Event::Schema { conn, result } => match result {
                Ok(tables) => {
                    if let Some(state) = self.conn_state_mut(conn) {
                        state.tables = tables;
                    }
                }
                Err(e) => window.push_notification(Notification::error(e), cx),
            },
            Event::PoolStats { conn, stats } => {
                if let Some(state) = self.conn_state_mut(conn) {
                    state.stats = stats;
                }
            }
        }
        cx.notify();
    }

    fn conn_state_mut(&mut self, conn: ConnId) -> Option<&mut ConnState> {
        let profile_id = self.conn_profiles.get(&conn)?;
        self.conns.get_mut(profile_id).filter(|s| s.id == conn)
    }

    // ---- rendering ---------------------------------------------------------------

    fn build_sidebar_rows(&self) -> Vec<SidebarRow> {
        let mut rows = Vec::new();
        for (profile_ix, profile) in self.store.profiles.iter().enumerate() {
            rows.push(SidebarRow::Profile { profile_ix });
            if !self.expanded_profiles.contains(&profile.id) {
                continue;
            }
            let Some(state) = self.conns.get(&profile.id) else {
                continue;
            };
            let mut current_schema: Option<&str> = None;
            let mut expanded = false;
            for table in &state.tables {
                if current_schema != Some(table.schema.as_str()) {
                    current_schema = Some(&table.schema);
                    let schema: SharedString = table.schema.clone().into();
                    expanded = self
                        .expanded_schemas
                        .contains(&(profile.id.clone(), schema.clone()));
                    rows.push(SidebarRow::Schema {
                        profile_ix,
                        schema,
                        expanded,
                    });
                }
                if expanded {
                    rows.push(SidebarRow::Table {
                        profile_ix,
                        table: table.clone(),
                    });
                }
            }
        }
        rows
    }

    fn render_sidebar_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let row = h_flex()
            .id(("sidebar-row", ix))
            .w_full()
            .h(px(ROW_HEIGHT))
            .px_2()
            .gap_1p5()
            .items_center()
            .text_sm()
            .rounded(theme.radius)
            .hover(|s| s.bg(theme.sidebar_accent))
            .cursor_pointer();

        match self.sidebar_rows[ix].clone() {
            SidebarRow::Profile { profile_ix } => {
                let profile = &self.store.profiles[profile_ix];
                let state = self.conns.get(&profile.id);
                let dot = match state.map(|s| &s.status) {
                    Some(ConnStatus::Connected) => theme.success,
                    Some(ConnStatus::Connecting) => theme.warning,
                    Some(ConnStatus::Failed(_)) => theme.danger,
                    None => theme.muted_foreground.opacity(0.4),
                };
                let active = self.active_profile.as_deref() == Some(profile.id.as_str());
                let expanded = self.expanded_profiles.contains(&profile.id);
                let id = profile.id.clone();
                let edit_id = profile.id.clone();
                let connected = matches!(state.map(|s| &s.status), Some(ConnStatus::Connected));
                let conn_id = state.map(|s| s.id);
                row.when(active, |r| r.bg(theme.sidebar_accent))
                    .child(chevron(expanded))
                    .child(div().size_2().flex_shrink_0().rounded_full().bg(dot))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(display_name(profile)),
                    )
                    .when_some(conn_id.filter(|_| connected), |r, conn| {
                        r.child(
                            Button::new(("refresh", ix))
                                .icon(IconName::RotateCw)
                                .ghost()
                                .xsmall()
                                .tooltip("Refresh schema")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.engine.send(Command::RefreshSchema { conn });
                                })),
                        )
                    })
                    .child(
                        Button::new(("edit", ix))
                            .icon(IconName::Settings)
                            .ghost()
                            .xsmall()
                            .tooltip("Edit connection")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.open_connection_dialog(Some(edit_id.clone()), window, cx)
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_profile(id.clone(), window, cx)
                    }))
                    .into_any_element()
            }
            SidebarRow::Schema {
                profile_ix,
                schema,
                expanded,
            } => {
                let key = (self.store.profiles[profile_ix].id.clone(), schema.clone());
                row.pl_5()
                    .child(chevron(expanded))
                    .child(
                        Icon::new(if expanded {
                            IconName::FolderOpen
                        } else {
                            IconName::FolderClosed
                        })
                        .xsmall(),
                    )
                    .child(div().min_w_0().truncate().child(schema))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.expanded_schemas.remove(&key) {
                            this.expanded_schemas.insert(key.clone());
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            }
            SidebarRow::Table { profile_ix, table } => {
                let icon = match table.kind {
                    TableKind::Table => IconName::File,
                    TableKind::View => IconName::Eye,
                };
                let name = table.name.clone();
                row.pl(px(52.))
                    .child(Icon::new(icon).xsmall().text_color(theme.muted_foreground))
                    .child(div().min_w_0().truncate().child(name))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_table(profile_ix, &table, window, cx)
                    }))
                    .into_any_element()
            }
        }
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        self.sidebar_rows = self.build_sidebar_rows();
        let theme = cx.theme();
        let count = self.sidebar_rows.len();
        v_flex()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_shrink_0()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                h_flex()
                    .h_9()
                    .px_3()
                    .items_center()
                    .justify_between()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("CONNECTIONS")
                    .child(
                        Button::new("add-connection")
                            .icon(IconName::Plus)
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_connection_dialog(None, window, cx)
                            })),
                    ),
            )
            .when(count == 0, |this| {
                this.child(
                    div()
                        .px_3()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("No connections yet. Click + to add one."),
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
                .px_1()
                .flex_1(),
            )
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self.tabs.iter().enumerate().map(|(ix, tab)| {
            let t = tab.read(cx);
            let label: SharedString = if t.in_transaction() {
                format!("{} •", t.title).into()
            } else {
                t.title.clone()
            };
            Tab::new().label(label).suffix(
                Button::new(("close-tab", ix))
                    .icon(IconName::Close)
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_tab(ix, window, cx)
                    })),
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

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let text: SharedString = self
            .active_profile
            .as_ref()
            .and_then(|id| Some((self.profile(id)?, self.conns.get(id)?)))
            .map(|(profile, state)| {
                let s = state.stats;
                let status = match &state.status {
                    ConnStatus::Connecting => "connecting…".to_string(),
                    ConnStatus::Failed(_) => "connection failed".to_string(),
                    ConnStatus::Connected => format!(
                        "{}/{} connections open · {} idle · {} in transaction",
                        s.open, s.max, s.idle, s.pinned
                    ),
                };
                format!("{} — {status}", display_name(profile)).into()
            })
            .unwrap_or_else(|| "Not connected".into());
        h_flex()
            .h_6()
            .px_3()
            .items_center()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(text)
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.tabs.get(self.active_tab).cloned();
        v_flex()
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::new_tab))
            .on_action(cx.listener(Self::close_active_tab))
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(TitleBar::new().child(div().text_sm().child("rusdb")))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_sidebar(cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.render_tabs(cx))
                            .children(active.map(|tab| div().flex_1().min_h_0().child(tab))),
                    ),
            )
            .child(self.render_status_bar(cx))
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

fn chevron(expanded: bool) -> impl IntoElement {
    Icon::new(if expanded {
        IconName::ChevronDown
    } else {
        IconName::ChevronRight
    })
    .xsmall()
}

fn display_name(p: &Profile) -> String {
    if !p.name.is_empty() {
        return p.name.clone();
    }
    let db = if p.database.is_empty() {
        String::new()
    } else {
        format!("/{}", p.database)
    };
    format!("{}@{}{}", p.user, p.host, db)
}
