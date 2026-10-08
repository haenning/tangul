//! What a tool call may see and use: the one security layer below the registry.

use std::sync::Arc;
use std::time::Duration;

use tangul_assist::lexer::fingerprint;
use tangul_assist::schema::SchemaIndex;
use tangul_assist::slice::KnownValues;
use tangul_assist::usage::Usage;
use tangul_core::board::{Board, Tile};
use tangul_core::chart::ChartStyle;
use tangul_core::{AiAccess, ConnId, DriverKind, ResultSet, ValueAccess};
use tangul_engine::EngineHandle;
use tangul_service::boards::Edit;
use tangul_service::knowledge::Knowledge;
use tokio::sync::oneshot;

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
    /// (`service::connections::connect_assistant`), never a tab's. `None` for a
    /// database with schema-only access: nothing the AI does reaches its server.
    pub conn: Option<ConnId>,
    pub driver: DriverKind,
    /// What the assistant may see: data, or only the schema.
    pub access: AiAccess,
    /// What the assistant may read from the rows (value profiles).
    pub values: ValueAccess,
    /// The user's bookmarks for this database (`list_bookmarks`, `run_readonly`).
    pub bookmarks: Vec<SharedBookmark>,
}

impl Exposed {
    /// Whether tools that read rows may run on it: data access and a connection.
    pub fn reads_data(&self) -> bool {
        self.access.is_data() && self.conn.is_some()
    }

    /// Itself under `ceiling`; with schema-only access it has no connection, no
    /// value profiles, and its bookmarks only as fingerprints (literals and
    /// comments, where values hide, gone).
    fn limited(mut self, ceiling: AiAccess) -> Self {
        self.access = self.access.min(ceiling);
        if !self.access.is_data() {
            self.conn = None;
            self.values = ValueAccess::None;
            for b in &mut self.bookmarks {
                b.sql = fingerprint(&b.sql);
            }
        }
        self
    }
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
    /// Where the board tools ask for boards; none: there are none.
    boards: Option<BoardSink>,
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

/// What a board tool asks of the app, which keeps the boards and changes them
/// through `service::boards` (the AI's changes with `apply_by_ai`: only at the
/// revision read, marked as the caller's). `user` names the database as
/// [`Exposed::user`] does.
#[derive(Debug, Clone, PartialEq)]
pub enum BoardRequest {
    /// The database's boards (not the archived ones).
    List { user: String },
    Create {
        user: String,
        name: String,
        description: String,
        tiles: Vec<Tile>,
    },
    Edit {
        user: String,
        board: String,
        revision: String,
        edits: Vec<Edit>,
    },
}

/// The app's answer to a [`BoardRequest`].
#[derive(Debug, Clone, PartialEq)]
pub enum BoardReply {
    Boards(Vec<Board>),
    /// The board as it is now.
    Board(Board),
}

/// A [`BoardRequest`] and where its answer goes (a refusal as text).
#[derive(Debug)]
pub struct BoardCall {
    pub request: BoardRequest,
    pub reply: oneshot::Sender<Result<BoardReply, String>>,
}

impl BoardCall {
    /// A call for `request` and where its answer arrives.
    pub fn new(request: BoardRequest) -> (Self, oneshot::Receiver<Result<BoardReply, String>>) {
        let (reply, answer) = oneshot::channel();
        (Self { request, reply }, answer)
    }
}

/// Where board calls go.
pub type BoardSink = Arc<dyn Fn(BoardCall) + Send + Sync>;

/// A connection resolved for a call.
pub(crate) struct Conn {
    /// How the client names it.
    pub name: String,
    /// `None`: schema only (see [`Conn::session`]).
    pub conn: Option<ConnId>,
    pub schema: Arc<SchemaIndex>,
    /// What the user's queries on this database used (learned joins, …).
    pub usage: Arc<Usage>,
    /// Sampled values of its columns; empty unless the user allows reading values.
    pub known_values: KnownValues,
    /// The database user in the knowledge pool, and what may be read from rows.
    pub user: String,
    pub access: AiAccess,
    pub values: ValueAccess,
}

impl Conn {
    /// The AI connection rows are read on; refused for a schema-only database.
    pub fn session(&self) -> Result<ConnId, ToolError> {
        self.conn.ok_or_else(|| {
            ToolError::NotAllowed(format!("{} is schema only: the AI sees no data", self.name))
        })
    }
}

impl Ctx {
    /// `engine` is `None` where no query may run (tests of the knowledge tools). It
    /// is dropped when no connection allows reading data.
    pub fn new(knowledge: Knowledge, engine: Option<EngineHandle>, exposed: Vec<Exposed>) -> Self {
        Self {
            knowledge,
            engine,
            exposed: Vec::new(),
            audit: None,
            proposals: None,
            shows: None,
            boards: None,
            results: Results::default(),
        }
        .with_ceiling_over(exposed, AiAccess::Data)
    }

