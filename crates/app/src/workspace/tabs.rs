//! Tabs: opening, closing, pinning, running their SQL, and the tab bar.

use super::*;

impl Workspace {
    pub(super) fn add_tab(
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
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        self.add_tab("", window, cx);
    }

    pub(super) fn close_active_tab(
        &mut self,
        _: &CloseTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_tab(self.active_tab, window, cx);
    }

    pub(super) fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn ask_to_save(
        &mut self,
        tab: Entity<QueryTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
    pub(super) fn remove_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(ix);
        let tab = tab.read(cx);
        if let Some(conn) = tab.conn_id(cx) {
            // Drops a pinned session (open transaction is rolled back by the server).
            self.engine.send(Command::ReleaseTab { tab: tab.id, conn });
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

    pub(super) fn tab_index(&self, id: TabId, cx: &App) -> Option<usize> {
        self.tabs.iter().position(|t| t.read(cx).id == id)
    }

    pub(super) fn close_tab_by_id(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.tab_index(id, cx) {
            self.close_tab(ix, window, cx);
        }
    }

    pub(super) fn toggle_tab_pin(&mut self, id: TabId, cx: &mut Context<Self>) {
        if let Some(ix) = self.tab_index(id, cx) {
            self.tabs[ix].update(cx, |t, _| t.toggle_pin());
            cx.notify();
        }
    }

    /// Removes the tab's bookmark, or asks for a name and bookmarks its query.
    pub(super) fn toggle_tab_bookmark(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    pub(super) fn on_tab_event(
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
                let Some(conn) = tab.read(cx).conn_id(cx) else {
                    return;
                };
                let request = self.next_id();
                self.validations
                    .insert(request, (tab.downgrade(), *generation));
                self.engine.send(Command::Validate {
                    request,
                    conn,
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
                let Some(conn) = tab.read(cx).conn.clone() else {
                    return;
                };
                let state = conn.read(cx);
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
                let Some(conn) = tab.read(cx).conn_id(cx) else {
                    return;
                };
                match self.decide_for_tab(conn, sql, cx) {
                    Decision::Run(_) => self.execute(tab, sql, Approval::None, cx),
                    Decision::Confirm { guard, .. } => {
                        self.hold_back(tab.clone(), sql.clone(), guard, window, cx)
                    }
                    Decision::Refuse(reason) => {
                        window.push_notification(Notification::error(reason), cx)
                    }
                }
            }
        }
    }

    /// Sends `sql` to the engine on the tab's connection, as the user; `approval` is
    /// the dialog's go-ahead, if one was needed.
    pub(super) fn execute(
        &mut self,
        tab: &Entity<QueryTab>,
        sql: &str,
        approval: Approval,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = tab.read(cx).conn_id(cx) else {
            return;
        };
        let query = QueryId(self.next_id());
        let user_id = self
            .connections
            .tab_user(conn)
            .unwrap_or_default()
            .to_string();
        // Running a bookmarked query: the tab shows the bookmark's name, and its
        // bookmark button edits that bookmark. Only when the whole editor ran, since
        // the button saves the whole editor.
        let whole_editor = bare_sql(&tab.read(cx).sql(cx)) == bare_sql(sql);
        let bookmark = self
            .find_user(&user_id)
            .filter(|_| whole_editor)
            .and_then(|(db, _)| self.data.local.bookmark_for_sql(&db.id, sql));
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
        let user = self.conn_user_name(conn);
        // A write while read-only mode is lifted restarts the wait until it is back.
        if let Some(user) = user.filter(|u| self.relock.contains_key(u))
            && guard::check(sql).iter().any(|c| c.writes)
        {
            self.schedule_relock(&user, cx);
        }
        self.engine.send(Command::Execute {
            query,
            tab: tab.read(cx).id,
            conn,
            sql: sql.to_string(),
            // The engine's gate decides read-only mode and the time limit.
            caller: Caller::User,
            approval,
        });
        tab.update(cx, |t, cx| t.started(query, sql, cx));
    }

    /// Runs a query derived from the tab's last result (filtered, sorted, next
    /// page). It only reads, so it skips the safety checks of a user run.
    pub(super) fn execute_derived(
        &mut self,
        tab: &Entity<QueryTab>,
        sql: &str,
        append: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = tab.read(cx).conn_id(cx) else {
            return;
        };
        let query = QueryId(self.next_id());
        self.queries.insert(
            query,
            RunningQuery {
                tab: tab.downgrade(),
                sql: sql.to_string(),
                user_id: self
                    .connections
                    .tab_user(conn)
                    .unwrap_or_default()
                    .to_string(),
                started_at: now_ms(),
                derived: true,
            },
        );
        self.engine.send(Command::Execute {
            query,
            tab: tab.read(cx).id,
            conn,
            sql: sql.to_string(),
            caller: Caller::User,
            approval: Approval::None,
        });
        tab.update(cx, |t, cx| t.started_derived(query, append, cx));
    }

    /// Opens `sql` in a new tab without running it.
    pub(super) fn open_sql(
        &mut self,
        title: &str,
        sql: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.add_tab(sql, window, cx);
        tab.update(cx, |t, _| t.title = title.to_string().into());
    }

    pub(super) fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
}

/// Right-click menu of a table or view in the sidebar.
pub(super) fn tab_menu(
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
