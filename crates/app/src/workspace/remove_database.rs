//! Removing a database: asked first, then everything tangul keeps for it goes (its
//! users and their secrets, boards, bookmarks and schedules, history, chats, pinned
//! tabs, its settings file, what is known about it). The database itself and the
//! audit log are left alone.

use super::*;

impl Workspace {
    /// Asks before [`Self::remove_database`], naming what goes with it.
    pub(super) fn ask_to_remove_database(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(db) = self.data.profiles.database(id) else {
            return;
        };
        let name = db.display_name();
        let count = |n: usize, one: &str, many: &str| match n {
            1 => format!("1 {one}"),
            n => format!("{n} {many}"),
        };
        let users = count(db.users.len(), "user", "users");
        let boards = count(self.data.boards.for_database(id).len(), "board", "boards");
        let bookmarks = self
            .data
            .local
            .bookmarks_for(Some(id))
            .iter()
            .filter(|b| b.database_id.as_deref() == Some(id))
            .count();
        let bookmarks = count(bookmarks, "bookmark", "bookmarks");
        let lines = [
            format!("Its connection settings, {users} and their saved passwords"),
            format!("{boards} and {bookmarks} (with their schedules and runs)"),
            "Query history, the assistant's chats about it, pinned tabs".to_string(),
            "Its own settings file and the cached schema".to_string(),
        ];
        let (workspace, id) = (cx.entity().downgrade(), id.to_string());
        window.open_dialog(cx, move |dialog, _, cx| {
            let muted = cx.theme().muted_foreground;
            let (workspace, id) = (workspace.clone(), id.clone());
            dialog
                .title(format!("Remove {name}?"))
                .w(px(460.))
                .child(
                    v_flex()
                        .gap_2()
                        .text_sm()
                        .child("This deletes everything tangul keeps for it:")
                        .children(lines.iter().map(|l| div().pl_2().child(format!("• {l}"))))
                        .child(
                            div()
                                .text_color(muted)
                                .child("The database itself isn't touched. This can't be undone."),
                        ),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("cancel-remove-database")
                                .label("Cancel")
                                .ghost()
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("remove-database")
                                .label("Remove")
                                .danger()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let _ = workspace
                                        .update(cx, |ws, cx| ws.remove_database(&id, window, cx));
                                }),
                        ),
                )
        });
    }

    /// Removes database `id` and everything kept for it; another database (or
    /// none) is shown instead.
    pub(super) fn remove_database(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(db) = self.data.profiles.database(id).cloned() else {
            return;
        };
        // Its connections, while the profiles still know whose they are.
        for user in &db.users {
            self.close_ai_connection(&user.id);
            for purpose in [Purpose::Tab, Purpose::Background] {
                if let Some(conn) = self.connections.get(purpose, &user.id) {
                    self.engine.send(Command::Disconnect { conn });
                    self.connections.forget(conn);
                }
            }
            self.conns.remove(&user.id);
        }
        if self
            .monitor
            .pending
            .as_ref()
            .is_some_and(|p| p.bookmark.database_id.as_deref() == Some(id))
        {
            self.monitor.pending = None;
        }
        // Show another database first: switching away keeps the tabs of the one
        // shown before (and saves its pinned ones).
        if self.active_db.as_deref() == Some(id) {
            let next = self
                .data
                .profiles
                .databases
                .iter()
                .find(|d| d.id != id)
                .map(|d| d.id.clone());
            match next {
                Some(next) => self.select_database(next, window, cx),
                None => {
                    let old = self.active_db.take();
                    self.active_user = None;
                    self.database_shown(old, window, cx);
                }
            }
        }
        if let Some(tabs) = self.forget_database_tabs(id) {
            for tab in tabs.items() {
                if let Some(board) = tab.board() {
                    board.update(cx, |b, _| b.cancel());
                }
            }
        }
        let settings_tab = self.tabs.items().position(|t| {
            t.file()
                .is_some_and(|f| f.read(cx).database_id() == Some(id))
        });
        if let Some(ix) = settings_tab {
            self.remove_tab(ix, window, cx);
        }
        if let Err(e) = self.data.remove_database(id) {
            window.push_notification(
                Notification::error(format!("Could not remove {}: {e:#}", db.display_name())),
                cx,
            );
        }
        self.knowledge.forget_database(id);
        self.ai_problems.remove(id);
        if self
            .board_asked
            .as_ref()
            .is_some_and(|(board, _)| self.data.boards.get(board).is_none())
        {
            self.board_asked = None;
        }
        self.expose_to_ai();
        self.sync_mcp(cx);
        self.check_ai_user(cx);
        self.sync_assistant(cx);
        self.check_board_links(cx);
        self.refresh_ai_bookmarks();
        self.refresh_history(cx);
        cx.notify();
    }
}
