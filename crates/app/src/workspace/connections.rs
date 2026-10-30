//! Connections: connecting, switching database and user, the database forms and engine events.

use super::*;
use crate::ui::{dialog_form, enter_saves};

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
        self.expose_to_ai();
        self.push_ai_settings(cx);
        cx.notify();
    }

    /// Whether AI tools (the assistant, MCP clients) may use the database.
    pub(super) fn set_ai_tools(
        &mut self,
        db_id: &str,
        on: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_ai(db_id, |db| db.ai_tools = on, window, cx);
    }

    /// The database user the AI connects as; `None`: the tab's user.
    pub(super) fn set_assistant_user(
        &mut self,
        db_id: &str,
        user: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Checked again with the new user.
        self.ai_problems.remove(db_id);
        self.update_ai(db_id, |db| db.assistant_user = user, window, cx);
        self.check_ai_user(cx);
    }

    /// Asks the server how many tables the AI's database user (of the active
    /// database) may change or drop; the title bar warns when it may any. Runs as
    /// the assistant, through the gate, on the assistant's own connection.
    pub(super) fn check_ai_user(&mut self, cx: &mut Context<Self>) {
        let (Some(db_id), Some(user_id)) = (self.active_db.clone(), self.active_user.clone())
        else {
            return;
        };
        let Some(db) = self.data.profiles.database(&db_id) else {
            return;
        };
        // Schema only: no AI connection, nothing it could write.
        if !self.ai_access(db).is_data() {
            self.knowledge.set_ai_writable(&db_id, 0);
            self.ai_problems.remove(&db_id);
            return;
        }
        let Some(sql) = dialect_for(db.driver).writable_tables_sql() else {
            return;
        };
        let conn = match self.connections.connect_assistant(
            &self.engine,
            &mut self.data.profiles,
            &user_id,
        ) {
            Ok(conn) => conn,
            Err(problem) => {
                self.ai_problems.insert(db_id, problem);
                cx.notify();
                return;
            }
        };
        let (engine, query) = (self.engine.clone(), QueryId(self.next_id()));
        let options = QueryOptions {
            row_limit: 1,
            ..QueryOptions::new(Caller::Assistant)
        };
        self.ai_check = cx.spawn(async move |this, cx| {
            let result = engine.query(query, conn, sql, options).await;
            let _ = this.update(cx, |ws, cx| match result {
                Ok(out) => {
                    let tables = out
                        .results
                        .last()
                        .and_then(|r| r.cell(0, 0))
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    ws.set_ai_writable(&db_id, tables, cx);
                }
                Err(e) => eprintln!("could not check the AI user's rights: {}", e.message),
            });
        });
    }

    /// The database the AI's connection `conn` belongs to.
    fn ai_database(&self, conn: ConnId) -> Option<(String, String)> {
        let user_id = self.connections.key(conn)?;
        let (db, user) = self.find_user(user_id)?;
        Some((db.id.clone(), user.user.clone()))
    }

    /// The AI's connection `conn` could not connect: shown in the title bar (and
    /// once as a notification), never answered by connecting as someone else. It
    /// tries again, as the same user, the next time the databases are offered.
    pub(super) fn ai_connect_failed(
        &mut self,
        conn: ConnId,
        error: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let found = self.ai_database(conn);
        self.connections.forget(conn);
        let Some((db_id, user)) = found else {
            return;
        };
        let problem = format!("The AI could not connect as {user}: {error}");
        if self.ai_problems.get(&db_id) != Some(&problem) {
            window.push_notification(Notification::error(problem.clone()), cx);
            self.ai_problems.insert(db_id, problem);
            self.ai_warning_pulse += 1;
        }
        cx.notify();
    }

    /// The AI's connection `conn` is up: nothing to report any more.
    pub(super) fn ai_connected(&mut self, conn: ConnId, cx: &mut Context<Self>) {
        if let Some((db_id, _)) = self.ai_database(conn)
            && self.ai_problems.remove(&db_id).is_some()
        {
            cx.notify();
        }
    }

    /// The AI's user of `db_id` may change or drop `tables` tables.
    pub(super) fn set_ai_writable(&mut self, db_id: &str, tables: u64, cx: &mut Context<Self>) {
        self.knowledge.set_ai_writable(db_id, tables);
        if tables > 0 {
            self.ai_warning_pulse += 1;
        }
        cx.notify();
    }

    /// What the AI may read from the database's rows.
    pub(super) fn set_ai_values(
        &mut self,
        db_id: &str,
        access: ValueAccess,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_ai(db_id, |db| db.ai_values = access, window, cx);
    }

    /// What AI tools may see of the database: its data or only its schema.
    pub(super) fn set_ai_access(
        &mut self,
        db_id: &str,
        access: AiAccess,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_ai(db_id, |db| db.ai_access = access, window, cx);
        self.check_ai_user(cx);
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
        let old = self.active_db.replace(db_id);
        let same_db = old == self.active_db;
        self.active_user = Some(user_id.clone());
        self.data
            .local
            .set_setting(tangul_data::local::LAST_USER, &user_id);
        if same_db {
            // Another user of the same database: the current tab runs as them.
            self.assign_active_tab(&user_id, window, cx);
        } else {
            self.database_shown(old, window, cx);
        }
        self.refresh_history(cx);
        self.check_ai_user(cx);
        self.sync_assistant(cx);
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
                let old = self.active_db.replace(db_id.clone());
                self.active_user = None;
                self.database_shown(old, window, cx);
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
                .on_ok(enter_saves(save.clone()))
                .title(title.clone())
                .w(px(460.))
                .overlay_closable(false)
                .keyboard(false)
                .close_button(false)
                .child(dialog_form(form.clone()))
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

    /// Files dropped on the window: the first one opens as a database, added unless
    /// it is already.
    pub(super) fn open_dropped(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = paths.paths().first() else {
            return;
        };
        let path = path.to_string_lossy().to_string();
        let saved = self
            .data
            .profiles
            .databases
            .iter()
            .find(|d| d.driver.is_file() && d.database == path)
            .map(|d| d.id.clone());
        match saved {
            Some(db_id) => self.select_database(db_id, window, cx),
            None => self.save_form(file_database(&path), window, cx),
        }
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
            ai_account,
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
        self.expose_to_ai();
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
            self.select_user(db_id.clone(), user_id, window, cx);
        }
        if let Some((user, password)) = ai_account {
            self.open_ai_account_sql(&db_id, user, password, window, cx);
        }
        self.check_ai_user(cx);
        cx.notify();
    }

    /// Saves the new read-only AI user's password and opens the statements that
    /// create it, unrun, on one of your users of the database: an admin runs them.
    fn open_ai_account_sql(
        &mut self,
        db_id: &str,
        user: DbUser,
        password: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = user.user.clone();
        let ai_id = user.id.clone();
        if let Err(e) = self
            .data
            .profiles
            .upsert_user(db_id, user, Some(password.clone()))
        {
            window.push_notification(
                Notification::error(format!("Could not save the AI user: {e:#}")),
                cx,
            );
            return;
        }
        let Some(db) = self.data.profiles.database(db_id).cloned() else {
            return;
        };
        // Yours: the active one if it is of this database, else the first other one.
        let admin = self
            .active_user
            .clone()
            .filter(|id| self.active_db.as_deref() == Some(db_id) && *id != ai_id)
            .or_else(|| {
                db.users
                    .iter()
                    .find(|u| u.id != ai_id)
                    .map(|u| u.id.clone())
            });
        let dialect = dialect_for(db.driver);
        // Every schema that has tables, as far as they are known.
        let mut schemas: Vec<String> = admin
            .as_deref()
            .and_then(|id| self.knowledge.schema(id))
            .map(|s| s.tables().iter().map(|t| t.schema.clone()).collect())
            .unwrap_or_default();
        schemas.sort();
        schemas.dedup();
        if schemas.is_empty() {
            schemas.extend(dialect.default_schema(&db.database));
        }
        let Some(statements) =
            dialect.readonly_account_sql(&name, &password, &db.database, &schemas)
        else {
            return;
        };
        if let Some(admin) = admin
            && self.active_user.as_deref() != Some(admin.as_str())
        {
            self.connect(&admin, cx);
            self.select_user(db_id.to_string(), admin, window, cx);
        }
        let sql = format!(
            "-- Creates {name}, the AI's user, that may only read. Run it as a user that \
             may create users and grant rights.\n\
             -- The password is in the Keychain; history keeps it hidden.\n{};\n",
            statements.join(";\n")
        );
        self.open_sql("Read-only AI user", &sql, window, cx);
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
                if let Some(done) = self.router.deliver(query, event, cx) {
                    if done.error.is_none() {
                        self.connection_works(&done.run.user_id, cx);
                    }
                    if !done.run.derived {
                        self.record_run(done.run, done.error, done.elapsed_ms, cx);
                    }
                }
            }
            Event::Connected { conn } => {
                self.update_conn(conn, cx, |c| c.status = ConnStatus::Connected);
                // Tabs without a connection pick up the newly connected one.
                if let Some(user_id) = self.connections.tab_user(conn).map(str::to_string)
                    && let Some(tc) = self.tab_conn(&user_id)
                {
                    for tab in self.all_query_tabs() {
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
            Event::Schema {
                conn,
                result,
                complete,
            } => match result {
                Ok(mut info) => {
                    if let Some(user_id) = self.connections.tab_user(conn).map(str::to_string) {
                        if complete {
                            if let Err(e) = self.data.local.save_schema(&user_id, &info) {
                                eprintln!("could not cache schema: {e:#}");
                            }
                        } else if let Some(known) = self.knowledge.schema(&user_id) {
                            // Tables first; until the rest arrives, keep what is known
                            // of it (from the cache or the last load).
                            let known = known.info();
                            info.indexes = known.indexes.clone();
                            info.checks = known.checks.clone();
                            info.enums = known.enums.clone();
                            info.views = known.views.clone();
                            info.routines = known.routines.clone();
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
    /// A query on `user_id`'s connection succeeded: it works again after a failure
    /// (the pool reconnects on its own once the network is back).
    fn connection_works(&self, user_id: &str, cx: &mut Context<Self>) {
        if let Some(conn) = self.tab_conn(user_id)
            && matches!(conn.read(cx).status, ConnStatus::Failed(_))
        {
            conn.update(cx, |c, cx| {
                c.status = ConnStatus::Connected;
                cx.notify();
            });
            cx.notify();
        }
    }

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
        // A file's one user has no name.
        let name = match user.user.as_str() {
            "" => db.display_name(),
            user => format!("{user}@{}", db.display_name()),
        };
        Some((name.into(), db.driver))
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
