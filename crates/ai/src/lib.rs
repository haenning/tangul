//! The assistant's tools: typed operations over what tangul knows about the
//! databases (the knowledge pool, `tangul_service::knowledge`) and read-only queries
//! through the engine's gate. Each tool is written once, here; the MCP server and
//! the provider adapters map the [`Registry`] generically (name, description, input
//! JSON Schema, call).
//!
//! **One security layer for every caller:** a tool only sees what its [`Ctx`]
//! exposes: the connections the user enabled for the assistant and their opt-ins.
//! Every statement goes through the engine's gate as `Caller::Assistant` (read-only
//! transaction, rolled back, time limit, audit); a write is never run. Front ends
//! only add authentication of their caller.
//!
//! Outputs carry structured data (for tests and UI) and a compact text for models.

mod audit;
mod ctx;
pub mod mcp;
mod results;
mod tool;
mod tools;

pub use audit::{AuditSink, ToolCall};
pub use ctx::{Ctx, Exposed, ProposalSink, SqlProposal};
pub use results::Results;
pub use tool::{Registry, Tool, ToolError, ToolOutput, ToolSpec};
