//! Scheduled bookmark runs: which bookmark is due, and the state of the one run in
//! progress. Runs happen only while the app is quiet (no input, no query running),
//! one at a time, read-only, on hidden connections the UI never shows. The
//! workspace owns the engine and does the sending; see `Workspace::monitor_*`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tangul_assist::guard;
use tangul_core::{ConnId, QueryId};

use crate::local::Bookmark;

/// No input for this long counts as quiet.
pub const QUIET: Duration = Duration::from_secs(120);
/// How often the workspace checks for due bookmarks.
pub const TICK: Duration = Duration::from_secs(30);
/// Longer than the query time limit plus connecting: the run is given up.
const STUCK_GRACE: Duration = Duration::from_secs(60);

/// A hidden connection for scheduled runs, per database user.
struct HiddenConn {
    id: ConnId,
    ready: bool,
}

/// The scheduled run in progress.
pub struct Pending {
    pub bookmark: Bookmark,
    pub conn: ConnId,
    /// Set once the query was sent (after connecting).
    pub query: Option<QueryId>,
    /// Unix milliseconds.
    pub started_at: i64,
    started: Instant,
    /// Rows of the last statement so far.
    pub rows: Option<i64>,
}

pub struct Monitor {
    conns: HashMap<String, HiddenConn>,
    users: HashMap<ConnId, String>,
    pub pending: Option<Pending>,
    last_input: Instant,
}

impl Default for Monitor {
    fn default() -> Self {
        Self {
            conns: HashMap::new(),
            users: HashMap::new(),
            pending: None,
            last_input: Instant::now(),
        }
    }
}

impl Monitor {
    /// The user did something; scheduled runs wait until it's quiet again.
    pub fn touch(&mut self) {
        self.last_input = Instant::now();
    }

    pub fn is_quiet(&self) -> bool {
        self.last_input.elapsed() >= QUIET
    }

    /// Whether `conn` is one of the hidden connections.
    pub fn owns(&self, conn: ConnId) -> bool {
        self.users.contains_key(&conn)
    }

    /// The hidden connection for `user_id` and whether it is up; `None` when there
    /// is none yet, and the caller connects and calls [`Self::connecting`].
    pub fn conn_for(&self, user_id: &str) -> Option<(ConnId, bool)> {
        self.conns.get(user_id).map(|c| (c.id, c.ready))
    }

    pub fn connecting(&mut self, user_id: &str, id: ConnId) {
        self.conns
            .insert(user_id.to_string(), HiddenConn { id, ready: false });
        self.users.insert(id, user_id.to_string());
    }

    pub fn connected(&mut self, id: ConnId) {
        if let Some(conn) = self.users.get(&id).and_then(|u| self.conns.get_mut(u)) {
            conn.ready = true;
        }
    }

    /// Forgets a connection that failed, so the next run connects again.
    pub fn failed(&mut self, id: ConnId) {
        if let Some(user) = self.users.remove(&id) {
            self.conns.remove(&user);
        }
    }

    pub fn start(&mut self, bookmark: Bookmark, conn: ConnId, started_at: i64) {
        self.pending = Some(Pending {
            bookmark,
            conn,
            query: None,
            started_at,
            started: Instant::now(),
            rows: None,
        });
    }

    /// The run in progress, if it has been going for much longer than it may.
    pub fn stuck(&self, limit: Option<Duration>) -> bool {
        self.pending.as_ref().is_some_and(|p| {
            let allowed = limit.unwrap_or(Duration::from_secs(3600)) + STUCK_GRACE;
            p.started.elapsed() > allowed
        })
    }
}

/// Whether a bookmark may run on a schedule: it is flagged, belongs to one database
/// and doesn't change data.
pub fn schedulable(bookmark: &Bookmark) -> bool {
    bookmark.scheduled
        && bookmark.database_id.is_some()
        && !guard::check(&bookmark.sql).iter().any(|c| c.writes)
}

/// The scheduled bookmark that waited longest past `interval` since its last run
/// (by hand or scheduled); one that never ran comes first.
pub fn due(
    bookmarks: &[Bookmark],
    last_run: impl Fn(&str) -> Option<i64>,
    now_ms: i64,
    interval: Duration,
) -> Option<&Bookmark> {
    let interval = i64::try_from(interval.as_millis()).unwrap_or(i64::MAX);
    bookmarks
        .iter()
        .filter(|b| schedulable(b))
        .map(|b| (last_run(&b.id), b))
        .filter(|(last, _)| last.is_none_or(|t| now_ms.saturating_sub(t) >= interval))
        .min_by_key(|(last, _)| last.unwrap_or(i64::MIN))
        .map(|(_, b)| b)
}

/// `"5 minutes"`, `"2 hours"`, `"1 day"`: the largest whole unit.
pub fn human_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let (n, unit) = if secs.is_multiple_of(86_400) {
        (secs / 86_400, "day")
    } else if secs.is_multiple_of(3600) {
        (secs / 3600, "hour")
    } else {
        (secs.div_ceil(60), "minute")
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bookmark(id: &str, sql: &str) -> Bookmark {
        Bookmark {
            id: id.into(),
            name: id.into(),
            sql: sql.into(),
            database_id: Some("db".into()),
            scheduled: true,
            ..Default::default()
        }
    }

    #[test]
    fn picks_the_longest_waiting_due_bookmark() {
        let hour = Duration::from_secs(3600);
        let list = vec![
            bookmark("recent", "select 1"),
            bookmark("old", "select 2"),
            bookmark("older", "select 3"),
        ];
        let last = |id: &str| match id {
            "recent" => Some(9_000_000),
            "old" => Some(5_000_000),
            "older" => Some(1_000_000),
            _ => None,
        };
        let now = 9_500_000;
        assert_eq!(due(&list, last, now, hour).unwrap().id, "older");
        // Nothing waited an hour yet.
        assert!(due(&list, |_| Some(now - 1000), now, hour).is_none());
        // Never ran: first.
        let mut with_new = list;
        with_new.push(bookmark("new", "select 4"));
        assert_eq!(due(&with_new, last, now, hour).unwrap().id, "new");
    }

    #[test]
    fn only_read_only_bookmarks_of_one_database() {
        let hour = Duration::from_secs(3600);
        let mut global = bookmark("global", "select 1");
        global.database_id = None;
        let mut off = bookmark("off", "select 1");
        off.scheduled = false;
        let list = vec![
            global,
            off,
            bookmark("writes", "delete from t where id = 1"),
            bookmark("reads", "select * from t"),
        ];
        assert_eq!(due(&list, |_| None, 0, hour).unwrap().id, "reads");
    }

    #[test]
    fn durations_in_words() {
        let d = |s| human_duration(Duration::from_secs(s));
        assert_eq!(d(300), "5 minutes");
        assert_eq!(d(3600), "1 hour");
        assert_eq!(d(2 * 86_400), "2 days");
        assert_eq!(d(90), "2 minutes");
    }

    #[test]
    fn hidden_connections() {
        let mut m = Monitor::default();
        assert_eq!(m.conn_for("u"), None);
        m.connecting("u", ConnId(7));
        assert!(m.owns(ConnId(7)));
        assert_eq!(m.conn_for("u"), Some((ConnId(7), false)));
        m.connected(ConnId(7));
        assert_eq!(m.conn_for("u"), Some((ConnId(7), true)));
        m.failed(ConnId(7));
        assert!(!m.owns(ConnId(7)));
        assert_eq!(m.conn_for("u"), None);
        assert!(!m.is_quiet());
    }
}