    /// Limits every connection to `ceiling` (the caller's: `ai.access`,
    /// `mcp.access`); the stricter of it and the database's own level applies.
    pub fn with_ceiling(self, ceiling: AiAccess) -> Self {
        let exposed = self.exposed.clone();
        self.with_ceiling_over(exposed, ceiling)
    }

    fn with_ceiling_over(mut self, exposed: Vec<Exposed>, ceiling: AiAccess) -> Self {
        self.exposed = exposed.into_iter().map(|e| e.limited(ceiling)).collect();
        if !self.exposed.iter().any(Exposed::reads_data) {
            self.engine = None;
        }
        self
    }

    /// Whether some connection allows tools that read rows (else they aren't
    /// offered).
    pub fn reads_data(&self) -> bool {
        self.engine.is_some()
    }

    /// Whether a tool that reads rows may run on connection `name` (as the tool
    /// resolves it): not on a schema-only one, with the reason.
    pub(crate) fn check_data(&self, name: Option<&str>) -> Result<(), ToolError> {
        let exposed = self.exposed_one(name)?;
        // Without a connection a data tool can't reach the server either way
        // (`Conn::session`, `Ctx::engine`).
        if exposed.access.is_data() {
            Ok(())
        } else {
            Err(ToolError::NotAllowed(format!(
                "{} is schema only: the AI sees no data from it. Write the query from the \
                 schema and open it for the user with propose_sql; they run it themselves",
                exposed.name
            )))
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

    /// Hands the board tools' calls to `sink`.
    pub fn with_boards(mut self, sink: BoardSink) -> Self {
        self.boards = Some(sink);
        self
    }

    /// Asks the app about boards.
    pub(crate) async fn boards(&self, request: BoardRequest) -> Result<BoardReply, ToolError> {
        let sink = self
            .boards
            .as_ref()
            .ok_or_else(|| ToolError::Failed("boards aren't available here".into()))?;
        let (call, answer) = BoardCall::new(request);
        sink(call);
        answer
            .await
            .map_err(|_| ToolError::Failed("tangul didn't answer".into()))?
            .map_err(ToolError::Failed)
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
            access: exposed.access,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn exposed(name: &str, access: AiAccess) -> Exposed {
        Exposed {
            name: name.into(),
            user: name.into(),
            conn: Some(ConnId(1)),
            driver: DriverKind::Postgres,
            access,
            values: ValueAccess::Values,
            bookmarks: vec![SharedBookmark {
                name: "Ann".into(),
                sql: "SELECT * FROM users WHERE email = 'ann@example.com' -- Ann's".into(),
            }],
        }
    }

    #[test]
    fn a_schema_only_database_keeps_nothing_to_read_with() {
        let ctx = Ctx::new(
            Knowledge::default(),
            None,
            vec![
                exposed("app", AiAccess::Data),
                exposed("hr", AiAccess::Schema),
            ],
        );
        let [app, hr] = ctx.exposed() else {
            panic!("two connections")
        };
        assert!(app.reads_data() && app.values.values());
        assert_eq!(
            app.bookmarks[0].sql,
            exposed("x", AiAccess::Data).bookmarks[0].sql
        );
        assert!(!hr.reads_data());
        assert_eq!((hr.conn, hr.values), (None, ValueAccess::None));
        assert_eq!(hr.bookmarks[0].sql, "select * from users where email = ?");
        // Data tools: per connection.
        assert_eq!(ctx.check_data(Some("app")), Ok(()));
        assert!(matches!(
            ctx.check_data(Some("hr")),
            Err(ToolError::NotAllowed(m)) if m.starts_with("hr is schema only")
        ));
        // A caller's ceiling limits every one.
        let ctx = ctx.with_ceiling(AiAccess::Schema);
        assert!(ctx.exposed().iter().all(|e| !e.reads_data()));
        assert!(ctx.check_data(Some("app")).is_err());
    }
}
