//! When scheduled bookmarks run: only while the app is quiet (no input, no query
//! running), one at a time. Which bookmark is due and whether it may run are
//! decided in `tangul_service` (`schedule`, `policy`); see `Workspace::monitor_tick`.

use std::time::{Duration, Instant};

use gpui_kit::Task;
use tangul_core::ConnId;
use tangul_data::local::Bookmark;

/// No input for this long counts as quiet.
pub const QUIET: Duration = Duration::from_secs(120);
/// How often the workspace checks for due bookmarks.
pub const TICK: Duration = Duration::from_secs(30);
/// Longer than the query time limit plus connecting: the run is given up.
const STUCK_GRACE: Duration = Duration::from_secs(60);

/// The scheduled run in progress.
pub struct Pending {
    pub bookmark: Bookmark,
    /// The hidden connection it runs on.
    pub conn: ConnId,
    /// Unix milliseconds.
    pub started_at: i64,
    started: Instant,
    /// Awaits the result; dropping it gives the run up.
    _run: Task<()>,
}

impl Pending {
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

pub struct Monitor {
    pub pending: Option<Pending>,
    last_input: Instant,
}

impl Default for Monitor {
    fn default() -> Self {
        Self {
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

    pub fn start(&mut self, bookmark: Bookmark, conn: ConnId, started_at: i64, run: Task<()>) {
        self.pending = Some(Pending {
            bookmark,
            conn,
            started_at,
            started: Instant::now(),
            _run: run,
        });
    }

    /// The run in progress, if it has been going for much longer than it may.
    pub fn stuck(&self, limit: Option<Duration>) -> bool {
        self.pending.as_ref().is_some_and(|p| {
            let allowed = limit.unwrap_or(Duration::from_secs(3600)) + STUCK_GRACE;
            p.elapsed() > allowed
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_quiet_right_after_start() {
        let m = Monitor::default();
        assert!(!m.is_quiet());
        assert!(!m.stuck(None));
    }
}
