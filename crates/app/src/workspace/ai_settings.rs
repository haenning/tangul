//! The AI settings tab in the workspace: opening it on a page, handing it what
//! only the workspace knows ([`AiState`]) and doing what it asks (API keys in the
//! Keychain, the MCP server's token and client setup, the databases' AI settings).

use std::collections::HashSet;

use tangul_ai::llm::{self, ProviderKind};

use super::*;
use crate::ai_settings::{
    AiSettingsEvent, AiSettingsTab, AiState, Calls, CallsView, DatabaseAi, Models, Page,
};

impl Workspace {
    /// Opens the AI settings on `page` (or switches the open tab to it).
    pub(super) fn open_ai_settings(
        &mut self,
        page: Page,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self.tabs.items().position(|t| t.ai_settings().is_some());
        let tab = match open {
            Some(ix) => {
                self.tabs.set_active(ix);
                let tab = self.tabs.get(ix).and_then(TabItem::ai_settings).cloned();
                if let Some(tab) = &tab {
                    tab.update(cx, |t, cx| t.show(page, cx));
                }
                tab
            }
            None => {
                let id = TabId(self.next_id());
                let tab = cx.new(|cx| AiSettingsTab::new(id, page, window, cx));
                let events = cx.subscribe_in(&tab, window, Self::on_ai_settings_event);
                self.tabs
                    .push(TabItem::AiSettings(tab.clone()), Some(events));
                Some(tab)
            }
        };
        if let Some(tab) = tab {
            AiSettingsTab::focus(&tab, window, cx);
        }
        self.push_ai_settings(cx);
        cx.notify();
    }

    /// Hands the open settings tab what it shows from the workspace.
    pub(super) fn push_ai_settings(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.items().find_map(TabItem::ai_settings).cloned() else {
            return;
        };
        let views = {
            let tab = tab.read(cx);
            (tab.calls_view("assistant"), tab.calls_view("mcp"))
        };
        let state = self.ai_state(views);
        tab.update(cx, |t, cx| t.set_state(state, cx));
    }

    fn ai_state(&mut self, (assistant, mcp): (CallsView, CallsView)) -> AiState {
        let databases = self
            .data
            .profiles
            .databases
            .iter()
            .map(|db| {
                let missing = db
                    .assistant_user
                    .as_deref()
                    .filter(|id| db.user(id).is_none())
                    .map(|id| format!("Its AI user {id} no longer exists: choose another."));
                let writable = self
                    .knowledge
                    .ai_writable(&db.id)
                    .filter(|n| *n > 0)
                    .map(bars::ai_user_warning);
                let warning = missing
                    .or_else(|| self.ai_problems.get(&db.id).cloned())
                    .or(writable);
                DatabaseAi {
                    id: db.id.clone(),
                    name: db.display_name(),
                    users: db
                        .users
                        .iter()
                        .map(|u| (u.id.clone(), u.user.clone()))
                        .collect(),
                    ai_user: db.assistant_user.clone(),
                    values: db.ai_values,
                    access: db.ai_access,
                    ai_tools: db.ai_tools,
                    // Schema only: no AI user to warn about.
                    warning: warning.filter(|_| self.ai_access(db).is_data()),
                }
            })
            .collect();
        AiState {
            mcp: self.mcp_state(),
            databases,
            keys: self.ai_keys().clone(),
            assistant_calls: self.tool_calls("assistant", assistant),
            mcp_calls: self.tool_calls("mcp", mcp),
        }
    }

    /// The tool calls of `caller` a page shows.
    fn tool_calls(&self, caller: &str, view: CallsView) -> Calls {
        let (offset, limit) = view.range();
        Calls {
            entries: self.data.local.tool_calls(caller, offset, limit),
            total: self.data.local.tool_call_count(caller),
        }
    }

