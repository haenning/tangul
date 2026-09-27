//! What a tool is, what it returns, and the registry that lists and calls them.

use std::fmt;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::ctx::Ctx;
use crate::tools;

/// A tool's answer: the data (structured) and how a model reads it (compact text).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub data: Value,
    pub text: String,
}

impl ToolOutput {
    pub(crate) fn new(data: &impl Serialize, text: impl Into<String>) -> Self {
        Self {
            data: serde_json::to_value(data).unwrap_or(Value::Null),
            text: text.into(),
        }
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

    pub async fn call(&self, ctx: &Ctx, name: &str, input: Value) -> Result<ToolOutput, ToolError> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.name() == name)
            .ok_or_else(|| ToolError::NotFound(format!("no tool named {name}")))?;
        tool.call(ctx, input).await
    }
}
