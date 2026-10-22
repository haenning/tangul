//! What the in-app assistant and MCP clients share: the databases offered to AI
//! tools (each on its AI connection), and what their tool calls hand back to the
//! window: statements they ran go to the database's history, proposals and shown
//! results open in tabs on the user's own connection. Every call is audited.

use std::collections::HashSet;
use std::sync::atomic::AtomicU64;
use std::sync::{Mutex, PoisonError, RwLock};

use tangul_ai::{
    AuditSink, BoardCall, BoardSink, Ctx, Exposed, ProposalSink, SharedBookmark, ShowSink,
    ShownResult, SqlProposal, ToolCall,
};
use tangul_ai::{Registry, ToolOutput};
use tangul_assist::ai_view;
use tangul_core::board::AiCaller;
use tangul_core::{ColumnInfo, ResultSet, RowBatch};
use tangul_data::database_settings;
use tangul_data::local::{AuditEntry, Source};
use tangul_service::connections::assistant_user;

use super::*;

/// What a tool call hands to the window (from the MCP server's thread or the
/// assistant's runtime).
pub(super) enum AiEvent {
    /// A call that ran a statement: into the database's history.
    Ran(ToolCall),
    /// Statements to open in a tab (`propose_sql`).
    Proposed(SqlProposal),
    /// A read's result to open in a tab (`run_readonly` with `show`).
    Shown(ShownResult),
    /// A board tool's call, answered by the window (`answer_board_call`).
    Board(AiCaller, BoardCall),
}

/// Most rows a query marked `-- ai view` shows: the most AI tools get of one.
const AI_VIEW_ROWS: usize = 500;

/// The databases offered to AI tools.
#[derive(Default)]
pub(super) struct Ai {
    /// Offered now; every tool call reads it.
    pub(super) exposed: Arc<RwLock<Vec<Exposed>>>,
    /// The most every AI tool may see of any database (`ai.access`); every tool
    /// call reads it.
    pub(super) ceiling: Arc<RwLock<AiAccess>>,
    /// Whether the assistant was used in this window: then the databases are
    /// offered (and their AI connections opened) even while MCP is off.
    pub(super) assistant_used: bool,
}

impl Workspace {
    /// Whether anything uses AI tools, so the databases must be offered.
    fn ai_in_use(&self) -> bool {
        self.mcp.running() || self.ai.assistant_used
    }

