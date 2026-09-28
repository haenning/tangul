//! What a tool is, what it returns, and the registry that lists and calls them.

use std::fmt;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::audit::{ToolCall, clip};
use crate::ctx::Ctx;
use crate::tools;

/// A tool's answer: the data (structured) and how a model reads it (compact text).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub data: Value,
    pub text: String,
    /// Rows the statement it ran returned (tools that run one).
    pub rows: Option<u64>,
}

impl ToolOutput {
    pub(crate) fn new(data: &impl Serialize, text: impl Into<String>) -> Self {
        Self {
            data: serde_json::to_value(data).unwrap_or(Value::Null),
            text: text.into(),
            rows: None,
        }
    }

    pub(crate) fn with_rows(mut self, rows: u64) -> Self {
        self.rows = Some(rows);
        self
    }
}

/// Why a call gave no answer. Its text goes to the model as the tool's result, so
/// it says what to do instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// The input doesn't fit the tool's schema.
    BadInput(String),
    /// A connection, table or tool of that name doesn't exist.
    NotFound(String),
    /// Not allowed for the assistant (a write, an opt-in that is off).
    NotAllowed(String),
    /// The database or engine failed.
    Failed(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolError::BadInput(m) => write!(f, "invalid input: {m}"),
            ToolError::NotFound(m) => write!(f, "not found: {m}"),
            ToolError::NotAllowed(m) => write!(f, "not allowed: {m}"),
            ToolError::Failed(m) => write!(f, "failed: {m}"),
        }
    }
}

impl std::error::Error for ToolError {}

/// A tool as front ends describe it to a model.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    /// For the model: what the tool does and when to use it.
    fn description(&self) -> &'static str;
    /// JSON Schema of the input (see [`schema_of`]).
    fn input_schema(&self) -> Value;
    /// Whether its input's `sql` is a statement it runs on the database (recorded
    /// with the call, for the database's history).
    fn runs_sql(&self) -> bool {
        false
    }
    async fn call(&self, ctx: &Ctx, input: Value) -> Result<ToolOutput, ToolError>;
}

/// The input schema of a tool, derived from its input type (doc comments become
/// the field descriptions).
pub(crate) fn schema_of<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or(Value::Null)
}

/// Reads a tool's input; `null` counts as `{}` (tools without required fields).
pub(crate) fn input<T: DeserializeOwned>(value: Value) -> Result<T, ToolError> {
    let value = if value.is_null() {
        Value::Object(Default::default())
    } else {
        value
    };
    serde_json::from_value(value).map_err(|e| ToolError::BadInput(e.to_string()))
}

/// Every tool, by name.
pub struct Registry {
    tools: Vec<Box<dyn Tool>>,
}

impl Registry {
    /// The tools tangul offers.
    pub fn standard() -> Self {
        Self {
            tools: tools::all(),
        }
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools
            .iter()
            .map(|t| ToolSpec {
                name: t.name(),
                description: t.description(),
                input_schema: t.input_schema(),
            })
            .collect()
    }

    /// Calls tool `name`, and records the call if the context has an audit sink.
    pub async fn call(&self, ctx: &Ctx, name: &str, input: Value) -> Result<ToolOutput, ToolError> {
        let started = Instant::now();
        let record = ctx.audit().map(|(caller, sink)| {
            let connection = input
                .get("connection")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| match ctx.exposed() {
                    [only] => Some(only.name.clone()),
                    _ => None,
                });
            (*caller, sink.clone(), connection, clip(&input.to_string()))
        });
        let tool = self.tools.iter().find(|t| t.name() == name);
        let sql = tool
            .filter(|t| t.runs_sql())
            .and_then(|_| input.get("sql"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let result = match tool {
            Some(tool) => tool.call(ctx, input).await,
            None => Err(ToolError::NotFound(format!("no tool named {name}"))),
        };
        if let Some((caller, sink, connection, input)) = record {
            let (outcome, error) = match &result {
                Ok(_) => ("ran", None),
                Err(e @ ToolError::NotAllowed(_)) => ("refused", Some(e.to_string())),
                Err(e) => ("failed", Some(e.to_string())),
            };
            sink(ToolCall {
                at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as i64),
                caller,
                tool: name.to_string(),
                connection,
                input,
                sql,
                rows: result.as_ref().ok().and_then(|out| out.rows),
                outcome,
                error,
                duration_ms: started.elapsed().as_millis() as i64,
            });
        }
        result
    }
}