    /// The providers with an API key in the Keychain (read once).
    pub(super) fn ai_keys(&mut self) -> &HashSet<ProviderKind> {
        if self.ai_keys.is_none() {
            let keys = ProviderKind::ALL
                .into_iter()
                .filter(|p| {
                    p.needs_key() && self.data.profiles.password(&p.key_account()).is_some()
                })
                .collect();
            self.ai_keys = Some(keys);
        }
        self.ai_keys.get_or_insert_default()
    }

    /// How to reach provider `kind` as the settings say, with its key.
    pub(super) fn llm_config(&mut self, kind: ProviderKind, cx: &App) -> llm::Config {
        let key = kind
            .needs_key()
            .then(|| self.data.profiles.password(&kind.key_account()))
            .flatten();
        llm::Config {
            kind,
            base_url: settings::current(cx).ai.of(kind).base_url,
            key,
        }
    }

    fn on_ai_settings_event(
        &mut self,
        tab: &Entity<AiSettingsTab>,
        event: &AiSettingsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.clone() {
            AiSettingsEvent::SaveKey(p, key) => {
                self.data.profiles.set_secret(&p.key_account(), key);
                self.ai_keys = None;
            }
            AiSettingsEvent::ForgetKey(p) => {
                self.data.profiles.forget_secret(&p.key_account());
                self.ai_keys = None;
            }
            AiSettingsEvent::LoadModels(p) => self.load_models(p, tab.clone(), cx),
            AiSettingsEvent::CopyMcpCommand => {
                if let Some(command) = self.mcp_command() {
                    cx.write_to_clipboard(ClipboardItem::new_string(command));
                    window.push_notification(
                        Notification::info("Copied: run it in a terminal to add tangul"),
                        cx,
                    );
                }
            }
            AiSettingsEvent::CopyDesktopConfig => {
                if let Some(config) = self.mcp_desktop_config() {
                    cx.write_to_clipboard(ClipboardItem::new_string(config));
                    window.push_notification(
                        Notification::info(
                            "Copied: add it to \"mcpServers\" in Claude Desktop's \
                             claude_desktop_config.json, then restart Claude Desktop",
                        ),
                        cx,
                    );
                }
            }
            AiSettingsEvent::NewMcpToken => {
                self.new_mcp_token(cx);
                window.push_notification(
                    Notification::info("New token: add tangul to your MCP clients again"),
                    cx,
                );
            }
            AiSettingsEvent::SetAiUser { database, user } => {
                self.set_assistant_user(&database, user, window, cx)
            }
            AiSettingsEvent::SetValues { database, values } => {
                self.set_ai_values(&database, values, window, cx)
            }
            AiSettingsEvent::SetAiTools { database, on } => {
                self.set_ai_tools(&database, on, window, cx)
            }
            AiSettingsEvent::SetAccess { database, access } => {
                self.set_ai_access(&database, access, window, cx)
            }
            AiSettingsEvent::EditDatabase(database) => {
                self.open_form(FormMode::EditDatabase, Some(database), None, window, cx)
            }
            // Read below, with the rest.
            AiSettingsEvent::CallsPage => {}
        }
        self.push_ai_settings(cx);
        self.sync_assistant(cx);
    }

    /// Asks provider `p` for its models and hands them to `tab`.
    fn load_models(&mut self, p: ProviderKind, tab: Entity<AiSettingsTab>, cx: &mut Context<Self>) {
        let config = self.llm_config(p, cx);
        let work = tangul_ai::runtime::spawn(async move {
            match llm::provider(config) {
                Ok(provider) => provider.models().await,
                Err(e) => Err(e),
            }
        });
        cx.spawn(async move |_, cx| {
            let models = match work.await {
                Ok(Ok(list)) => Models::Loaded(list),
                Ok(Err(e)) => Models::Failed(e.to_string()),
                Err(e) => Models::Failed(e.to_string()),
            };
            tab.update(cx, |t, cx| t.set_models(p, models, cx));
        })
        .detach();
    }
}
