//! A board in a tab: its name, description and links on top, its tiles below (a
//! `ResultView` each), all refreshed in one batch. It knows nothing about the
//! workspace: changes go out as [`BoardEvent::Edit`], which the workspace applies
//! through `tangul_service::boards` and hands back with [`BoardTab::set_board`];
//! its queries run as `Caller::Board` on the connection it is given.
//!
//! When it refreshes is `tangul_service::boards::due`'s, asked while it is drawn:
//! a board that isn't shown doesn't refresh, and a timer only asks for a redraw.

mod form;
mod view;

pub use form::{AddForm, Adding, BookmarkChoice, Choices, DetailsForm, Target};

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::*;
use tangul_assist::dialect::Dialect;
use tangul_core::board::{Board, LinkTo, Refresh};
use tangul_core::chart::ChartView;
use tangul_core::{ConnId, QueryId, TabId};
use tangul_engine::{Caller, Command, EngineHandle, QueryError, QueryOptions, QueryOutput};
use tangul_service::boards::{Batch, Edit, due, next_in};

use crate::assets::Lucide;
use crate::result_view::{ResultEvent, ResultView};

/// What the board asks of the workspace.
#[derive(Clone, Debug)]
pub enum BoardEvent {
    /// Change the board (and save it).
    Edit(Edit),
    /// Open a tile's query in a tab, shown as the tile shows it.
    OpenTile {
        title: String,
        sql: String,
        view: Option<ChartView>,
    },
    /// SQL from a tile's rows (related rows, an UPDATE for them): into a tab, run
    /// there by the user if `run`.
    OpenSql {
        title: String,
        sql: String,
        run: bool,
    },
    /// Open the board's link at this index (not a web address: the board opens
    /// those itself).
    OpenLink(usize),
    /// Edit the board's name, description and links.
    EditDetails,
    /// Ask the assistant for a tile's query.
    AskAi(String),
    /// A whole refresh failed on this connection (gone, never connected): the
    /// workspace drops it and hands over a new one ([`BoardTab::set_conn`]).
    ConnectionFailed(ConnId),
}

/// A tile as shown: its result, and whether it has one yet.
struct TileView {
    result: Entity<ResultView>,
    /// The SQL its result is from (a changed tile runs again).
    sql: String,
    loaded: bool,
    /// A filter, sort or page of its result running.
    _refine: Task<()>,
    _events: Subscription,
}

pub struct BoardTab {
    pub id: TabId,
    pub title: SharedString,
    board: Board,
    engine: EngineHandle,
    /// Where the tiles run, or why they can't.
    conn: Result<ConnId, SharedString>,
    dialect: Option<&'static dyn Dialect>,
    tiles: HashMap<String, TileView>,
    /// Edit mode: details, widths, order, removing tiles.
    editing: bool,
    /// The whole description instead of its first line.
    show_description: bool,
    /// Per link: what it opens is gone (a deleted bookmark or board).
    broken: Vec<bool>,
    /// What "Add tile" offers, asked for when the menu opens.
    choices: Option<ChoicesFn>,
    /// When the last refresh started, and the one running.
    last: Option<Instant>,
    running: Option<QueryId>,
    /// When the last refresh finished, and why it failed as a whole.
    updated: Option<Instant>,
    failed: Option<SharedString>,
    _refresh: Task<()>,
    /// Asks for a redraw when the interval is up, and now and then for "updated
    /// … ago".
    _timer: Task<()>,
    _tick: Task<()>,
    focus_handle: FocusHandle,
}

impl EventEmitter<BoardEvent> for BoardTab {}

/// What "Add tile" and links can offer (the workspace knows).
pub type ChoicesFn = Rc<dyn Fn(&App) -> Choices>;

/// How often "updated … ago" is redrawn.
const TICK: Duration = Duration::from_secs(30);

