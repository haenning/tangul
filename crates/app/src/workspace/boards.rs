//! Boards: opening them in tabs, applying their changes (through
//! `tangul_service::boards`), "Add to board" and "Update tile" from a query tab or
//! a bookmark, and the sidebar panel.

use tangul_core::board::Board;
use tangul_service::boards::{self, Edit};

use super::*;
use crate::board::{AddForm, BoardEvent, BoardTab, Target};

impl Workspace {
    /// Shows board `id` in its tab (opened if it isn't).
    pub(super) fn open_board(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let open = self
            .tabs
            .items()
            .position(|t| t.board().is_some_and(|b| b.read(cx).board().id == id));
        if let Some(ix) = open {
            self.tabs.set_active(ix);
        } else {
            let Some(board) = self.data.boards.get(id).cloned() else {
                return;
            };
            let conn = self.board_conn(&board.database_id);
            let dialect = self
                .data
                .profiles
                .database(&board.database_id)
                .map(|db| dialect_for(db.driver));
            let (tab_id, engine) = (TabId(self.next_id()), self.engine.clone());
            let tab = cx.new(|cx| BoardTab::new(tab_id, board, engine, conn, dialect, window, cx));
            let events = cx.subscribe_in(&tab, window, Self::on_board_event);
            self.tabs.push(TabItem::Board(tab), Some(events));
        }
        if let Some(tab) = self.tabs.active().cloned() {
            tab.focus(window, cx);
        }
        cx.notify();
    }

    /// The connection a board of `db_id` runs on: the hidden one of the user the
    /// database's tabs use, so a board never takes a tab's session.
    fn board_conn(&mut self, db_id: &str) -> Result<ConnId, SharedString> {
        let user = self
            .board_user(db_id)
            .ok_or("No database user to run the board with")?;
        if let Some(conn) = self.connections.get(Purpose::Background, &user) {
            return Ok(conn);
        }
        self.connections
            .connect_user(
                &self.engine,
                &mut self.data.profiles,
                &user,
                Purpose::Background,
            )
            .ok_or_else(|| "Could not connect".into())
    }

    /// The selected user if the database is the selected one, else the one used last
    /// on it, else its first.
    fn board_user(&self, db_id: &str) -> Option<String> {
        if self.active_db.as_deref() == Some(db_id)
            && let Some(user) = &self.active_user
        {
            return Some(user.clone());
        }
        let db = self.data.profiles.database(db_id)?;
        let last = self.data.local.last_user(db_id);
        db.users
            .iter()
            .find(|u| Some(&u.id) == last.as_ref())
            .or(db.users.first())
            .map(|u| u.id.clone())
    }

    fn on_board_event(
        &mut self,
        tab: &Entity<BoardTab>,
        event: &BoardEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (board_id, db_id) = {
            let board = tab.read(cx).board();
            (board.id.clone(), board.database_id.clone())
        };
        match event.clone() {
            BoardEvent::Edit(edit) => self.edit_board(&board_id, edit, window, cx),
            BoardEvent::OpenTile { title, sql, view } => {
                let tile = tab
                    .read(cx)
                    .board()
                    .tiles
                    .iter()
                    .find(|t| t.sql == sql)
                    .map(|t| t.id.clone());
                let query = self.open_query_on(&db_id, &title, &sql, true, window, cx);
                query.update(cx, |t, cx| {
                    t.set_chart_view(view, cx);
                    t.board_tile = tile.map(|tile| (board_id, tile));
                });
            }
            BoardEvent::OpenSql { title, sql, run } => {
                self.open_query_on(&db_id, &title, &sql, run, window, cx);
            }
            BoardEvent::ConnectionFailed(conn) => {
                if self.connections.purpose(conn) == Some(Purpose::Background) {
                    self.engine.send(Command::Disconnect { conn });
                    self.connections.forget(conn);
                }
                let new = self.board_conn(&db_id);
                tab.update(cx, |t, cx| t.set_conn(new, cx));
            }
        }
    }

