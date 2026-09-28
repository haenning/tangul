//! The MCP server (`tangul_ai::mcp`) while `mcp.enabled` is set: which databases it
//! offers, its token (Keychain), where its tool calls are recorded (the audit log)
//! and its button in the status bar.

use std::collections::HashSet;
use std::sync::atomic::AtomicU64;
use std::sync::{Mutex, PoisonError, RwLock};

use tangul_ai::mcp::{CtxSource, McpServer, new_token};
use tangul_ai::{AuditSink, Ctx, Exposed, ProposalSink, SqlProposal, ToolCall};
use tangul_data::local::AuditEntry;

use super::*;

/// Keychain account of the server's token.
const TOKEN_ACCOUNT: &str = "mcp-token";

/// Fields drop in order: the proposal task goes before the server, so the server
/// closing its end never wakes the task from the server's thread.
#[derive(Default)]
pub(super) struct Mcp {
    /// Opens the statements clients propose (`propose_sql`) in tabs.
    proposals: Option<Task<()>>,
    server: Option<McpServer>,
    /// The port it could not start on, and why.
    failed: Option<(u16, String)>,
    /// The databases offered now; the server reads it for every call.
    exposed: Arc<RwLock<Vec<Exposed>>>,
    calls: Arc<AtomicU64>,
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
        cx.notify();
    }

    /// Stops the server, and the task opening its proposals first.
    fn stop_mcp(&mut self) {
        self.mcp.proposals = None;
        self.mcp.server = None;
    }

    fn start_mcp(&mut self, port: u16, cx: &mut Context<Self>) {
        let token = self.mcp_token();
        let (knowledge, engine) = (self.knowledge.clone(), self.engine.clone());
        let (exposed, sink) = (self.mcp.exposed.clone(), self.mcp_audit());
        let proposals = self.mcp_proposals(cx);
        let source: CtxSource = Arc::new(move || {
            let exposed = exposed
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            Ctx::new(knowledge.clone(), Some(engine.clone()), exposed)
                .with_audit("mcp", sink.clone())
                .with_proposals(proposals.clone())
        });
        match McpServer::start(port, token, source) {
            Ok(server) => {
                self.mcp.server = Some(server);
                self.mcp.failed = None;
                self.expose_to_mcp();
            }
            Err(e) => self.mcp.failed = Some((port, e.to_string())),
        }
    }

    /// Where clients' proposals go: to this window, which opens each in a tab.
    pub(super) fn mcp_proposals(&mut self, cx: &mut Context<Self>) -> ProposalSink {
        let (tx, rx) = async_channel::unbounded::<SqlProposal>();
        self.mcp.proposals = Some(cx.spawn(async move |this, cx| {
            while let Ok(proposal) = rx.recv().await {
                // The workspace's window (there is one at a time).
                let opened = cx.update(|cx| {
                    let window = cx.windows().first().copied()?;
                    window
                        .update(cx, |_, window, cx| {
                            this.update(cx, |ws, cx| ws.open_proposal(proposal, window, cx))
                                .ok()
                        })
                        .ok()
                        .flatten()
                });
                if opened.is_none() {
                    break;
                }
            }
        }));
        Arc::new(move |proposal| {
            let _ = tx.try_send(proposal);
        })
    }

    /// Opens a proposal in a new tab, not run, on your connection to its database.
    pub(super) fn open_proposal(
        &mut self,
        proposal: SqlProposal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = format!("Proposed · {}", proposal.connection);
        self.open_sql(&title, &proposal.sql, window, cx);
        if let (Some(conn), Some(tab)) = (
            self.tab_conn(&proposal.user),
            self.tabs.active_query().cloned(),
        ) {
            tab.update(cx, |t, cx| t.set_conn(conn, cx));
        }
        cx.notify();
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
    fn new_mcp_token(&mut self, cx: &mut Context<Self>) {
        self.data.profiles.set_secret(TOKEN_ACCOUNT, new_token());
        if let Some(port) = self.mcp.server.as_ref().map(|s| s.addr().port()) {
            self.stop_mcp();
            self.start_mcp(port, cx);
        }
        cx.notify();
    }

    /// Writes each tool call to the audit log (its own handle on `local.db`, as the
    /// calls come from the server's thread).
    fn mcp_audit(&self) -> AuditSink {
        let local = Mutex::new(Local::open(self.data.profiles.dir()));
        let calls = self.mcp.calls.clone();
        Arc::new(move |call: ToolCall| {
            calls.fetch_add(1, Ordering::Relaxed);
            let entry = AuditEntry {
                at: call.at,
                caller: call.caller.into(),
                target: match &call.connection {
                    Some(c) => format!("tool:{} {c}", call.tool),
                    None => format!("tool:{}", call.tool),
                },
                sql: call.input,
                outcome: call.outcome.into(),
                error: call.error,
                duration_ms: Some(call.duration_ms),
                rows: None,
            };
            let local = local.lock().unwrap_or_else(PoisonError::into_inner);
            if let Err(e) = local.add_audit(&entry) {
                eprintln!("could not write the audit log: {e:#}");
            }
        })
    }

    /// Offers the connected databases that are available to AI tools, each on its
    /// assistant connection (the database's AI user), with what is known of it
    /// through its connected user (the active one first).
    pub(super) fn expose_to_mcp(&mut self) {
        if self.mcp.server.is_none() {
            return;
        }
        let mut users: Vec<String> = self.conns.keys().cloned().collect();
        users.sort_by_key(|id| (Some(id) != self.active_user.as_ref(), id.clone()));
        let (mut seen, mut names, mut exposed) = (HashSet::new(), HashSet::new(), Vec::new());
        for user_id in users {
            let Some((db, _)) = self.find_user(&user_id) else {
                continue;
            };
            if !db.ai_tools || !seen.insert(db.id.clone()) {
                continue;
            }
            let db = db.clone();
            let Some(conn) =
                self.connections
                    .connect_assistant(&self.engine, &mut self.data.profiles, &user_id)
            else {
                continue;
            };
            // Clients name a database by it, so no two may share one.
            let base = db.display_name();
            let name = (1..)
                .map(|n| match n {
                    1 => base.clone(),
                    n => format!("{base} ({n})"),
                })
                .find(|name| names.insert(name.clone()))
                .unwrap_or(base);
            exposed.push(Exposed {
                name,
                user: user_id,
                conn,
                driver: db.driver,
                values: db.ai_values,
            });
        }
        *self
            .mcp
            .exposed
            .write()
            .unwrap_or_else(PoisonError::into_inner) = exposed;
    }

    /// Where the server listens, if it runs.
    #[cfg(test)]
    pub(super) fn mcp_url(&self) -> Option<String> {
        self.mcp.server.as_ref().map(McpServer::url)
    }

    /// The names of the databases offered now.
    #[cfg(test)]
    pub(super) fn mcp_offered(&self) -> Vec<String> {
        let exposed = self.mcp.exposed.read().unwrap();
        exposed.iter().map(|e| e.name.clone()).collect()
    }

    /// The command that adds the server to Claude Code.
    fn mcp_command(&mut self) -> Option<String> {
        let url = self.mcp.server.as_ref()?.url();
        let token = self.mcp_token();
        Some(format!(
            "claude mcp add --transport http tangul {url} --header \"Authorization: Bearer {token}\""
        ))
    }

    /// The status bar's plug: whether MCP clients can use tangul, and its menu.
    pub(super) fn render_mcp_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let databases = self
            .mcp
            .exposed
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        let (color, tooltip) = match (&self.mcp.server, &self.mcp.failed) {
            (Some(server), _) => (
                theme.foreground,
                format!(
                    "MCP server on {} · {databases} database{} · {} tool calls",
                    server.url(),
                    if databases == 1 { "" } else { "s" },
                    self.mcp.calls.load(Ordering::Relaxed)
                ),
            ),
            (None, Some((port, e))) => (
                theme.danger,
                format!("The MCP server could not start on port {port}: {e}"),
            ),
            (None, None) => (
                theme.muted_foreground,
                "MCP server off: MCP clients (Claude Code, …) can't use tangul's databases"
                    .to_string(),
            ),
        };
        let (on, weak) = (self.mcp.server.is_some(), cx.entity().downgrade());
        let enabled = settings::current(cx).mcp.enabled;
        Button::new("mcp")
            .icon(Icon::new(Lucide::Plug).text_color(color))
            .ghost()
            .xsmall()
            .tooltip(tooltip)
            .dropdown_menu(move |menu, _, _| mcp_menu(menu, enabled, on, &weak))
    }
}

