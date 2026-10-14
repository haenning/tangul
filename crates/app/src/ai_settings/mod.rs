//! The AI settings tab: two pages, the assistant (provider, key, model) and the
//! MCP server. What lives in `settings.json` it writes itself; what the workspace
//! owns (API keys in the Keychain, the server, the databases' AI settings) it asks
//! for with an [`AiSettingsEvent`], and the workspace hands it back as [`AiState`].
//! It knows nothing about the workspace.

mod assistant_page;
mod layout;
mod mcp_page;

use std::collections::{HashMap, HashSet};

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    input::{InputEvent, InputState},
    tab::{Tab, TabBar},
    v_flex,
};
use gpui_kit::*;
use tangul_core::llm::ProviderKind;
use tangul_core::{AiAccess, TabId, ValueAccess};
use tangul_data::local::AuditEntry;

use crate::settings;

/// The pages of the tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// The in-app assistant: its provider, key and model.
    Assistant,
    Mcp,
}

impl Page {
    const ALL: [Page; 2] = [Page::Assistant, Page::Mcp];

    fn label(self) -> &'static str {
        match self {
            Page::Assistant => "AI",
            Page::Mcp => "MCP",
        }
    }
}

/// The MCP server as it is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct McpState {
    /// Where it listens, while it runs.
    pub url: Option<String>,
    /// The port it could not start on, and why.
    pub failed: Option<(u16, String)>,
    pub calls: u64,
    /// The databases it offers (their names for tools).
    pub offered: Vec<String>,
}

/// What the AI may do on a saved database (as in its connection settings).
#[derive(Debug, Clone, PartialEq)]
pub struct DatabaseAi {
    pub id: String,
    pub name: String,
    /// Its users as (id, name).
    pub users: Vec<(String, String)>,
    /// The user the AI connects as; `None`: the user selected in the app.
    pub ai_user: Option<String>,
    pub values: ValueAccess,
    /// Its own level (the settings' ceiling, `ai.access`, may limit it further).
    pub access: AiAccess,
    pub ai_tools: bool,
    /// What is wrong with its AI user (gone, can't connect, may write).
    pub warning: Option<String>,
}

/// What only the workspace knows, handed to the tab when it changes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AiState {
    pub mcp: McpState,
    pub databases: Vec<DatabaseAi>,
    /// Providers with an API key in the Keychain.
    pub keys: HashSet<ProviderKind>,
    /// The tool calls of the assistant and of MCP clients shown, as each page asks
    /// for them ([`AiSettingsTab::calls_view`]).
    pub assistant_calls: Calls,
    pub mcp_calls: Calls,
}

/// Tool calls of one caller from the audit log: those shown, newest first, and
/// how many there are.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Calls {
    pub entries: Vec<AuditEntry>,
    pub total: usize,
}

/// Which tool calls a page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallsView {
    /// The latest [`CALLS_COLLAPSED`], with "Show more".
    Collapsed,
    /// Page `n` (from 0) of [`CALLS_PAGE`] each.
    Page(usize),
}

impl CallsView {
    /// Where the calls shown start, and how many.
    pub fn range(self) -> (usize, usize) {
        match self {
            CallsView::Collapsed => (0, CALLS_COLLAPSED),
            CallsView::Page(n) => (n * CALLS_PAGE, CALLS_PAGE),
        }
    }
}

/// Tool calls shown before "Show more".
pub const CALLS_COLLAPSED: usize = 5;
/// Tool calls per page after it.
pub const CALLS_PAGE: usize = 25;

/// The models a provider offers, as far as they were asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Models {
    Loading,
    Loaded(Vec<String>),
    Failed(String),
}

/// What the tab asks the workspace to do.
#[derive(Debug, Clone, PartialEq)]
pub enum AiSettingsEvent {
    /// Keep an API key in the Keychain.
    SaveKey(ProviderKind, String),
    ForgetKey(ProviderKind),
    /// Ask the provider which models it offers.
    LoadModels(ProviderKind),
    CopyMcpCommand,
    CopyDesktopConfig,
    NewMcpToken,
    SetAiUser {
        database: String,
        user: Option<String>,
    },
    SetValues {
        database: String,
        values: ValueAccess,
    },
    SetAiTools {
        database: String,
        on: bool,
    },
    SetAccess {
        database: String,
        access: AiAccess,
    },
    /// Open the database's connection settings.
    EditDatabase(String),
    /// Another page of tool calls is shown (see [`AiSettingsTab::calls_view`]).
    CallsPage,
}

