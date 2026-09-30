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
            knowledge: self.knowledge.clone(),
            tab: tab.downgrade(),
        });
        tab.update(cx, |t, cx| t.set_completion(provider, cx));
        let check = self.question_check();
        tab.update(cx, |t, _| t.set_question_check(check));
        let events = cx.subscribe_in(&tab, window, Self::on_tab_event);
        self.tabs.push(TabItem::Query(tab.clone()), Some(events));
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
            .items()
            .position(|t| t.file().is_some_and(|f| f.read(cx).path() == path));
        match open {
            Some(ix) => self.tabs.set_active(ix),
            None => {
                let id = TabId(self.next_id());
                let tab = cx.new(|cx| FileTab::new(id, path, window, cx));
                self.tabs.push(TabItem::File(tab), None);
            }
        }
        if let Some(tab) = self.tabs.active().cloned() {
            tab.focus(window, cx);
        }
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
        self.close_tab(self.tabs.active_index(), window, cx);
    }

    pub(super) fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix).cloned() else {
            return;
        };
        if let Some(file) = tab.file().filter(|_| tab.has_unsaved_changes(cx)) {
            self.ask_to_save(file.clone(), window, cx);
            return;
        }
        self.remove_tab(ix, window, cx);
    }

    /// Closing a file tab with unsaved changes: save, discard or keep the tab.
    pub(super) fn ask_to_save(
        &mut self,
        tab: Entity<FileTab>,
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
                if save && !tab.update(cx, |t, cx| t.save(window, cx)) {
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
        let Some(tab) = self.tabs.remove(ix) else {
            return;
        };
        if let Some(tab) = tab.query().map(|t| t.read(cx))
            && let Some(conn) = tab.conn_id(cx)
        {
            // Drops a pinned session (open transaction is rolled back by the server).
            self.engine.send(Command::ReleaseTab { tab: tab.id, conn });
        }
        match self.tabs.active().cloned() {
            Some(active) => active.focus(window, cx),
            // The empty state takes over.
            None => window.focus(&self.focus_handle, cx),
        }
        cx.notify();
    }

    pub(super) fn tab_index(&self, id: TabId, cx: &App) -> Option<usize> {
        self.tabs.index_of(id, cx)
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
            self.tabs.toggle_pin(ix);
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
        let Some(tab) = self
            .tab_index(id, cx)
            .and_then(|ix| self.tabs.get(ix)?.query().cloned())
        else {
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
                self.router.checking(request, tab.downgrade(), *generation);
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
            TabEvent::Inspect { cell, open } => {
                let picked = self.picked(tab, cell, cx);
                let old = self
                    .panels
                    .value
                    .update(cx, |p, cx| p.set_a(picked, window, cx));
                self.clear_value_mark(old, false, cx);
                if *open {
                    self.show_panel(Panel::Value, cx);
                }
            }
            TabEvent::ToggleViewer => {
                if self.host(Panel::Value).shows(Panel::Value) {
                    self.close_value_viewer(&CloseValueViewer, window, cx);
                } else if !tab.update(cx, |t, cx| t.inspect_selected(cx)) {
                    self.show_panel(Panel::Value, cx);
                }
            }
            TabEvent::Ask(question) => self.ask_from_editor(question.clone(), tab, window, cx),
            TabEvent::Compare(cell) => {
                let picked = self.picked(tab, cell, cx);
                let old = self
                    .panels
                    .value
                    .update(cx, |p, cx| p.set_b(picked, window, cx));
                self.clear_value_mark(old, true, cx);
                self.show_panel(Panel::Value, cx);
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
                let Some((known, _)) = self.knowledge.for_conn(conn.read(cx).id) else {
                    return;
                };
                let Some(ix) = known.resolve(Some(schema), table) else {
                    return;
                };
                let event = TabEvent::OpenRelated {
                    title: table.clone(),
                    sql: select_rows(known.dialect(), &known.sql_name(ix), OPEN_ROWS),
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
                    // Only for the assistant; tabs run as the user.
                    Decision::Propose(_) => {}
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
        self.router.started(
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
        self.router.started(
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
        let tabs = self.tabs.items().enumerate().map(|(ix, tab)| {
            let (id, pinned, title) = (tab.id(cx), self.tabs.is_pinned(ix), tab.title(cx));
            // A file tab (settings) holds no query to bookmark.
            let query = tab.query().map(|t| t.read(cx));
            let bookmarked = query.is_some_and(|t| t.bookmark_id.is_some());
            let has_sql = query.is_some_and(|t| !t.sql(cx).trim().is_empty());
            let label: SharedString = if tab.is_dirty(cx) {
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
                    cx.listener(move |this, _, _, _| this.tabs.middle_pressed(id)),
                )
                .on_mouse_up(
                    MouseButton::Middle,
                    cx.listener(move |this, _, window, cx| {
                        if this.tabs.middle_released(id) && !pinned {
                            this.close_tab_by_id(id, window, cx);
                        }
                    }),
                )
        });
        TabBar::new("tabs")
            .children(tabs)
            .selected_index(self.tabs.active_index())
            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                this.tabs.set_active(*ix);
                if let Some(tab) = this.tabs.active().cloned() {
                    tab.focus(window, cx);
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

/// The value viewer's side: cells from the tabs, and what it asks of them.
impl Workspace {
    /// A cell of `tab` for the value viewer.
    fn picked(&self, tab: &Entity<QueryTab>, cell: &CellValue, cx: &App) -> Picked {
        let t = tab.read(cx);
        Picked {
            tab: t.id,
            source: format!("{} · {} · row {}", t.title, cell.column, cell.row).into(),
            cell: cell.clone(),
        }
    }

    fn query_tab(&self, id: TabId, cx: &App) -> Option<Entity<QueryTab>> {
        self.tabs.queries().find(|t| t.read(cx).id == id).cloned()
    }

    /// The value viewer shows A (or B, `compared`) from elsewhere now: the tab
    /// `old` drops its mark.
    fn clear_value_mark(&mut self, old: Option<TabId>, compared: bool, cx: &mut Context<Self>) {
        if let Some(tab) = old.and_then(|id| self.query_tab(id, cx)) {
            tab.update(cx, |t, cx| t.clear_value_mark(compared, cx));
        }
    }

    pub(super) fn on_value_event(
        &mut self,
        _: &Entity<ValuePanel>,
        event: &ValueEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ValueEvent::ClearCompared(id) => self.clear_value_mark(Some(*id), true, cx),
            ValueEvent::OpenRows { tab, title, sql } => match self.query_tab(*tab, cx) {
                Some(tab) => {
                    let open = TabEvent::OpenRelated {
                        title: title.clone(),
                        sql: sql.clone(),
                    };
                    self.on_tab_event(&tab, &open, window, cx);
                }
                None => window.push_notification(
                    Notification::info("The tab this value is from is closed"),
                    cx,
                ),
            },
        }
    }
}
