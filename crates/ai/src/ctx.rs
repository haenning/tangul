//! What a tool call may see and use: the one security layer below the registry.

use std::sync::Arc;
use std::time::Duration;

use tangul_assist::schema::SchemaIndex;
use tangul_assist::slice::KnownValues;
use tangul_assist::usage::Usage;
use tangul_core::chart::ChartStyle;
use tangul_core::{ConnId, DriverKind, ResultSet, ValueAccess};
use tangul_engine::EngineHandle;
use tangul_service::knowledge::Knowledge;

use crate::audit::AuditSink;
use crate::results::Results;
use crate::tool::ToolError;

/// A database the user makes available to the assistant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exposed {
    /// How tools and models name it, e.g. `app@localhost`.
    pub name: String,
    /// The database user id whose schema, usage and profiles the knowledge pool
    /// holds (the one open in the app).
    pub user: String,
    /// The connection the assistant's queries run on: as the database's AI user
    /// (`service::connections::connect_assistant`), never a tab's.
    pub conn: ConnId,
    pub driver: DriverKind,
    /// What the assistant may read from the rows (value profiles).
    pub values: ValueAccess,
    /// The user's bookmarks for this database (`list_bookmarks`, `run_readonly`).
    pub bookmarks: Vec<SharedBookmark>,
}

/// A bookmark as the assistant sees it: its name and SQL (passwords are redacted
/// before bookmarks are saved).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedBookmark {
    pub name: String,
    pub sql: String,
}

/// The context of every tool call: the knowledge pool, the engine for queries, and
/// only the connections exposed to the assistant.
#[derive(Clone)]
pub struct Ctx {
    knowledge: Knowledge,
    engine: Option<EngineHandle>,
    exposed: Vec<Exposed>,
    /// Who calls (`mcp`, `assistant`) and where its calls are recorded.
    audit: Option<(&'static str, AuditSink)>,
    /// Where `propose_sql` sends statements for the user; none: it can't.
    proposals: Option<ProposalSink>,
    /// Where `run_readonly` sends results to show the user; none: it can't.
    shows: Option<ShowSink>,
    /// Results whose cells the text cut, for `get_cell`.
    results: Results,
}

/// Statements the assistant proposes: the app opens them in a new tab, unrun, on
/// the user's connection. Only changes: reads are shown with [`ShownResult`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlProposal {
    /// The exposed connection's name.
    pub connection: String,
    /// The database user whose tab connection gets it (see [`Exposed::user`]).
    pub user: String,
    pub sql: String,
    /// The tab's title; `None`: tangul names it.
    pub title: Option<String>,
}

/// Where proposals go.
pub type ProposalSink = Arc<dyn Fn(SqlProposal) + Send + Sync>;

/// A read the assistant ran (`run_readonly` with `show`): the app opens it in a new
/// tab on the user's connection with this result already in it, so it isn't run
/// again. Filtering, sorting and paging it later are the user's own runs.
#[derive(Debug)]
pub struct ShownResult {
    /// The exposed connection's name.
    pub connection: String,
    /// The database user whose tab connection gets it (see [`Exposed::user`]).
    pub user: String,
    /// The statement as it ran.
    pub sql: String,
    /// The tab's title; `None`: tangul names it.
    pub title: Option<String>,
    /// One line on what it shows, above the SQL.
    pub note: Option<String>,
    /// Show it as this chart instead of the grid.
    pub chart: Option<ChartRequest>,
    pub result: ResultSet,
    /// The result had more rows than tangul keeps.
    pub truncated: bool,
    pub elapsed: Duration,
}

/// A chart asked for by the assistant. If the result can't be one, tangul shows
/// the grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChartRequest {
    pub style: ChartStyle,
    /// One value column; `None`: all of them.
    pub column: Option<String>,
}

/// Where results to show go.
pub type ShowSink = Arc<dyn Fn(ShownResult) + Send + Sync>;

