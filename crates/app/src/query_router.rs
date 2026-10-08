//! Engine answers to where they belong: a query's events to the tab it runs for,
//! a check's problems to the editor that asked. Keeps what is needed to record a
//! finished run in history.

use std::collections::HashMap;

use gpui_kit::*;
use tangul_assist::ai_view::{self, Mode, Plan};
use tangul_core::QueryId;
use tangul_engine::{ExecEvent, Problem, QueryEvent};

use crate::query_tab::QueryTab;

/// A query sent to the engine, kept until it finishes.
pub struct RunningQuery {
    pub tab: WeakEntity<QueryTab>,
    pub sql: String,
    pub user_id: String,
    /// Unix milliseconds.
    pub started_at: i64,
    /// A filtered, sorted or further page of an earlier result: not a run of its
    /// own (no history, usage or bookmark hint).
    pub derived: bool,
}

/// A run that finished, for history.
pub struct Finished {
    pub run: RunningQuery,
    pub error: Option<String>,
    pub elapsed_ms: i64,
}

#[derive(Default)]
pub struct QueryRouter {
    running: HashMap<QueryId, RunningQuery>,
    /// Runs shown as AI tools see them (`-- ai view`), and the modes of the
    /// columns of the result arriving.
    ai_views: HashMap<QueryId, (Plan, Vec<Mode>)>,
    /// Pending editor checks: request → (tab, edit generation).
    checks: HashMap<u64, (WeakEntity<QueryTab>, u64)>,
}

impl QueryRouter {
    pub fn started(&mut self, query: QueryId, run: RunningQuery) {
        self.running.insert(query, run);
    }

    /// Nothing is running.
    pub fn is_idle(&self) -> bool {
        self.running.is_empty()
    }

    /// Hands the event to the query's tab; returns the run once it finished.
    /// Shows `query`'s result as AI tools see it.
    pub fn view_as_ai(&mut self, query: QueryId, plan: Plan) {
        self.ai_views.insert(query, (plan, Vec::new()));
    }

    pub fn deliver(&mut self, query: QueryId, event: QueryEvent, cx: &mut App) -> Option<Finished> {
        let event = match (self.ai_views.get_mut(&query), event) {
            (Some((plan, modes)), QueryEvent::Exec(ExecEvent::ResultStart(columns))) => {
                *modes = plan.modes(columns.len());
                QueryEvent::Exec(ExecEvent::ResultStart(ai_view::columns(modes, &columns)))
            }
            (Some((_, modes)), QueryEvent::Exec(ExecEvent::Rows(rows))) => {
                QueryEvent::Exec(ExecEvent::Rows(ai_view::batch(modes, &rows)))
            }
            (_, event) => event,
        };
        if matches!(event, QueryEvent::Finished { .. }) {
            self.ai_views.remove(&query);
        }
        let finished = match &event {
            QueryEvent::Finished { elapsed, error, .. } => {
                Some((error.clone(), elapsed.as_millis() as i64))
            }
            QueryEvent::Exec(_) => None,
        };
        if let Some(tab) = self.running.get(&query).and_then(|q| q.tab.upgrade()) {
            tab.update(cx, |t, cx| t.on_query_event(event, cx));
        }
        let (error, elapsed_ms) = finished?;
        let run = self.running.remove(&query)?;
        Some(Finished {
            run,
            error,
            elapsed_ms,
        })
    }

    /// `request` checks the SQL of `tab` at edit `generation`. The engine doesn't
    /// answer when no session was free: only the latest request per tab is kept,
    /// and none for closed tabs.
    pub fn checking(&mut self, request: u64, tab: WeakEntity<QueryTab>, generation: u64) {
        self.checks
            .retain(|_, (t, _)| *t != tab && t.upgrade().is_some());
        self.checks.insert(request, (tab, generation));
    }

    pub fn deliver_check(&mut self, request: u64, problems: Vec<Problem>, cx: &mut App) {
        if let Some((tab, generation)) = self.checks.remove(&request)
            && let Some(tab) = tab.upgrade()
        {
            tab.update(cx, |t, cx| t.apply_validation(generation, problems, cx));
        }
    }
}
