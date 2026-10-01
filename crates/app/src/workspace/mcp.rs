//! The MCP server (`tangul_ai::mcp`) while `mcp.enabled` is set: starting and
//! stopping it, its token (Keychain), what clients need to add it, and its icon in
//! the status bar. The databases it offers and where its calls go are shared with
//! the assistant (`ai.rs`); its settings are on the AI settings tab.

use std::sync::atomic::AtomicU64;

use tangul_ai::mcp::{McpServer, new_token};

use super::*;
use crate::ai_settings::{McpState, Page};

/// Keychain account of the server's token.
const TOKEN_ACCOUNT: &str = "mcp-token";

/// Fields drop in order: the event task goes before the server, so the server
/// closing its end never wakes the task from the server's thread.
#[derive(Default)]
pub(super) struct Mcp {
    /// Takes the server's events (see `AiEvent`) in the window.
    events: Option<Task<()>>,
    server: Option<McpServer>,
    /// The port it could not start on, and why.
    failed: Option<(u16, String)>,
    calls: Arc<AtomicU64>,
}

impl Mcp {
    pub(super) fn running(&self) -> bool {
        self.server.is_some()
    }
}

impl Workspace {
    /// Starts or stops the server as `mcp` in the settings says.
    pub(super) fn sync_mcp(&mut self, cx: &mut Context<Self>) {
        let want = settings::current(cx).mcp.clone();
        let running = self.mcp.server.as_ref().map(|s| s.addr().port());
        if !want.enabled {
            if running.is_none() && self.mcp.failed.is_none() {
                return;
            }
            self.stop_mcp();
            self.mcp.failed = None;
        } else if running == Some(want.port)
            || self
                .mcp
                .failed
                .as_ref()
                .is_some_and(|(p, _)| *p == want.port)
        {
            return;
        } else {
            self.stop_mcp();
            self.start_mcp(want.port, cx);
        }
        self.push_ai_settings(cx);
        cx.notify();
    }

    /// Stops the server, and the task taking its events first.
    fn stop_mcp(&mut self) {
        self.mcp.events = None;
        self.mcp.server = None;
    }

    fn start_mcp(&mut self, port: u16, cx: &mut Context<Self>) {
        let token = self.mcp_token();
        let (task, events) = self.ai_events(cx);
        self.mcp.events = Some(task);
        let source = self.ai_ctx_source("mcp", events, self.mcp.calls.clone());
        match McpServer::start(port, token, source) {
            Ok(server) => {
                self.mcp.server = Some(server);
                self.mcp.failed = None;
                self.expose_to_ai();
            }
            Err(e) => self.mcp.failed = Some((port, e.to_string())),
        }
    }

    /// The server's token; made (and saved) the first time.
    fn mcp_token(&mut self) -> String {
        if let Some(token) = self.data.profiles.password(TOKEN_ACCOUNT) {
            return token;
        }
        let token = new_token();
        self.data.profiles.set_secret(TOKEN_ACCOUNT, token.clone());
        token
    }

    /// A new token: clients set up with the old one are locked out.
    pub(super) fn new_mcp_token(&mut self, cx: &mut Context<Self>) {
        self.data.profiles.set_secret(TOKEN_ACCOUNT, new_token());
        if let Some(port) = self.mcp.server.as_ref().map(|s| s.addr().port()) {
            self.stop_mcp();
            self.start_mcp(port, cx);
        }
        cx.notify();
    }

    /// The server as the settings tab shows it.
    pub(super) fn mcp_state(&self) -> McpState {
        McpState {
            url: self.mcp.server.as_ref().map(McpServer::url),
            failed: self.mcp.failed.clone(),
            calls: self.mcp.calls.load(Ordering::Relaxed),
            offered: if self.mcp.server.is_some() {
                self.ai_exposed().into_iter().map(|e| e.name).collect()
            } else {
                Vec::new()
            },
        }
    }

    /// Where the server listens, if it runs.
    #[cfg(test)]
    pub(super) fn mcp_url(&self) -> Option<String> {
        self.mcp.server.as_ref().map(McpServer::url)
    }

    /// The command that adds the server to Claude Code.
    /// For every project (user scope), not only the directory it is run in.
    pub(super) fn mcp_command(&mut self) -> Option<String> {
        let url = self.mcp.server.as_ref()?.url();
        let token = self.mcp_token();
        Some(format!(
            "claude mcp add --scope user --transport http tangul {url} \
             --header \"Authorization: Bearer {token}\""
        ))
    }

    /// The entry for `mcpServers` in Claude Desktop's `claude_desktop_config.json`.
    /// Claude Desktop starts local servers as commands, so `mcp-remote` bridges to
    /// the HTTP server; the header comes from the environment (no space in the
    /// arguments).
    pub(super) fn mcp_desktop_config(&mut self) -> Option<String> {
        let url = self.mcp.server.as_ref()?.url();
        let token = self.mcp_token();
        let entry = serde_json::json!({
            "tangul": {
                "command": "npx",
                "args": ["-y", "mcp-remote", url, "--header", "Authorization:${TANGUL_AUTH}"],
                "env": { "TANGUL_AUTH": format!("Bearer {token}") }
            }
        });
        serde_json::to_string_pretty(&entry).ok()
    }

    /// The status bar's plug: green while MCP clients can use tangul, grey when
    /// not. It opens the MCP settings.
    pub(super) fn render_mcp_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let databases = self.ai_exposed().len();
        let (color, tooltip) = match (&self.mcp.server, &self.mcp.failed) {
            (Some(server), _) => (
                theme.success,
                format!(
                    "MCP server on {} · {databases} database{} · {} tool calls",
                    server.url(),
                    if databases == 1 { "" } else { "s" },
                    self.mcp.calls.load(Ordering::Relaxed)
                ),
            ),
            (None, Some((port, e))) => (
                theme.muted_foreground,
                format!("The MCP server could not start on port {port}: {e}"),
            ),
            (None, None) => (
                theme.muted_foreground,
                "MCP server off: MCP clients (Claude Code, …) can't use tangul's databases"
                    .to_string(),
            ),
        };
        Button::new("mcp")
            .icon(Icon::new(Lucide::Plug).text_color(color))
            .ghost()
            .xsmall()
            .tooltip(tooltip)
            .on_click(
                cx.listener(|this, _, window, cx| this.open_ai_settings(Page::Mcp, window, cx)),
            )
    }
}