/// A connection resolved for a call.
pub(crate) struct Conn {
    /// How the client names it.
    pub name: String,
    pub conn: ConnId,
    pub schema: Arc<SchemaIndex>,
    /// What the user's queries on this database used (learned joins, …).
    pub usage: Arc<Usage>,
    /// Sampled values of its columns; empty unless the user allows reading values.
    pub known_values: KnownValues,
    /// The database user in the knowledge pool, and what may be read from rows.
    pub user: String,
    pub values: ValueAccess,
}

impl Ctx {
    /// `engine` is `None` where no query may run (tests of the knowledge tools).
    pub fn new(knowledge: Knowledge, engine: Option<EngineHandle>, exposed: Vec<Exposed>) -> Self {
        Self {
            knowledge,
            engine,
            exposed,
            audit: None,
            proposals: None,
            shows: None,
            results: Results::default(),
        }
    }

    /// Records every tool call as `caller`'s in `sink`.
    pub fn with_audit(mut self, caller: &'static str, sink: AuditSink) -> Self {
        self.audit = Some((caller, sink));
        self
    }

    pub(crate) fn audit(&self) -> Option<&(&'static str, AuditSink)> {
        self.audit.as_ref()
    }

    /// Sends what `propose_sql` proposes to `sink`.
    pub fn with_proposals(mut self, sink: ProposalSink) -> Self {
        self.proposals = Some(sink);
        self
    }

    pub(crate) fn proposals(&self) -> Option<&ProposalSink> {
        self.proposals.as_ref()
    }

    /// Sends what `run_readonly` shows to `sink`.
    pub fn with_shows(mut self, sink: ShowSink) -> Self {
        self.shows = Some(sink);
        self
    }

    pub(crate) fn shows(&self) -> Option<&ShowSink> {
        self.shows.as_ref()
    }

    /// Keeps results in `results` (shared by the calls of one client).
    pub fn with_results(mut self, results: Results) -> Self {
        self.results = results;
        self
    }

    pub(crate) fn results(&self) -> &Results {
        &self.results
    }

    pub fn exposed(&self) -> &[Exposed] {
        &self.exposed
    }

    pub(crate) fn knowledge(&self) -> &Knowledge {
        &self.knowledge
    }

    /// The exposed connection called `name`, or the only one when it is left out.
    /// The exposed connection called `name`, or the only one, without what the
    /// knowledge pool holds (for tools that need no schema).
    pub(crate) fn exposed_one(&self, name: Option<&str>) -> Result<&Exposed, ToolError> {
        let names = || {
            self.exposed
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        match name {
            Some(name) => self
                .exposed
                .iter()
                .find(|e| e.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| {
                    ToolError::NotFound(format!("connection {name}; available: {}", names()))
                }),
            None => match self.exposed.as_slice() {
                [only] => Ok(only),
                [] => Err(ToolError::NotAllowed(
                    "no connection is enabled for the assistant".into(),
                )),
                _ => Err(ToolError::BadInput(format!(
                    "several connections; name one of: {}",
                    names()
                ))),
            },
        }
    }

    pub(crate) fn conn(&self, name: Option<&str>) -> Result<Conn, ToolError> {
        let exposed = self.exposed_one(name)?;
        let known = self
            .knowledge
            .user(&exposed.user)
            .ok_or_else(|| ToolError::Failed(format!("{} is not connected yet", exposed.name)))?;
        let known_values = if exposed.values.values() {
            self.knowledge.known_values(&exposed.user)
        } else {
            KnownValues::new()
        };
        Ok(Conn {
            name: exposed.name.clone(),
            conn: exposed.conn,
            known_values,
            user: exposed.user.clone(),
            values: exposed.values,
            usage: self.knowledge.usage(&known.database).unwrap_or_default(),
            schema: known.schema,
        })
    }

    pub(crate) fn engine(&self) -> Result<&EngineHandle, ToolError> {
        self.engine
            .as_ref()
            .ok_or_else(|| ToolError::NotAllowed("queries are not available here".into()))
    }
}
