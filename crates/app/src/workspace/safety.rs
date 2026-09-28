//! Read-only mode, auto relock and the checks before a statement runs.

use super::*;

impl Workspace {
    /// The database user name of a connection.
    pub(super) fn conn_user_name(&self, conn: ConnId) -> Option<String> {
        let user_id = self.connections.tab_user(conn)?;
        Some(self.find_user(user_id)?.1.user.clone())
    }

    /// Whether the active user is in read-only mode.
    pub(super) fn active_user_read_only(&self, cx: &App) -> Option<bool> {
        let user = &self.find_user(self.active_user.as_deref()?)?.1.user;
        Some(settings::current(cx).is_read_only(user))
    }

    /// Flips read-only mode for the active user (kept in `settings.json`).
    pub(super) fn toggle_read_only(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(user) = self
            .active_user
            .as_deref()
            .and_then(|id| self.find_user(id))
            .map(|(_, u)| u.user.clone())
        else {
            return;
        };
        let on = !settings::current(cx).is_read_only(&user);
        match self.set_read_only(&user, on, cx) {
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
    pub(super) fn schedule_relock(&mut self, user: &str, cx: &mut Context<Self>) {
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

    pub(super) fn relock(&mut self, user: &str, cx: &mut Context<Self>) {
        self.relock.remove(user);
        if settings::current(cx).is_read_only(user) {
            return;
        }
        if let Err(e) = self.set_read_only(user, true, cx) {
            eprintln!("could not turn read-only mode back on for {user}: {e}");
        }
        cx.notify();
    }

    /// Turns read-only mode on or off for `user` in `settings.json` and has the
    /// engine read it again right away (it decides from its own copy).
    pub(super) fn set_read_only(
        &mut self,
        user: &str,
        on: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let result = settings::set_read_only(user, on, cx);
        self.engine.send(Command::ReloadSettings);
        result
    }

    /// Whether and how the user's `sql` runs on a tab's connection: the same decision
    /// the engine makes, asked first to show a dialog.
    pub(super) fn decide_for_tab(&self, conn: ConnId, sql: &str, cx: &App) -> Decision {
        let user = self.conn_user_name(conn);
        tangul_engine::decide(settings::current(cx), user.as_deref(), sql, Caller::User)
    }

    /// Explains why `sql` did not run. The red button lifts the guard: "Allow writes"
    /// turns read-only mode off (the statement is not run; run it again), "Allow …"
    /// for an unfiltered statement runs it once.
    pub(super) fn hold_back(
        &mut self,
        tab: Entity<QueryTab>,
        sql: String,
        guard: Guard,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = cx.entity().downgrade();
        let (title, intro, statements, allow) = match &guard {
            Guard::ReadOnly { user, statements } => (
                "Read-only mode",
                format!(
                    "Read-only mode is on for {user}. Nothing was run; these statements change data:"
                ),
                statements.clone(),
                "Allow writes".to_string(),
            ),
            Guard::Unfiltered { statements } => {
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
            Guard::ReadOnly { user, .. } => Some(user.clone()),
            Guard::Unfiltered { .. } => None,
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
                        Some(user) => match workspace
                            .update(cx, |ws, cx| ws.set_read_only(user, false, cx))
                            .unwrap_or_else(|_| Err("the window is gone".into()))
                        {
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
                            let _ = workspace.update(cx, |ws, cx| {
                                ws.execute(&tab, &sql, Approval::Confirmed, cx)
                            });
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
}
