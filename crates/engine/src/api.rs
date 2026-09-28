//! What front ends send the engine and get back.

use std::time::Duration;

use tangul_core::{
    ConnId, ConnectConfig, DriverKind, QueryId, ResultSet, SchemaInfo, SshTarget, TabId,
};
use tangul_drivers::ExecEvent;
use tokio::sync::oneshot;

use crate::ROW_LIMIT;
use crate::gate::{Approval, Caller, Proposal};
use crate::pool::PoolStats;

#[derive(Debug)]
pub enum Command {
    /// Opens the pool and verifies credentials by loading the schema. With `ssh`,
    /// sessions go through a tunnel and `cfg.host`/`port` are as the SSH server sees them.
    Connect {
        conn: ConnId,
        driver: DriverKind,
        cfg: ConnectConfig,
        ssh: Option<SshTarget>,
    },
    /// The user confirmed an unknown SSH host key: add it to `~/.ssh/known_hosts`.
    TrustHostKey {
        host: String,
        port: u16,
        key: String,
    },
    Disconnect {
        conn: ConnId,
    },
    RefreshSchema {
        conn: ConnId,
    },
    Execute {
        query: QueryId,
        tab: TabId,
        conn: ConnId,
        sql: String,
        /// The user's go-ahead from a dialog, if one was needed. Tabs are the user's
        /// only: the assistant and schedules run through [`Command::Query`], so
        /// their statements never touch a tab's session.
        approval: Approval,
    },
    /// `settings.json` changed; read it again now instead of within the second.
    ReloadSettings,
    /// Runs `sql` on a spare session (never a tab's) and answers on `reply` with the
    /// whole result instead of streaming events. See [`EngineHandle::query`].
    Query {
        query: QueryId,
        conn: ConnId,
        sql: String,
        options: QueryOptions,
        reply: oneshot::Sender<Result<QueryOutput, QueryError>>,
    },
    Cancel {
        query: QueryId,
    },
    /// Checks statements without running them (prepare only), on a spare session so
    /// an open transaction of a tab is never touched. `statements` are
    /// (byte offset in the editor, text).
    Validate {
        request: u64,
        conn: ConnId,
        statements: Vec<(usize, String)>,
    },
    /// Tab closed or switched connection: drop its pinned session (rolls back).
    ReleaseTab {
        tab: TabId,
        conn: ConnId,
    },
}

#[derive(Debug)]
pub enum Event {
    Connected {
        conn: ConnId,
    },
    ConnectFailed {
        conn: ConnId,
        error: String,
    },
    /// The SSH server's key is not in known_hosts. Connecting stopped; after the user
    /// confirms, send `TrustHostKey` and connect again.
    HostKeyUnknown {
        conn: ConnId,
        host: String,
        port: u16,
        fingerprint: String,
        key: String,
    },
    /// A connection's schema, sent twice per load: first tables, columns and
    /// foreign keys (what the schema tree and completion need, as soon as they are
    /// there), then `complete` with indexes, checks, enums, views and routines.
    Schema {
        conn: ConnId,
        result: Result<SchemaInfo, String>,
        complete: bool,
    },
    Query {
        query: QueryId,
        event: QueryEvent,
    },
    PoolStats {
        conn: ConnId,
        stats: PoolStats,
    },
    /// Result of `Validate`: one entry per failing statement.
    Validated {
        request: u64,
        problems: Vec<Problem>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Index into the request's statements.
    pub statement: usize,
    /// Byte offset within that statement, when the server said where.
    pub position: Option<usize>,
    pub message: String,
}

#[derive(Debug)]
pub enum QueryEvent {
    Exec(ExecEvent),
    Finished {
        elapsed: Duration,
        error: Option<String>,
        /// Byte offset of the error in the executed SQL, when the server reported one.
        error_position: Option<usize>,
        /// The tab's session stays pinned while this is true.
        in_transaction: bool,
        /// The user cancelled the query. Not every cancel produces an error (MySQL's
        /// `SLEEP()` just returns 1), so this is reported separately.
        cancelled: bool,
    },
}

/// How a [`Command::Query`] runs. Read-only mode and the time limit are the gate's
/// decision, not the caller's.
#[derive(Debug, Clone)]
pub struct QueryOptions {
    pub caller: Caller,
    pub approval: Approval,
    /// Rows kept per result set; the rest are dropped and `truncated` is set.
    pub row_limit: usize,
}

impl QueryOptions {
    pub fn new(caller: Caller) -> Self {
        Self {
            caller,
            approval: Approval::None,
            row_limit: ROW_LIMIT,
        }
    }
}

/// The whole result of a [`Command::Query`].
#[derive(Debug, Default)]
pub struct QueryOutput {
    /// One per statement that returned rows.
    pub results: Vec<ResultSet>,
    /// Rows the last statement returned or changed, as the server reported them.
    pub rows: Option<u64>,
    /// A result set had more rows than `row_limit`.
    pub truncated: bool,
    pub elapsed: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryError {
    pub message: String,
    /// Byte offset of the error in the SQL, when the server said where.
    pub position: Option<usize>,
    /// Not run: the assistant's statements that aren't reads, for the user to open
    /// and run as themselves.
    pub proposal: Option<Proposal>,
}

impl QueryError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            position: None,
            proposal: None,
        }
    }
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for QueryError {}