    /// Opens `sql` in a new tab on database `db_id` (selected first, so it never runs
    /// on another), run there by the user if `run`.
    fn open_query_on(
        &mut self,
        db_id: &str,
        title: &str,
        sql: &str,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<QueryTab> {
        if self.active_db.as_deref() != Some(db_id) {
            self.select_database(db_id.to_string(), window, cx);
        }
        let same_db = self.active_db.as_deref() == Some(db_id);
        let tab = self.add_tab(sql, window, cx);
        tab.update(cx, |t, cx| {
            t.title = title.to_string().into();
            if run && same_db && t.conn.is_some() {
                cx.emit(TabEvent::Run(sql.to_string()));
            }
        });
        tab
    }

    /// Applies `edit` to board `id`, saves it and shows it in its tab.
    pub(super) fn edit_board(
        &mut self,
        id: &str,
        edit: Edit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match boards::apply(&mut self.data.boards, id, edit) {
            Ok(board) => self.show_board(board, window, cx),
            Err(e) => window.push_notification(
                Notification::error(format!("Could not change the board: {e}")),
                cx,
            ),
        }
        cx.notify();
    }

    /// Hands the open tab of `board` (if any) the board as it is now.
    fn show_board(&mut self, board: Board, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self
            .tabs
            .items()
            .find_map(|t| t.board().filter(|b| b.read(cx).board().id == board.id))
            .cloned();
        if let Some(tab) = tab {
            tab.update(cx, |t, cx| t.set_board(board, window, cx));
        }
    }

    /// Asks for a name and opens a new board of the selected database.
    pub(super) fn new_board(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(db_id) = self.active_db.clone() else {
            return;
        };
        self.open_add_dialog("New board", None, db_id, window, cx);
    }

    /// "Add to board" for the query of tab `id`.
    pub(super) fn add_tab_to_board(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.query_tab(id, cx) else {
            return;
        };
        let Some(db_id) = self.active_db.clone() else {
            return;
        };
        let (title, sql, view) = {
            let t = tab.read(cx);
            (t.title.to_string(), t.sql(cx), t.chart_shown(cx))
        };
        let tile = boards::tile(&title, &sql, view);
        self.open_add_dialog("Add to board", Some(tile), db_id, window, cx);
    }

    /// "Add to board" for a bookmark (on the selected database).
    pub(super) fn add_bookmark_to_board(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(bookmark), Some(db_id)) = (self.data.local.bookmark(id), self.active_db.clone())
        else {
            return;
        };
        let tile = boards::tile(&bookmark.name, &bookmark.sql, bookmark.view);
        self.open_add_dialog("Add to board", Some(tile), db_id, window, cx);
    }

