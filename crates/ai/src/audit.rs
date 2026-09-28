//! A record of every tool call, for the audit log. Whoever runs the tools (the MCP
//! server, the assistant panel) gives a sink that writes it where it belongs; the
//! engine records the SQL itself.

use std::sync::Arc;

/// Longest input kept; longer ones are cut.
const INPUT_CHARS: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    /// Unix milliseconds.
    pub at: i64,
    /// Who called: `mcp`, `assistant`.
    pub caller: &'static str,
    pub tool: String,
    /// The connection it was about, when it named one or there is only one.
    pub connection: Option<String>,
    /// The input as JSON, cut to 2000 characters.
    pub input: String,
    /// The whole statement, when the tool ran one on the database (`run_readonly`).
    pub sql: Option<String>,
    /// Rows it returned, when it ran one.
    pub rows: Option<u64>,
    /// `ran`, `refused` (not allowed) or `failed`.
    pub outcome: &'static str,
    pub error: Option<String>,
    pub duration_ms: i64,
}

/// Where tool calls go.
pub type AuditSink = Arc<dyn Fn(ToolCall) + Send + Sync>;

pub(crate) fn clip(input: &str) -> String {
    match input.char_indices().nth(INPUT_CHARS) {
        Some((i, _)) => format!("{}…", &input[..i]),
        None => input.to_string(),
    }
}
