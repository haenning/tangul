//! Backend: owns a tokio runtime on its own thread and all database sessions.
//! Front ends talk to it only through `Command`s and receive `Event`s (or the whole
//! result of [`EngineHandle::query`]), so a UI thread never blocks on the network.
//! Every statement passes the [`gate`] first, which decides from the engine's own
//! copy of `settings.json` whether and how it runs for the caller.

mod api;
mod audit;
mod engine;
pub mod gate;
mod pool;
mod run;
mod schema;

use std::path::PathBuf;

pub use api::{Command, Event, Problem, QueryError, QueryEvent, QueryOptions, QueryOutput};
use engine::Engine;
pub use gate::{
    Approval, Caller, Decision, Guard, Mode, Proposal, Run, SettingsSource, decide, user_run,
};
pub use pool::PoolStats;
use tangul_core::{ConnId, QueryId};
pub use tangul_drivers::ExecEvent;
use tokio::sync::{mpsc, oneshot};

/// Max rows kept per result set. TODO(fetch-more): make configurable / page on demand.
pub const ROW_LIMIT: usize = 10_000;
/// Max server connections per profile (tabs + schema browsing share them).
pub const MAX_CONNECTIONS: usize = 4;

/// Cheap to clone; sending never blocks.
#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::UnboundedSender<Command>,
}

impl EngineHandle {
    pub fn send(&self, cmd: Command) {
        // Only fails if the engine thread died; nothing sensible to do then.
        let _ = self.tx.send(cmd);
    }

    /// Runs `sql` on `conn` and returns the whole result, for callers that want an
    /// answer rather than a stream (scheduled runs, the assistant). Works from any
    /// executor. `query` identifies it for [`Command::Cancel`].
    pub async fn query(
        &self,
        query: QueryId,
        conn: ConnId,
        sql: impl Into<String>,
        options: QueryOptions,
    ) -> Result<QueryOutput, QueryError> {
        let (reply, answer) = oneshot::channel();
        self.send(Command::Query {
            query,
            conn,
            sql: sql.into(),
            options,
            reply,
        });
        answer
            .await
            .unwrap_or_else(|_| Err(QueryError::new("the engine stopped")))
    }
}

/// Where the engine takes its settings from and writes its audit log.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub settings: SettingsSource,
    /// Directory of the `local.db` that gets the audit log; `None` keeps none.
    pub audit_dir: Option<PathBuf>,
}

impl EngineConfig {
    /// The app's: `settings.json` and `local.db` in the config directory.
    pub fn from_config_dir() -> Self {
        Self {
            settings: SettingsSource::File(tangul_data::settings::path()),
            audit_dir: tangul_data::config::config_dir(),
        }
    }

    /// Default settings, no audit log (tests).
    pub fn isolated() -> Self {
        Self {
            settings: SettingsSource::Fixed(Box::default()),
            audit_dir: None,
        }
    }
}

/// Starts the engine with the app's settings file and audit log.
pub fn start() -> (EngineHandle, async_channel::Receiver<Event>) {
    start_with(EngineConfig::from_config_dir())
}

/// A handle to no engine: commands are dropped and no event ever arrives. For UI
/// tests, whose deterministic scheduler must not be woken from another thread.
pub fn detached() -> (EngineHandle, async_channel::Receiver<Event>) {
    let (tx, _) = mpsc::unbounded_channel();
    let (_, events_rx) = async_channel::unbounded();
    (EngineHandle { tx }, events_rx)
}

/// Starts the engine thread. Events arrive on the returned receiver, which can be
/// awaited from any executor (the UI uses GPUI's).
pub fn start_with(config: EngineConfig) -> (EngineHandle, async_channel::Receiver<Event>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (events_tx, events_rx) = async_channel::unbounded();
    std::thread::Builder::new()
        .name("tangul-engine".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(Engine::new(events_tx, config).run(rx));
        })
        .expect("engine thread");
    (EngineHandle { tx }, events_rx)
}