    /// The dialog for a tile (or, without one, a new empty board): its title, and
    /// one of the database's boards or a new one. The board opens afterwards.
    fn open_add_dialog(
        &mut self,
        title: &'static str,
        tile: Option<tangul_core::board::Tile>,
        db_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let choices: Vec<(String, String)> = match &tile {
            Some(_) => self
                .data
                .boards
                .for_database(&db_id)
                .into_iter()
                .filter(|b| !b.archived)
                .map(|b| (b.id.clone(), b.name.clone()))
                .collect(),
            None => Vec::new(),
        };
        let tile_title = tile.as_ref().map_or(String::new(), |t| t.title.clone());
        let form = cx.new(|cx| AddForm::new(&tile_title, choices, tile.is_some(), window, cx));
        let workspace = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let save = {
                let (form, workspace, tile, db_id) =
                    (form.clone(), workspace.clone(), tile.clone(), db_id.clone());
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let Some((title, target)) = form.update(cx, |f, cx| f.submit(cx)) else {
                        return;
                    };
                    let tile = tile
                        .clone()
                        .map(|t| tangul_core::board::Tile { title, ..t });
                    let _ = workspace.update(cx, |ws, cx| {
                        ws.add_to_board(&db_id, target, tile, window, cx)
                    });
                    window.close_dialog(cx);
                }
            };
            dialog.title(title).w(px(420.)).child(form.clone()).footer(
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
                        Button::new("add")
                            .label(if title == "New board" {
                                "Create"
                            } else {
                                "Add"
                            })
                            .primary()
                            .on_click(save),
                    ),
            )
        });
    }

    /// Adds `tile` to the target board (made first if new) and opens it.
    pub(super) fn add_to_board(
        &mut self,
        db_id: &str,
        target: Target,
        tile: Option<tangul_core::board::Tile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = match target {
            Target::Board(id) => id,
            Target::New(name) => match boards::create(&mut self.data.boards, db_id, &name) {
                Ok(board) => board.id,
                Err(e) => {
                    window.push_notification(
                        Notification::error(format!("Could not create the board: {e}")),
                        cx,
                    );
                    return;
                }
            },
        };
        if let Some(tile) = tile {
            self.edit_board(&id, Edit::AddTile(tile), window, cx);
        }
        self.open_board(&id, window, cx);
    }

    /// "Update tile": the tab's query and chart back into the tile it came from.
    pub(super) fn update_tile_from_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.query_tab(id, cx) else {
            return;
        };
        let (link, sql, view) = {
            let t = tab.read(cx);
            (t.board_tile.clone(), t.sql(cx), t.chart_shown(cx))
        };
        let Some((board, tile)) = link else {
            return;
        };
        let edit = Edit::SetQuery { tile, sql, view };
        self.edit_board(&board, edit, window, cx);
    }

    pub(super) fn delete_board(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let open = self
            .tabs
            .items()
            .position(|t| t.board().is_some_and(|b| b.read(cx).board().id == id));
        if let Some(ix) = open {
            self.remove_tab(ix, window, cx);
        }
        if let Err(e) = boards::delete(&mut self.data.boards, id) {
            window.push_notification(
                Notification::error(format!("Could not delete the board: {e}")),
                cx,
            );
        }
        cx.notify();
    }

    pub(super) fn render_boards_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let all: Vec<Board> = match self.active_db.as_deref() {
            Some(db) => self
                .data
                .boards
                .for_database(db)
                .into_iter()
                .cloned()
                .collect(),
            None => Vec::new(),
        };
        let (archived, current): (Vec<Board>, Vec<Board>) =
            all.into_iter().partition(|b| b.archived);
        let empty = match (&self.active_db, current.is_empty()) {
            (None, _) => Some("Select a database to see its boards."),
            (Some(_), true) => Some(
                "No boards yet. Create one with +, or add a query with “Add to board” in a \
                 tab's menu or a bookmark's.",
            ),
            _ => None,
        };
        let show_archived = self.panels.boards_archived;
        let rows: Vec<AnyElement> = current
            .iter()
            .enumerate()
            .map(|(i, b)| self.render_board_row(i, b, cx))
            .collect();
        let archived_rows: Vec<AnyElement> = if show_archived {
            archived
                .iter()
                .enumerate()
                .map(|(i, b)| self.render_board_row(current.len() + i, b, cx))
                .collect()
        } else {
            Vec::new()
        };
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
                    .text_color(muted)
                    .child("BOARDS")
                    .when(self.active_db.is_some(), |this| {
                        this.child(
                            Button::new("new-board")
                                .icon(IconName::Plus)
                                .ghost()
                                .xsmall()
                                .tooltip("New board")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.new_board(window, cx)),
                                ),
                        )
                    }),
            )
            .children(empty.map(|text| div().px_3().py_2().text_sm().text_color(muted).child(text)))
            .child(
                v_flex()
                    .id("boards")
                    .flex_1()
                    .min_h_0()
                    .px_1()
                    .overflow_y_scroll()
                    .children(rows)
                    .when(!archived.is_empty(), |this| {
                        this.child(
                            h_flex()
                                .id("boards-archived")
                                .mt_2()
                                .px_2()
                                .h(px(ROW_HEIGHT))
                                .gap_1()
                                .items_center()
                                .text_xs()
                                .text_color(muted)
                                .cursor_pointer()
                                .child(Icon::new(if show_archived {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                }))
                                .child(format!("Archived ({})", archived.len()))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.panels.boards_archived = !this.panels.boards_archived;
                                    cx.notify();
                                })),
                        )
                        .when(show_archived, |this| this.children(archived_rows))
                    }),
            )
    }

    fn render_board_row(&self, ix: usize, board: &Board, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let group: SharedString = format!("board-{ix}").into();
        let (open_id, archive_id, delete_id) =
            (board.id.clone(), board.id.clone(), board.id.clone());
        let archived = board.archived;
        let tiles = match board.tiles.len() {
            1 => "1 tile".to_string(),
            n => format!("{n} tiles"),
        };
        let actions = h_flex()
            .gap_0p5()
            .invisible()
            .group_hover(group.clone(), |s| s.visible())
            .child(
                Button::new(("board-archive", ix))
                    .icon(Icon::new(Lucide::Archive))
                    .ghost()
                    .xsmall()
                    .tooltip(if archived { "Unarchive" } else { "Archive" })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.edit_board(&archive_id, Edit::Archive(!archived), window, cx)
                    })),
            )
            .child(
                Button::new(("board-delete", ix))
                    .icon(IconName::Delete)
                    .ghost()
                    .xsmall()
                    .tooltip("Delete")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.delete_board(&delete_id, window, cx)
                    })),
            );
        h_flex()
            .id(("board-row", ix))
            .test_support()
            .group(group)
            .w_full()
            .h(px(BOOKMARK_ROW_HEIGHT))
            .px_2()
            .gap_1p5()
            .items_center()
            .rounded(theme.radius)
            .hover(|s| s.bg(theme.sidebar_accent))
            .cursor_pointer()
            .child(
                Icon::new(Lucide::LayoutDashboard)
                    .xsmall()
                    .text_color(theme.muted_foreground),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().truncate().child(board.name.clone()))
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(tiles),
                    ),
            )
            .child(actions)
            .on_click(cx.listener(move |this, _, window, cx| this.open_board(&open_id, window, cx)))
            .into_any_element()
    }
}