fn mcp_menu(
    menu: PopupMenu,
    enabled: bool,
    on: bool,
    workspace: &WeakEntity<Workspace>,
) -> PopupMenu {
    let ws = workspace.clone();
    let menu = menu.item(PopupMenuItem::new("MCP server").checked(enabled).on_click(
        move |_, window, cx| {
            if let Err(e) = settings::set_mcp_enabled(!enabled, cx) {
                window.push_notification(Notification::error(e), cx);
            }
            let _ = ws.update(cx, |ws, cx| ws.sync_mcp(cx));
        },
    ));
    if !on {
        return menu;
    }
    let (copy, renew) = (workspace.clone(), workspace.clone());
    menu.separator()
        .item(
            PopupMenuItem::new("Copy the command for Claude Code").on_click(
                move |_, window, cx| {
                    let command = copy.update(cx, |ws, _| ws.mcp_command()).ok().flatten();
                    if let Some(command) = command {
                        cx.write_to_clipboard(ClipboardItem::new_string(command));
                        window.push_notification(
                            Notification::info("Copied: run it in a terminal to add tangul"),
                            cx,
                        );
                    }
                },
            ),
        )
        .item(
            PopupMenuItem::new("New token (clients must be added again)").on_click(
                move |_, _, cx| {
                    let _ = renew.update(cx, |ws, cx| ws.new_mcp_token(cx));
                },
            ),
        )
}
