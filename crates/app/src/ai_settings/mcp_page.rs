//! The MCP page: the server, how clients add it, its token, and what it offers.

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, button::Button, input::Input, v_flex,
};
use gpui_kit::*;

use super::layout::{note, row, section};
use super::{AiSettingsEvent, AiSettingsTab};
use crate::settings;
use crate::ui::switch;

impl AiSettingsTab {
    pub(super) fn render_mcp(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (enabled, access) = {
            let mcp = &settings::current(cx).mcp;
            (mcp.enabled, mcp.access)
        };
        let mcp = self.state.mcp.clone();
        let status: SharedString = match (&mcp.url, &mcp.failed) {
            (Some(url), _) => format!(
                "Running on {url} · {} tool call{}",
                mcp.calls,
                if mcp.calls == 1 { "" } else { "s" }
            )
            .into(),
            (None, Some((port, e))) => format!("Could not start on port {port}: {e}").into(),
            (None, None) => "Off: MCP clients can't use tangul's databases.".into(),
        };
        let schema_only = self.schema_only_row("mcp-schema-only", "mcp", access, cx);
        let server = section(
            "MCP server",
            Some(
                "MCP clients (Claude Code, Claude Desktop, …) use tangul's AI tools on the \
                 databases below while the app runs: reads only, through the same checks as \
                 the assistant; changes open in a tab for you to run.",
            ),
            [
                row(
                    "MCP server",
                    Some(status),
                    switch("mcp-enabled", cx)
                        .checked(enabled)
                        .on_click(cx.listener(|this, on: &bool, window, cx| {
                            let result = settings::set_mcp_enabled(*on, cx);
                            this.report(result, window, cx);
                        })),
                    cx,
                ),
                row(
                    "Port",
                    Some("On 127.0.0.1 only. Clients must be added again after a change.".into()),
                    div().w(px(90.)).child(Input::new(&self.port).small()),
                    cx,
                ),
                schema_only,
            ],
            cx,
        );
        let running = mcp.url.is_some();
        let clients = section(
            "Clients",
            Some("The token is in the Keychain; the copied text contains it."),
            [
                row(
                    "Claude Code",
                    Some("A command to run once in a terminal.".into()),
                    Button::new("mcp-copy-command")
                        .label("Copy command")
                        .small()
                        .outline()
                        .disabled(!running)
                        .on_click(
                            cx.listener(|_, _, _, cx| cx.emit(AiSettingsEvent::CopyMcpCommand)),
                        ),
                    cx,
                ),
                row(
                    "Claude Desktop",
                    Some(
                        "An entry for \"mcpServers\" in claude_desktop_config.json (Settings → \
                         Developer → Edit Config); restart Claude Desktop after."
                            .into(),
                    ),
                    Button::new("mcp-copy-desktop")
                        .label("Copy config")
                        .small()
                        .outline()
                        .disabled(!running)
                        .on_click(
                            cx.listener(|_, _, _, cx| cx.emit(AiSettingsEvent::CopyDesktopConfig)),
                        ),
                    cx,
                ),
                row(
                    "Token",
                    Some("A new token locks out clients added with the old one.".into()),
                    Button::new("mcp-new-token")
                        .label("New token")
                        .small()
                        .outline()
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(AiSettingsEvent::NewMcpToken))),
                    cx,
                ),
            ],
            cx,
        );
        let offered = section(
            "Offered now",
            None,
            [if !running {
                note("Nothing while the server is off.", cx)
            } else if mcp.offered.is_empty() {
                note(
                    "No database: connect to one that is available to AI tools.",
                    cx,
                )
            } else {
                note(mcp.offered.join(", "), cx)
            }],
            cx,
        );
        v_flex()
            .gap_6()
            .text_color(cx.theme().foreground)
            .child(server)
            .child(clients)
            .child(offered)
            .child(self.render_databases(cx))
            .child(self.render_calls("mcp", cx))
    }
}
