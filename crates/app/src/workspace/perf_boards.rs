//! "Performance boards" in the Boards panel's + menu ([`tangul_assist::perf`]): what
//! the active database's server has, checked on its board connection once the
//! database is connected (the panel doesn't connect by itself; again after a minute,
//! so an extension installed meanwhile shows), and creating one as an ordinary board.

use std::time::Instant;

use tangul_assist::dialect::dialect_for;
use tangul_assist::perf::{Facts, Kind, Offer};
use tangul_service::boards;

use super::*;

/// How long a check holds before it is made again.
const RECHECK: Duration = Duration::from_secs(60);

/// The checks by database id.
#[derive(Default)]
pub(super) struct PerfChecks(HashMap<String, Check>);

struct Check {
    at: Instant,
    /// `None` while it runs.
    facts: Option<Result<Facts, String>>,
}

/// What the + menu's "Performance boards" shows.
#[derive(Clone)]
pub(super) enum PerfMenu {
    /// Not known yet: checking, or not connected.
    Checking,
    Failed(String),
    Offers(Vec<Offer>),
}

impl Workspace {
    /// Checks what the active database's server has, unless that is known (or
    /// being checked) and recent, or the database isn't connected.
    pub(super) fn check_perf(&mut self, cx: &mut Context<Self>) {
        let Some(db_id) = self.active_db.clone() else {
            return;
        };
        let connected = self
            .active_conn(cx)
            .is_some_and(|c| matches!(c.status, ConnStatus::Connected));
        let recent = self
            .perf_checks
            .0
            .get(&db_id)
            .is_some_and(|c| c.facts.is_none() || c.at.elapsed() < RECHECK);
        if !connected || recent {
            return;
        }
        let Some(driver) = self.data.profiles.database(&db_id).map(|d| d.driver) else {
            return;
        };
        let Some(sql) = dialect_for(driver).perf_facts_sql() else {
            return;
        };
        let checked = |facts| Check {
            at: Instant::now(),
            facts,
        };
        let conn = match self.board_conn(&db_id) {
            Ok(conn) => conn,
            Err(e) => {
                let failed = checked(Some(Err(e.to_string())));
                self.perf_checks.0.insert(db_id, failed);
                return;
            }
        };
        self.perf_checks.0.insert(db_id.clone(), checked(None));
        let (engine, query) = (self.engine.clone(), QueryId(self.next_id()));
        cx.spawn(async move |this, cx| {
            let facts = tangul_service::perf::facts(&engine, query, conn, sql).await;
            let _ = this.update(cx, |ws, cx| {
                ws.perf_checks.0.insert(db_id, checked(Some(facts)));
                cx.notify();
            });
        })
        .detach();
    }

    /// What a check of `db_id` would find.
    #[cfg(test)]
    pub(super) fn set_perf_facts(&mut self, db_id: &str, facts: Facts) {
        let check = Check {
            at: Instant::now(),
            facts: Some(Ok(facts)),
        };
        self.perf_checks.0.insert(db_id.into(), check);
    }

    /// The boards the active database's server offers; `None` for a database
    /// without performance boards.
    pub(super) fn perf_menu(&self) -> Option<PerfMenu> {
        let db_id = self.active_db.as_deref()?;
        let driver = self.data.profiles.database(db_id)?.driver;
        let d = dialect_for(driver);
        d.perf_facts_sql()?;
        Some(
            match self.perf_checks.0.get(db_id).and_then(|c| c.facts.as_ref()) {
                None => PerfMenu::Checking,
                Some(Err(e)) => PerfMenu::Failed(e.clone()),
                Some(Ok(facts)) => PerfMenu::Offers(d.perf_boards(facts)),
            },
        )
    }

    /// Creates the performance board `kind` for the active database and opens it.
    pub(super) fn create_perf_board(
        &mut self,
        kind: Kind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(PerfMenu::Offers(offers)) = self.perf_menu() else {
            return;
        };
        let (Some(db_id), Some(offer)) = (
            self.active_db.clone(),
            offers.into_iter().find(|o| o.board.kind == kind),
        ) else {
            return;
        };
        let Some(driver) = self.data.profiles.database(&db_id).map(|d| d.driver) else {
            return;
        };
        let name = format!("Performance: {}", kind.name());
        let lexing = dialect_for(driver).lexing();
        match boards::create_from(&mut self.data.boards, &db_id, &name, &offer.board, lexing) {
            Ok(board) => self.open_board(&board.id, window, cx),
            Err(e) => window.push_notification(
                Notification::error(format!("Could not create the board: {e}")),
                cx,
            ),
        }
    }
}

/// The + menu of the Boards panel: a new board, and the performance boards (each,
/// or why it can't be made now).
pub(super) fn new_board_menu(
    menu: PopupMenu,
    perf: Option<PerfMenu>,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let blank = workspace.clone();
    let menu = menu.item(
        PopupMenuItem::new("New board").on_click(move |_, window, cx| {
            let _ = blank.update(cx, |ws, cx| ws.new_board(window, cx));
        }),
    );
    let Some(perf) = perf else {
        return menu;
    };
    menu.submenu(
        "Performance boards",
        window,
        cx,
        move |mut menu, _, _| match &perf {
            PerfMenu::Checking => menu.item(note("Checking the server…", None)),
            PerfMenu::Failed(e) => menu.item(note("Couldn't check the server", Some(e.clone()))),
            PerfMenu::Offers(offers) => {
                for offer in offers {
                    let kind = offer.board.kind;
                    menu = match &offer.unavailable {
                        Some(why) => menu.item(note(kind.name(), Some(why.clone()))),
                        None => {
                            let workspace = workspace.clone();
                            menu.item(PopupMenuItem::new(kind.name()).on_click(
                                move |_, window, cx| {
                                    let _ = workspace.update(cx, |ws, cx| {
                                        ws.create_perf_board(kind, window, cx)
                                    });
                                },
                            ))
                        }
                    };
                }
                menu
            }
        },
    )
}

/// An entry that can't be chosen: its title, and why in the muted color.
fn note(title: &str, why: Option<String>) -> PopupMenuItem {
    let title = SharedString::from(title.to_string());
    PopupMenuItem::element(move |_, cx| {
        v_flex()
            .max_w(px(320.))
            .whitespace_normal()
            .child(title.clone())
            .when_some(why.clone(), |this, why| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(why),
                )
            })
    })
    .disabled(true)
}