pub struct AiSettingsTab {
    pub id: TabId,
    pub title: SharedString,
    page: Page,
    state: AiState,
    models: HashMap<ProviderKind, Models>,
    /// The tool calls each page shows, by caller.
    calls: HashMap<&'static str, CallsView>,
    /// A new API key for the provider shown (never shows a saved one).
    key: Entity<InputState>,
    model: Entity<InputState>,
    base_url: Entity<InputState>,
    port: Entity<InputState>,
    /// The provider whose values the inputs hold.
    inputs_for: Option<ProviderKind>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AiSettingsEvent> for AiSettingsTab {}

impl AiSettingsTab {
    pub fn new(id: TabId, page: Page, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Paste an API key")
        });
        let model = cx.new(|cx| InputState::new(window, cx).placeholder("Model id"));
        let base_url = cx.new(|cx| InputState::new(window, cx));
        let port = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings::current(cx).mcp.port.to_string())
        });
        let subscriptions = vec![
            cx.subscribe_in(&key, window, |this, _, ev, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.save_key(window, cx);
                }
            }),
            cx.subscribe_in(&model, window, |this, _, ev, _, cx| {
                if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.save_provider_value("model", cx);
                }
            }),
            cx.subscribe_in(&base_url, window, |this, _, ev, _, cx| {
                if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.save_provider_value("base_url", cx);
                }
            }),
            cx.subscribe_in(&port, window, |this, _, ev, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.save_port(window, cx);
                }
            }),
            // Edited in settings.json or elsewhere: the inputs follow.
            cx.observe_global_in::<settings::Current>(window, |this, window, cx| {
                this.inputs_for = None;
                this.sync_inputs(window, cx);
            }),
        ];
        let mut this = Self {
            id,
            title: "AI settings".into(),
            page,
            state: AiState::default(),
            models: HashMap::new(),
            calls: HashMap::new(),
            key,
            model,
            base_url,
            port,
            inputs_for: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        this.sync_inputs(window, cx);
        this
    }

    #[cfg(test)]
    pub fn page(&self) -> Page {
        self.page
    }

    pub fn show(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        cx.notify();
    }

    pub fn set_state(&mut self, state: AiState, cx: &mut Context<Self>) {
        if self.state != state {
            self.state = state;
            cx.notify();
        }
    }

    #[cfg(test)]
    pub fn state(&self) -> &AiState {
        &self.state
    }

    /// Which tool calls the page of `caller` (`assistant`, `mcp`) shows.
    pub fn calls_view(&self, caller: &str) -> CallsView {
        self.calls
            .get(caller)
            .copied()
            .unwrap_or(CallsView::Collapsed)
    }

    fn show_calls(&mut self, caller: &'static str, view: CallsView, cx: &mut Context<Self>) {
        self.calls.insert(caller, view);
        cx.emit(AiSettingsEvent::CallsPage);
        cx.notify();
    }

    pub fn set_models(&mut self, provider: ProviderKind, models: Models, cx: &mut Context<Self>) {
        self.models.insert(provider, models);
        cx.notify();
    }

    pub fn focus(tab: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let handle = tab.read(cx).focus_handle.clone();
        window.focus(&handle, cx);
    }

    /// The provider chosen in the settings.
    fn provider(cx: &App) -> Option<ProviderKind> {
        settings::current(cx).ai.provider
    }

    /// Fills the inputs with the chosen provider's saved values.
    fn sync_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let provider = Self::provider(cx);
        let port = settings::current(cx).mcp.port.to_string();
        self.port.update(cx, |i, cx| {
            if i.value() != port {
                i.set_value(port, window, cx)
            }
        });
        if self.inputs_for == provider {
            return;
        }
        self.inputs_for = provider;
        let Some(provider) = provider else {
            return;
        };
        let ai = &settings::current(cx).ai;
        let model = ai.of(provider).model.unwrap_or_default();
        let base_url = ai.of(provider).base_url.unwrap_or_default();
        let placeholder = provider.default_base_url();
        self.model
            .update(cx, |i, cx| i.set_value(model, window, cx));
        self.base_url.update(cx, |i, cx| {
            i.set_value(base_url, window, cx);
            i.set_placeholder(placeholder, window, cx);
        });
        self.key.update(cx, |i, cx| i.set_value("", window, cx));
    }

    fn set_provider(
        &mut self,
        provider: ProviderKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = settings::set_value(&["ai", "provider"], provider.key().into(), cx);
        self.report(result, window, cx);
        self.sync_inputs(window, cx);
        // Ask right away what it offers, when it can answer.
        if !provider.needs_key() || self.state.keys.contains(&provider) {
            self.load_models(provider, cx);
        }
    }

    fn load_models(&mut self, provider: ProviderKind, cx: &mut Context<Self>) {
        self.models.insert(provider, Models::Loading);
        cx.emit(AiSettingsEvent::LoadModels(provider));
        cx.notify();
    }

    fn save_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(provider), key) = (
            Self::provider(cx),
            self.key.read(cx).value().trim().to_string(),
        ) else {
            return;
        };
        if key.is_empty() {
            return;
        }
        self.key.update(cx, |i, cx| i.set_value("", window, cx));
        cx.emit(AiSettingsEvent::SaveKey(provider, key));
        self.load_models(provider, cx);
    }

    /// Writes the model or base URL input of the chosen provider (blank: unset).
    fn save_provider_value(&mut self, key: &'static str, cx: &mut Context<Self>) {
        let Some(provider) = Self::provider(cx) else {
            return;
        };
        let input = if key == "model" {
            &self.model
        } else {
            &self.base_url
        };
        let value = input.read(cx).value().trim().to_string();
        let saved = settings::current(cx).ai.of(provider);
        let current = if key == "model" {
            saved.model
        } else {
            saved.base_url
        };
        if current.unwrap_or_default() == value {
            return;
        }
        let json = if value.is_empty() {
            serde_json::Value::Null
        } else {
            value.into()
        };
        if let Err(e) = settings::set_value(&["ai", "providers", provider.key(), key], json, cx) {
            eprintln!("could not save the AI settings: {e}");
        }
        if key == "base_url" {
            self.models.remove(&provider);
        }
    }

    fn save_port(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.port.read(cx).value().trim().to_string();
        let current = settings::current(cx).mcp.port;
        match text.parse::<u16>() {
            Ok(port) if port != current => {
                let result = settings::set_value(&["mcp", "port"], port.into(), cx);
                self.report(result, window, cx);
            }
            Ok(_) => {}
            Err(_) => self
                .port
                .update(cx, |i, cx| i.set_value(current.to_string(), window, cx)),
        }
    }

    fn set_model(&mut self, model: String, window: &mut Window, cx: &mut Context<Self>) {
        self.model
            .update(cx, |i, cx| i.set_value(model, window, cx));
        self.save_provider_value("model", cx);
    }

    /// Shows why writing the settings failed.
    fn report(&self, result: Result<(), String>, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(e) = result {
            use gpui_kit::component::{WindowExt as _, notification::Notification};
            window.push_notification(Notification::error(e), cx);
        }
    }
}

impl Render for AiSettingsTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = match self.page {
            Page::Assistant => self.render_assistant(window, cx).into_any_element(),
            Page::Mcp => self.render_mcp(window, cx).into_any_element(),
        };
        let selected = Page::ALL.iter().position(|p| *p == self.page).unwrap_or(0);
        v_flex()
            .id("ai-settings")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .child(
                TabBar::new("ai-settings-pages")
                    .underline()
                    .small()
                    .px_4()
                    .children(Page::ALL.map(|p| Tab::new().label(p.label())))
                    .selected_index(selected)
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.show(Page::ALL[*ix], cx);
                    })),
            )
            .child(
                div()
                    .id("ai-settings-body")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(v_flex().max_w(px(720.)).p_6().gap_6().child(page)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{CALLS_COLLAPSED, CALLS_PAGE, CallsView};

    #[test]
    fn calls_are_shown_a_few_then_by_page() {
        assert_eq!(CallsView::Collapsed.range(), (0, CALLS_COLLAPSED));
        assert_eq!(CallsView::Page(0).range(), (0, CALLS_PAGE));
        assert_eq!(CallsView::Page(2).range(), (2 * CALLS_PAGE, CALLS_PAGE));
    }
}
