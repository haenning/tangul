//! Connections: connecting, switching database and user, the database forms and engine events.

use super::*;

impl Workspace {
    /// Changes what the database lets the AI do and saves the profiles.
    fn update_ai(
        &mut self,
        db_id: &str,
        change: impl FnOnce(&mut Database),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut db) = self.data.profiles.database(db_id).cloned() else {
            return;
        };
        change(&mut db);
        if let Err(e) = self.data.profiles.upsert_database(db) {
            window.push_notification(
                Notification::error(format!("Could not save connection: {e:#}")),
                cx,
            );
        }
        cx.notify();
    }

    /// The database user the AI connects as; `None`: the tab's user.
    pub(super) fn set_assistant_user(
        &mut self,
        db_id: &str,
        user: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_ai(db_id, |db| db.assistant_user = user, window, cx);
    }

    /// Whether the AI may read column values of the database.
    pub(super) fn set_ai_values(
        &mut self,
        db_id: &str,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_ai(db_id, |db| db.ai_values = on, window, cx);
    }

    pub(super) fn connect(&mut self, user_id: &str, cx: &mut Context<Self>) {
        let Some(id) = self.connections.connect_user(
            &self.engine,
            &mut self.data.profiles,
            user_id,
            Purpose::Tab,
        ) else {
            return;
        };
        let Some((name, driver)) = self.conn_name(user_id) else {
            return;
        };
        // Tabs on it see it reconnect: the entity stays, its state changes.
        if let Some(conn) = self.conns.get(user_id) {
            conn.update(cx, |c, cx| {
                c.id = id;
                c.name = name;
                c.driver = driver;
                c.status = ConnStatus::Connecting;
                cx.notify();
            });
            self.knowledge.set_conn(user_id, id);
        } else {
            // Show (and complete against) the cached schema until the server answers.
            let cached = self.data.local.cached_schema(user_id).unwrap_or_default();
            self.set_schema(user_id, id, cached);
            let conn = cx.new(|_| Connection {
                id,
                name,
                driver,
                status: ConnStatus::Connecting,
                stats: PoolStats::default(),
            });
            self.conns.insert(user_id.to_string(), conn);
        }
        cx.notify();
    }

    /// Makes a user the active connection (connecting if needed) and uses it for the
    /// current tab.
    pub(super) fn select_user(
        &mut self,
        db_id: String,
        user_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let needs_connect = !matches!(
            self.conns.get(&user_id).map(|c| &c.read(cx).status),
            Some(ConnStatus::Connected | ConnStatus::Connecting)
        );
        if needs_connect {
            self.connect(&user_id, cx);
        }
        self.active_db = Some(db_id);
        self.active_user = Some(user_id.clone());
        self.data.local.set_setting(LAST_USER_SETTING, &user_id);
        self.assign_active_tab(&user_id, window, cx);
        self.refresh_history(cx);
        cx.notify();
    }

    /// Switches to a database with a user that is already connected, else its first
    /// user. A database without users asks for one.
    pub(super) fn select_database(
        &mut self,
        db_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(db) = self.data.profiles.database(&db_id) else {
            return;
        };
        let user = db
            .users
            .iter()
            .find(|u| {
                self.conns
                    .get(&u.id)
                    .is_some_and(|c| c.read(cx).is_connected())
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

    pub(super) fn assign_active_tab(
        &mut self,
        user_id: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = self.tab_conn(user_id) else {
            return;
        };
        if let Some(tab) = self.tabs.active_query().cloned() {
            if tab.read(cx).is_running(cx) {
                return;
            }
            tab.update(cx, |t, cx| t.set_conn(conn, cx));
        }
    }

    /// Opens the database/user dialog. `db_id` is the database to edit or add a user
    /// to, `user_id` the user to edit.
    pub(super) fn open_form(
        &mut self,
        mode: FormMode,
        db_id: Option<String>,
        user_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let db = db_id
            .as_deref()
            .and_then(|id| self.data.profiles.database(id))
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

    pub(super) fn save_form(
        &mut self,
        output: FormOutput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let FormOutput {
            database,
            user,
            ssh_secret,
        } = output;
        let db_id = database.id.clone();
        let target_changed = ssh_secret.is_some()
            || self.data.profiles.database(&db_id).is_some_and(|old| {
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
        let mut result = self.data.profiles.upsert_database(database);
        if let Some(secret) = ssh_secret {
            self.data.profiles.set_ssh_secret(&db_id, secret);
        }
        let user_id = user.map(|(user, password)| {
            let id = user.id.clone();
            result = std::mem::replace(&mut result, Ok(()))
                .and_then(|_| self.data.profiles.upsert_user(&db_id, user, password));
            id
        });
        self.rename_conns(&db_id, cx);
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
                .data
                .profiles
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

    pub(super) fn on_engine_event(
        &mut self,
        event: Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(event) = self.hidden_event(event, window, cx) else {
            return;
        };
        match event {
            Event::Query { query, event } => {
                if let Some(done) = self.router.deliver(query, event, cx)
                    && !done.run.derived
                {
                    self.record_run(done.run, done.error, done.elapsed_ms, cx);
                }
            }
            Event::Connected { conn } => {
                self.update_conn(conn, cx, |c| c.status = ConnStatus::Connected);
                // Tabs without a connection pick up the newly connected one.
                if let Some(user_id) = self.connections.tab_user(conn).map(str::to_string)
                    && let Some(tc) = self.tab_conn(&user_id)
                {
                    let tabs: Vec<_> = self.tabs.queries().cloned().collect();
                    for tab in tabs {
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
                let failed = ConnStatus::Failed(error.clone().into());
                self.update_conn(conn, cx, |c| c.status = failed);
                window.push_notification(Notification::error(error), cx);
            }
            Event::Schema { conn, result } => match result {
                Ok(info) => {
                    if let Some(user_id) = self.connections.tab_user(conn).map(str::to_string) {
                        if let Err(e) = self.data.local.save_schema(&user_id, &info) {
                            eprintln!("could not cache schema: {e:#}");
                        }
                        self.set_schema(&user_id, conn, info);
                        cx.notify();
                    }
                }
                Err(e) => window.push_notification(Notification::error(e), cx),
            },
            Event::Validated { request, problems } => {
                self.router.deliver_check(request, problems, cx)
            }
            Event::PoolStats { conn, stats } => {
                // Only the title bar shows them: tabs aren't redrawn for every tick.
                if let Some(c) = self.tab_connection(conn, cx)
                    && c.read(cx).stats != stats
                {
                    self.update_conn(conn, cx, |c| c.stats = stats);
                }
            }
        }
        cx.notify();
    }

    /// Unknown SSH host key: show its fingerprint and let the user decide. Trusting
    /// adds it to `~/.ssh/known_hosts` and connects again.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn confirm_host_key(
        &mut self,
        conn: ConnId,
        host: String,
        port: u16,
        fingerprint: String,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(user_id) = self.connections.tab_user(conn).map(str::to_string) else {
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
                        if let Some(conn) = ws.conns.get(&user_id) {
                            conn.update(cx, |c, cx| {
                                c.status = ConnStatus::Failed("SSH host key not trusted".into());
                                cx.notify();
                            });
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

    /// The tab connection with engine id `conn`, if it is still its user's.
    pub(super) fn tab_connection(&self, conn: ConnId, cx: &App) -> Option<Entity<Connection>> {
        let user_id = self.connections.tab_user(conn)?;
        self.conns
            .get(user_id)
            .filter(|c| c.read(cx).id == conn)
            .cloned()
    }

    /// Changes the tab connection `conn` and redraws the tabs on it.
    pub(super) fn update_conn(
        &self,
        conn: ConnId,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut Connection),
    ) {
        if let Some(entity) = self.tab_connection(conn, cx) {
            entity.update(cx, |c, cx| {
                change(c);
                cx.notify();
            });
        }
    }

    /// A user's connection name (`user@database`) and driver.
    fn conn_name(&self, user_id: &str) -> Option<(SharedString, DriverKind)> {
        let (db, user) = self.find_user(user_id)?;
        Some((
            format!("{}@{}", user.user, db.display_name()).into(),
            db.driver,
        ))
    }

    /// Renamed database or users: the open connections show the new names.
    fn rename_conns(&self, db_id: &str, cx: &mut Context<Self>) {
        let Some(db) = self.data.profiles.database(db_id) else {
            return;
        };
        for user in &db.users {
            if let Some(conn) = self.conns.get(&user.id)
                && let Some((name, _)) = self.conn_name(&user.id)
                && conn.read(cx).name != name
            {
                conn.update(cx, |c, cx| {
                    c.name = name;
                    cx.notify();
                });
            }
        }
    }
}