    /// The ceiling of every AI tool now.
    pub(super) fn ai_ceiling(&self) -> AiAccess {
        *self
            .ai
            .ceiling
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The most AI tools may see of `db`: its own level under the ceiling. Schema
    /// only: no AI connection is opened for it.
    pub(super) fn ai_access(&self, db: &Database) -> AiAccess {
        db.ai_access.min(self.ai_ceiling())
    }

    /// Takes `ai.access` from the settings; a change applies to the next tool call.
    pub(super) fn sync_ai_access(&mut self, cx: &mut Context<Self>) {
        let ceiling = settings::current(cx).ai.access;
        if ceiling == self.ai_ceiling() {
            return;
        }
        *self
            .ai
            .ceiling
            .write()
            .unwrap_or_else(PoisonError::into_inner) = ceiling;
        self.expose_to_ai();
        self.check_ai_user(cx);
        self.push_ai_settings(cx);
        cx.notify();
    }

    /// Closes the AI connection of `user_id`'s database, if one is open: a
    /// schema-only database has none.
    pub(super) fn close_ai_connection(&mut self, user_id: &str) {
        let Ok(as_user) = assistant_user(&self.data.profiles, user_id) else {
            return;
        };
        if let Some(conn) = self.connections.get(Purpose::Assistant, &as_user) {
            self.engine.send(Command::Disconnect { conn });
            self.connections.forget(conn);
        }
    }

    /// Offers the connected databases that are available to AI tools, each on its
    /// assistant connection (the database's AI user), with what is known of it
    /// through its connected user (the active one first).
    pub(super) fn expose_to_ai(&mut self) {
        if !self.ai_in_use() {
            return;
        }
        let mut users: Vec<String> = self.conns.keys().cloned().collect();
        users.sort_by_key(|id| (Some(id) != self.active_user.as_ref(), id.clone()));
        let (mut seen, mut names, mut exposed) = (HashSet::new(), HashSet::new(), Vec::new());
        for user_id in users {
            let Some((db, _)) = self.find_user(&user_id) else {
                continue;
            };
            if !db.ai_tools || !seen.insert(db.id.clone()) {
                continue;
            }
            let db = db.clone();
            let access = self.ai_access(&db);
            let conn = if access.is_data() {
                // No AI connection: shown in the title bar, and the database isn't
                // offered.
                match self.connections.connect_assistant(
                    &self.engine,
                    &mut self.data.profiles,
                    &user_id,
                ) {
                    Ok(conn) => Some(conn),
                    Err(problem) => {
                        self.ai_problems.insert(db.id.clone(), problem);
                        continue;
                    }
                }
            } else {
                // Schema only: what is known through your connection, nothing more.
                self.close_ai_connection(&user_id);
                self.ai_problems.remove(&db.id);
                None
            };
            // Tools name a database by it, so no two may share one.
            let base = db.display_name();
            let name = (1..)
                .map(|n| match n {
                    1 => base.clone(),
                    n => format!("{base} ({n})"),
                })
                .find(|name| names.insert(name.clone()))
                .unwrap_or(base);
            exposed.push(Exposed {
                database: db.id.clone(),
                columns: Default::default(),
                name,
                user: user_id,
                conn,
                driver: db.driver,
                access,
                values: db.ai_values,
                bookmarks: self.ai_bookmarks(&db.id),
            });
        }
        *self
            .ai
            .exposed
            .write()
            .unwrap_or_else(PoisonError::into_inner) = exposed;
    }

    /// The databases offered now.
    pub(super) fn ai_exposed(&self) -> Vec<Exposed> {
        self.ai
            .exposed
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The bookmarks of database `db_id` as the assistant sees them.
    fn ai_bookmarks(&self, db_id: &str) -> Vec<SharedBookmark> {
        self.data
            .local
            .bookmarks_for(Some(db_id))
            .into_iter()
            .map(|b| SharedBookmark {
                name: b.name,
                sql: b.sql,
            })
            .collect()
    }

    /// A bookmark was saved or deleted: the bookmarks are offered as they are.
    pub(super) fn refresh_ai_bookmarks(&mut self) {
        let mut exposed = self
            .ai
            .exposed
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        for e in exposed.iter_mut() {
            if let Some((db, _)) = self.find_user(&e.user) {
                e.bookmarks = self.ai_bookmarks(&db.id.clone());
            }
        }
    }

    /// Runs `sql`, marked `-- ai view`, the way AI tools do: `run_readonly` through
    /// the registry, on the database's AI connection, under the AI tools' ceiling
    /// (audited as `ai view`). `tab` shows what they get, or why they are refused.
    pub(super) fn run_as_ai(
        &mut self,
        tab: &Entity<QueryTab>,
        sql: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = tab.read(cx).conn_id(cx) else {
            return;
        };
        let user_id = self
            .connections
            .tab_user(conn)
            .unwrap_or_default()
            .to_string();
        let Some(db_id) = self.find_user(&user_id).map(|(db, _)| db.id.clone()) else {
            return;
        };
        // Offered from now on, as once the assistant is used.
        self.ai.assistant_used = true;
        self.expose_to_ai();
        let name = self
            .ai_exposed()
            .into_iter()
            .find(|e| e.database == db_id)
            .map(|e| e.name);
        let query = QueryId(self.next_id());
        tab.update(cx, |t, cx| t.started(query, sql, cx));
        let Some(name) = name else {
            let why = self.ai_problems.get(&db_id).cloned().unwrap_or_else(|| {
                "this database isn't offered to AI tools (database form: AI tools)".into()
            });
            self.show_ai_view(tab, query, sql, Err(why), window, cx);
            return;
        };
        if self.assistant.events.is_none() {
            self.assistant.events = Some(self.ai_events(cx));
        }
        let Some(events) = self.assistant.events.as_ref().map(|(_, tx)| tx.clone()) else {
            return;
        };
        let ctx = self.ai_ctx_source("ai view", events, self.assistant.calls.clone())();
        let input = serde_json::json!({
            "sql": sql,
            "connection": name,
            "limit": AI_VIEW_ROWS,
        });
        let started = std::time::Instant::now();
        let job = async move {
            Registry::standard()
                .call(&ctx, "run_readonly", input)
                .await
                .map_err(|e| e.to_string())
        };
        // On the AI tools' runtime. Tests keep it on their own executor, which
        // allows no wake-ups from other threads.
        #[cfg(not(test))]
        let work = {
            let handle = tangul_ai::runtime::spawn(job);
            async move { handle.await.map_err(|e| e.to_string()).flatten() }
        };
        #[cfg(test)]
        let work = cx.background_spawn(job);
        let (tab, sql) = (tab.clone(), sql.to_string());
        cx.spawn_in(window, async move |this, cx| {
            let output = work.await.map(|output| (output, started.elapsed()));
            let _ = this.update_in(cx, |ws, window, cx| {
                ws.show_ai_view(&tab, query, &sql, output, window, cx)
            });
        })
        .detach();
    }

    /// Shows in `tab` what AI tools got for `sql`: the rows as handed to them, or
    /// why they were refused. Only what they got: it can't be sorted, filtered or
    /// paged (that would run as you).
    pub(super) fn show_ai_view(
        &mut self,
        tab: &Entity<QueryTab>,
        query: QueryId,
        sql: &str,
        output: Result<(ToolOutput, Duration), String>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let output = output.map(|(out, elapsed)| {
            let data = &out.data;
            let columns: Vec<ColumnInfo> = data["columns"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| ColumnInfo {
                    name: c.as_str().unwrap_or_default().to_string(),
                })
                .collect();
            let mut rows = RowBatch::default();
            for row in data["rows"].as_array().into_iter().flatten() {
                for cell in row.as_array().into_iter().flatten() {
                    rows.push_cell(cell.as_str());
                }
                rows.finish_row();
            }
            let mut result = ResultSet::new(columns);
            result.append(rows);
            QueryOutput {
                rows: Some(result.row_count() as u64),
                results: vec![result],
                truncated: data["truncated"].as_bool().unwrap_or(false),
                elapsed,
            }
        });
        let output = output.map_err(|e| format!("AI tools would get: {e}"));
        tab.update(cx, |t, cx| t.show_output(query, sql, output, false, cx));
    }

    /// A tool context for `caller` (`mcp`, `assistant`) over the databases offered
    /// at the time of the call, whose calls are audited and hand their statements,
    /// proposals and shown results to `events`.
    pub(super) fn ai_ctx_source(
        &self,
        caller: &'static str,
        events: async_channel::Sender<AiEvent>,
        calls: Arc<AtomicU64>,
    ) -> Arc<dyn Fn() -> Ctx + Send + Sync> {
        let (knowledge, engine) = (self.knowledge.clone(), self.engine.clone());
        let (exposed, ceiling) = (self.ai.exposed.clone(), self.ai.ceiling.clone());
        let dir = self.data.profiles.dir().map(std::path::Path::to_path_buf);
        let sink = self.ai_audit(events.clone(), calls);
        let proposals: ProposalSink = Arc::new({
            let events = events.clone();
            move |proposal| {
                let _ = events.try_send(AiEvent::Proposed(proposal));
            }
        });
        let shows: ShowSink = Arc::new({
            let events = events.clone();
            move |shown| {
                let _ = events.try_send(AiEvent::Shown(shown));
            }
        });
        let by = match caller {
            "mcp" => AiCaller::Mcp,
            _ => AiCaller::Assistant,
        };
        // Unanswered (the window is gone), the call fails.
        let boards: BoardSink = Arc::new(move |call| {
            let _ = events.try_send(AiEvent::Board(by, call));
        });
        Arc::new(move || {
            let mut exposed = exposed
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            // Each database's settings as they are now: a saved change applies at once.
            for e in &mut exposed {
                e.columns = database_settings::load(dir.as_deref(), &e.database)
                    .0
                    .ai
                    .content_blocking;
            }
            let ceiling = *ceiling.read().unwrap_or_else(PoisonError::into_inner);
            // Without a connection that may read data, the context has no engine.
            Ctx::new(knowledge.clone(), Some(engine.clone()), exposed)
                .with_ceiling(ceiling)
                .with_audit(caller, sink.clone())
                .with_proposals(proposals.clone())
                .with_shows(shows.clone())
                .with_boards(boards.clone())
        })
    }

    /// A channel whose events this window handles while the returned task lives.
    pub(super) fn ai_events(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (Task<()>, async_channel::Sender<AiEvent>) {
        let (tx, rx) = async_channel::unbounded::<AiEvent>();
        let task = cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv().await {
                // The workspace's window (there is one at a time).
                let handled = cx.update(|cx| {
                    let window = cx.windows().first().copied()?;
                    window
                        .update(cx, |_, window, cx| {
                            this.update(cx, |ws, cx| match event {
                                AiEvent::Ran(call) => ws.record_tool_call(call, cx),
                                AiEvent::Proposed(p) => ws.open_proposal(p, window, cx),
                                AiEvent::Shown(s) => ws.open_shown(s, window, cx),
                                AiEvent::Board(by, call) => {
                                    ws.answer_board_call(by, call, window, cx)
                                }
                            })
                            .ok()
                        })
                        .ok()
                        .flatten()
                });
                if handled.is_none() {
                    break;
                }
            }
        });
        (task, tx)
    }

    /// Writes each tool call to the audit log (its own handle on `local.db`, as the
    /// calls come from another thread), and hands the ones that ran a statement to
    /// the window for the history (a refused one never ran).
    fn ai_audit(&self, events: async_channel::Sender<AiEvent>, calls: Arc<AtomicU64>) -> AuditSink {
        let local = Mutex::new(Local::open(self.data.profiles.dir()));
        Arc::new(move |call: ToolCall| {
            calls.fetch_add(1, Ordering::Relaxed);
            if call.sql.is_some() && call.outcome != "refused" {
                let _ = events.try_send(AiEvent::Ran(call.clone()));
            }
            let entry = AuditEntry {
                at: call.at,
                caller: call.caller.into(),
                target: match &call.connection {
                    Some(c) => format!("tool:{} {c}", call.tool),
                    None => format!("tool:{}", call.tool),
                },
                sql: call.input,
                outcome: call.outcome.into(),
                error: call.error,
                duration_ms: Some(call.duration_ms),
                rows: None,
            };
            let local = local.lock().unwrap_or_else(PoisonError::into_inner);
            if let Err(e) = local.add_audit(&entry) {
                eprintln!("could not write the audit log: {e:#}");
            }
        })
    }

    /// Records the statement a tool call ran in its database's history, as run by
    /// an MCP client or the assistant.
    pub(super) fn record_tool_call(&mut self, call: ToolCall, cx: &mut Context<Self>) {
        let Some(sql) = call.sql.as_deref() else {
            return;
        };
        let user = {
            let exposed = self
                .ai
                .exposed
                .read()
                .unwrap_or_else(PoisonError::into_inner);
            let named = |e: &&Exposed| match &call.connection {
                Some(name) => e.name.eq_ignore_ascii_case(name),
                None => exposed.len() == 1,
            };
            exposed.iter().find(named).map(|e| e.user.clone())
        };
        let Some(user) = user else {
            return;
        };
        let Some(db_id) = self.find_user(&user).map(|(db, _)| db.id.clone()) else {
            return;
        };
        // Passwords (`CREATE ROLE … PASSWORD '…'`) are never kept. Run again from
        // history, it shows what the AI saw.
        let sql = ai_view::mark(&tangul_assist::redact::redact(sql));
        let fp = fingerprint(&sql);
        let entry = NewHistory {
            database_id: &db_id,
            user_id: &user,
            sql: &sql,
            fingerprint: &fp,
            entry: HistoryEntry {
                sql: sql.to_string(),
                source: if call.caller == "mcp" {
                    Source::Mcp
                } else {
                    Source::Ai
                },
                started_at: call.at - call.duration_ms,
                duration_ms: call.duration_ms,
                rows: call.rows.map(|n| n as i64),
                error: call.error,
            },
        };
        if let Err(e) = self.data.local.add_history(&entry) {
            eprintln!("could not record history: {e:#}");
        }
        if self.active_db.as_deref() == Some(db_id.as_str()) {
            self.refresh_history(cx);
        }
    }

    /// Opens a proposal in a new tab, not run, on your connection to its database; a
    /// chart it asks for is drawn once you run it.
    pub(super) fn open_proposal(
        &mut self,
        proposal: SqlProposal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = proposal
            .title
            .clone()
            .unwrap_or_else(|| format!("Proposed · {}", proposal.connection));
        self.open_sql(&title, &proposal.sql, window, cx);
        let Some(tab) = self.tabs.active_query().cloned() else {
            return;
        };
        let conn = self.tab_conn(&proposal.user);
        tab.update(cx, |t, cx| {
            if let Some(conn) = conn {
                t.set_conn(conn, cx);
            }
            if let Some(chart) = proposal.chart {
                t.request_chart(chart.style, chart.column, cx);
            }
        });
        cx.notify();
    }

    /// Opens a read the assistant ran in a new tab on your connection to its
    /// database, with its result (or chart) already there: it isn't run again.
    pub(super) fn open_shown(
        &mut self,
        shown: ShownResult,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = shown
            .title
            .clone()
            .unwrap_or_else(|| format!("Shown · {}", shown.connection));
        let text = match &shown.note {
            Some(note) => format!("-- Shown by the assistant: {note}\n{}", shown.sql),
            None => format!("-- Shown by the assistant\n{}", shown.sql),
        };
        self.open_sql(&title, &text, window, cx);
        let Some(tab) = self.tabs.active_query().cloned() else {
            return;
        };
        let conn = self.tab_conn(&shown.user);
        let query = QueryId(self.next_id());
        tab.update(cx, |t, cx| {
            if let Some(conn) = conn {
                t.set_conn(conn, cx);
            }
            if let Some(chart) = shown.chart {
                t.request_chart(chart.style, chart.column, cx);
            }
            t.show_result(
                query,
                &shown.sql,
                shown.result,
                shown.truncated,
                shown.elapsed,
                cx,
            );
        });
        cx.notify();
    }

    /// The names of the databases offered now (tests).
    #[cfg(test)]
    pub(super) fn ai_offered(&self) -> Vec<String> {
        self.ai_exposed().into_iter().map(|e| e.name).collect()
    }

    /// The bookmarks offered for the first database (tests).
    #[cfg(test)]
    pub(super) fn ai_offered_bookmarks(&self) -> Vec<SharedBookmark> {
        self.ai_exposed()
            .first()
            .map(|e| e.bookmarks.clone())
            .unwrap_or_default()
    }

    /// Where the tool calls of a test hand their events.
    #[cfg(test)]
    pub(super) fn test_events(&mut self, cx: &mut Context<Self>) -> async_channel::Sender<AiEvent> {
        let (task, events) = self.ai_events(cx);
        task.detach();
        events
    }

    /// A proposal, as a tool call hands it over (tests).
    #[cfg(test)]
    pub(super) fn ai_proposals(&mut self, cx: &mut Context<Self>) -> ProposalSink {
        let events = self.test_events(cx);
        Arc::new(move |proposal| {
            let _ = events.try_send(AiEvent::Proposed(proposal));
        })
    }

    /// A result to show, as a tool call hands it over (tests).
    #[cfg(test)]
    pub(super) fn ai_shows(&mut self, cx: &mut Context<Self>) -> ShowSink {
        let events = self.test_events(cx);
        Arc::new(move |shown| {
            let _ = events.try_send(AiEvent::Shown(shown));
        })
    }
}