impl BoardTab {
    pub fn new(
        id: TabId,
        board: Board,
        engine: EngineHandle,
        conn: Result<ConnId, SharedString>,
        dialect: Option<&'static dyn Dialect>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        let mut tab = Self {
            id,
            title: board.name.clone().into(),
            board: Board::default(),
            engine,
            conn,
            dialect,
            tiles: HashMap::new(),
            editing: false,
            show_description: false,
            broken: Vec::new(),
            choices: None,
            last: None,
            running: None,
            updated: None,
            failed: None,
            _refresh: Task::ready(()),
            _timer: Task::ready(()),
            _tick: tick,
            focus_handle: cx.focus_handle(),
        };
        tab.sync(board, window, cx);
        tab
    }

    pub fn board(&self) -> &Board {
        &self.board
    }

    /// The board as it is now (after an edit). Tiles whose query changed, and new
    /// ones, run again unless the board refreshes only by hand.
    pub fn set_board(&mut self, board: Board, window: &mut Window, cx: &mut Context<Self>) {
        self.title = board.name.clone().into();
        let stale = self.sync(board, window, cx);
        if stale && self.last.is_some() && self.board.refresh.on_open() {
            self.refresh(cx);
        }
        cx.notify();
    }

    /// Where "Add tile" takes its bookmarks, recent queries and whether the
    /// assistant is there.
    pub fn set_choices(&mut self, choices: ChoicesFn) {
        self.choices = Some(choices);
    }

    /// What "Add tile" offers now.
    #[cfg(test)]
    pub fn choices(&self, cx: &App) -> Choices {
        self.choices.as_ref().map(|f| f(cx)).unwrap_or_default()
    }

    /// Whether link `ix` opens nothing any more.
    pub fn is_broken(&self, ix: usize) -> bool {
        self.broken.get(ix).copied().unwrap_or(false)
    }

    /// Which links open nothing any more (a deleted bookmark or board).
    pub fn set_broken(&mut self, broken: Vec<bool>, cx: &mut Context<Self>) {
        self.broken = broken;
        cx.notify();
    }

    /// A connection to run on (connected again, or another user's).
    pub fn set_conn(&mut self, conn: Result<ConnId, SharedString>, cx: &mut Context<Self>) {
        self.conn = conn;
        cx.notify();
    }

    pub fn focus(this: &Entity<Self>, window: &mut Window, cx: &mut App) {
        window.focus(&this.read(cx).focus_handle.clone(), cx);
    }

    /// Stops a refresh that is running (the tab closes).
    pub fn cancel(&mut self) {
        if let Some(query) = self.running.take() {
            self.engine.send(Command::Cancel { query });
        }
    }

    /// Takes `board`, keeping the views of the tiles it still has. Whether a tile's
    /// result is out of date (new, or its query changed).
    fn sync(&mut self, board: Board, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mut stale = false;
        self.tiles
            .retain(|id, _| board.tiles.iter().any(|t| &t.id == id));
        for tile in &board.tiles {
            match self.tiles.get_mut(&tile.id) {
                Some(view) => {
                    if view.sql != tile.sql {
                        view.sql = tile.sql.clone();
                        stale = true;
                    }
                    view.result
                        .update(cx, |r, cx| r.set_chart_view(tile.view, cx));
                }
                None => {
                    let view = self.tile_view(&tile.id, &tile.sql, tile.view, window, cx);
                    self.tiles.insert(tile.id.clone(), view);
                    stale = true;
                }
            }
        }
        self.board = board;
        stale
    }

    fn tile_view(
        &self,
        id: &str,
        sql: &str,
        view: Option<ChartView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TileView {
        let result = cx.new(|cx| ResultView::new(window, cx));
        result.update(cx, |r, cx| {
            if let Some(dialect) = self.dialect {
                r.set_dialect(dialect);
            }
            r.set_chart_view(view, cx);
        });
        let tile = id.to_string();
        let events = cx.subscribe(&result, move |this, _, event: &ResultEvent, cx| {
            this.on_result_event(&tile, event.clone(), cx)
        });
        TileView {
            result,
            sql: sql.to_string(),
            loaded: false,
            _refine: Task::ready(()),
            _events: events,
        }
    }

    fn on_result_event(&mut self, tile: &str, event: ResultEvent, cx: &mut Context<Self>) {
        match event {
            ResultEvent::Refine { sql, append } => self.refine(tile, sql, append, cx),
            ResultEvent::OpenRelated { title, sql } => {
                cx.emit(BoardEvent::OpenSql {
                    title,
                    sql,
                    run: true,
                });
            }
            ResultEvent::OpenSql { title, sql } => {
                cx.emit(BoardEvent::OpenSql {
                    title,
                    sql,
                    run: false,
                });
            }
            // The value viewer and the schema belong to tabs.
            ResultEvent::OpenTable { .. }
            | ResultEvent::Failed { .. }
            | ResultEvent::Inspect { .. }
            | ResultEvent::ToggleViewer
            | ResultEvent::Compare(_) => {}
        }
    }

    /// A tile's filter, sort or next page: a read like its query, on its own.
    fn refine(&mut self, tile: &str, sql: String, append: bool, cx: &mut Context<Self>) {
        let (Ok(conn), Some(view)) = (self.conn.clone(), self.tiles.get_mut(tile)) else {
            return;
        };
        let query = QueryId(crate::next_id());
        let result = view.result.clone();
        result.update(cx, |r, cx| r.started_derived(query, append, cx));
        let engine = self.engine.clone();
        view._refine = cx.spawn(async move |_, cx| {
            let output = engine
                .query(query, conn, sql, QueryOptions::new(Caller::Board))
                .await
                .map_err(|e| e.message);
            result.update(cx, |r, cx| r.replay(output, cx));
        });
    }

    /// Runs every tile's query now, in one batch (each statement once).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Ok(conn) = self.conn.clone() else {
            return;
        };
        if self.running.is_some() {
            return;
        }
        let batch = Batch::of(&self.board);
        let tiles: Vec<(String, usize)> = self
            .board
            .tiles
            .iter()
            .map(|t| t.id.clone())
            .zip(batch.tiles.iter().copied())
            .collect();
        self.last = Some(Instant::now());
        self.failed = None;
        self.arm_timer(cx);
        if batch.statements.is_empty() {
            self.updated = self.last;
            cx.notify();
            return;
        }
        let query = QueryId(crate::next_id());
        self.running = Some(query);
        let (engine, statements) = (self.engine.clone(), batch.statements);
        self._refresh = cx.spawn(async move |this, cx| {
            let options = QueryOptions::new(Caller::Board);
            let output = engine
                .query_batch(query, conn, statements.clone(), options)
                .await;
            let _ = this.update(cx, |this, cx| {
                if output.is_err() {
                    cx.emit(BoardEvent::ConnectionFailed(conn));
                }
                this.finished(query, &statements, &tiles, output, cx)
            });
        });
        cx.notify();
    }

    /// Shows each tile's result of refresh `query`.
    fn finished(
        &mut self,
        query: QueryId,
        statements: &[String],
        tiles: &[(String, usize)],
        output: Result<Vec<Result<QueryOutput, QueryError>>, QueryError>,
        cx: &mut Context<Self>,
    ) {
        if self.running != Some(query) {
            return;
        }
        self.running = None;
        self.updated = Some(Instant::now());
        let results: Vec<Result<QueryOutput, String>> = match output {
            Ok(results) => results
                .into_iter()
                .map(|r| r.map_err(|e| e.message))
                .collect(),
            Err(e) => {
                self.failed = Some(e.message.clone().into());
                statements.iter().map(|_| Err(e.message.clone())).collect()
            }
        };
        for (tile, at) in tiles {
            let (Some(view), Some(output), Some(sql)) = (
                self.tiles.get_mut(tile),
                results.get(*at),
                statements.get(*at),
            ) else {
                continue;
            };
            // Changed while it ran: the next refresh has it.
            if view.sql != *sql {
                continue;
            }
            view.loaded = true;
            let run = QueryId(crate::next_id());
            view.result.update(cx, |r, cx| {
                r.started(run, sql, cx);
                r.replay(output.clone(), cx);
            });
        }
        cx.notify();
    }

    /// A refresh that returned `results` (one per statement of the batch), without
    /// the engine.
    #[cfg(test)]
    pub fn show_results(
        &mut self,
        results: Vec<Result<QueryOutput, QueryError>>,
        cx: &mut Context<Self>,
    ) {
        let batch = Batch::of(&self.board);
        let tiles: Vec<(String, usize)> = self
            .board
            .tiles
            .iter()
            .map(|t| t.id.clone())
            .zip(batch.tiles.iter().copied())
            .collect();
        let query = QueryId(crate::next_id());
        self.running = Some(query);
        self.finished(query, &batch.statements, &tiles, Ok(results), cx);
    }

    /// The result view of tile `id` (tests).
    #[cfg(test)]
    pub fn tile_result(&self, id: &str) -> Option<Entity<ResultView>> {
        self.tiles.get(id).map(|t| t.result.clone())
    }

    /// Redraws when the interval is up; the redraw refreshes if the board is shown.
    fn arm_timer(&mut self, cx: &mut Context<Self>) {
        let Some(wait) = next_in(self.board.refresh, self.last, Instant::now()) else {
            self._timer = Task::ready(());
            return;
        };
        self._timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        });
    }

    /// Refreshes after this frame if it is due: the board is being drawn, so shown.
    fn refresh_if_due(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        if self.conn.is_ok() && due(self.board.refresh, self.last, self.running.is_some(), now) {
            cx.defer_in(window, |this, _, cx| this.refresh(cx));
        }
    }

    fn set_refresh(&mut self, refresh: Refresh, cx: &mut Context<Self>) {
        cx.emit(BoardEvent::Edit(Edit::SetRefresh(refresh)));
    }

    fn open_tile(&self, tile: &str, cx: &mut Context<Self>) {
        if let Some(tile) = self.board.tile(tile) {
            let title = if tile.title.is_empty() {
                self.board.name.clone()
            } else {
                tile.title.clone()
            };
            cx.emit(BoardEvent::OpenTile {
                title,
                sql: tile.sql.clone(),
                view: tile.view,
            });
        }
    }
}

/// What a link shows as its kind.
pub fn link_icon(to: &LinkTo) -> Lucide {
    match to {
        LinkTo::Url(_) => Lucide::ExternalLink,
        LinkTo::Bookmark(_) => Lucide::Bookmark,
        LinkTo::Board(_) => Lucide::LayoutDashboard,
        LinkTo::Query { view: None, .. } => Lucide::Table,
        LinkTo::Query { view: Some(_), .. } => Lucide::ChartLine,
    }
}

/// What a link does, as its tooltip says.
pub fn link_action(to: &LinkTo) -> &'static str {
    match to {
        LinkTo::Url(_) => "Open in the browser",
        LinkTo::Bookmark(_) => "Go to bookmark",
        LinkTo::Board(_) => "Open board",
        LinkTo::Query { .. } => "Open in a new tab",
    }
}

impl Focusable for BoardTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
