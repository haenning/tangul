//! What a tool call may see and use: the one security layer below the registry.

use std::sync::Arc;
use tangul_assist::schema::SchemaIndex;
use tangul_assist::slice::KnownValues;
use tangul_assist::usage::Usage;
use tangul_core::{ConnId, DriverKind};
use tangul_engine::EngineHandle;
use tangul_service::knowledge::Knowledge;

use crate::tool::ToolError;

/// A connection the user enabled for the assistant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exposed {
    /// How tools and models name it, e.g. `app@localhost`.
    pub name: String,
    /// The database user id in the knowledge pool.
    pub user: String,
    pub driver: DriverKind,
    /// The assistant may read column values (samples, value profiles).
    pub values_allowed: bool,
}

/// The context of every tool call: the knowledge pool, the engine for queries, and
/// only the connections exposed to the assistant.
#[derive(Clone)]
pub struct Ctx {
    knowledge: Knowledge,
    engine: Option<EngineHandle>,
    exposed: Vec<Exposed>,
}

/// A connection resolved for a call.
pub(crate) struct Conn {
    pub conn: ConnId,
    pub schema: Arc<SchemaIndex>,
    /// What the user's queries on this database used (learned joins, …).
    pub usage: Arc<Usage>,
    /// Sampled values of its columns; empty unless the user allows reading values.
    pub known_values: KnownValues,
    /// The database user in the knowledge pool, and whether values may be read.
    pub user: String,
    pub values_allowed: bool,
}

impl Ctx {
    /// `engine` is `None` where no query may run (tests of the knowledge tools).
    pub fn new(knowledge: Knowledge, engine: Option<EngineHandle>, exposed: Vec<Exposed>) -> Self {
        Self {
            knowledge,
            engine,
            exposed,
        }
    }

    pub fn exposed(&self) -> &[Exposed] {
        &self.exposed
    }

    pub(crate) fn knowledge(&self) -> &Knowledge {
        &self.knowledge
    }

    /// The exposed connection called `name`, or the only one when it is left out.
    pub(crate) fn conn(&self, name: Option<&str>) -> Result<Conn, ToolError> {
        let names = || {
            self.exposed
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let exposed = match name {
            Some(name) => self
                .exposed
                .iter()
                .find(|e| e.name.eq_ignore_ascii_case(name))
                .ok_or_else(|| {
                    ToolError::NotFound(format!("connection {name}; available: {}", names()))
                })?,
            None => match self.exposed.as_slice() {
                [only] => only,
                [] => {
                    return Err(ToolError::NotAllowed(
                        "no connection is enabled for the assistant".into(),
                    ));
                }
                _ => {
                    return Err(ToolError::BadInput(format!(
                        "several connections; name one of: {}",
                        names()
                    )));
                }
            },
        };
        let known = self
            .knowledge
            .user(&exposed.user)
            .ok_or_else(|| ToolError::Failed(format!("{} is not connected yet", exposed.name)))?;
        let known_values = if exposed.values_allowed {
            self.knowledge.known_values(&exposed.user)
        } else {
            KnownValues::new()
        };
        Ok(Conn {
            conn: known.conn,
            known_values,
            user: exposed.user.clone(),
            values_allowed: exposed.values_allowed,
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
