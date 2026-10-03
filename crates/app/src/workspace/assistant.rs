//! The assistant's chat in the right sidebar: what it runs with (the provider and
//! model from the settings, a tool context like MCP clients get, as caller
//! `assistant`), where its conversations are kept (`local.db`) and what its
//! answers do in the workspace (SQL into a tab or the editor).

use std::cell::Cell;
use std::sync::atomic::AtomicU64;

use tangul_ai::chat::{ChatSettings, for_storage, title_for};
use tangul_ai::llm::{self, Message};

use tangul_assist::nl::is_question;

use super::ai::AiEvent;
use super::*;
use crate::ai_settings::Page;
use crate::assistant::{ChatPanel, ChatPanelEvent, ChatRun};
use crate::query_tab::QuestionCheck;

/// Conversations listed in the panel.
const CHATS_LISTED: usize = 50;

/// The assistant's side of the window: where its tool calls hand their events.
#[derive(Default)]
pub(super) struct Assistant {
    events: Option<(Task<()>, async_channel::Sender<AiEvent>)>,
    /// Whether a provider, model and key are set up for the chat; tabs only look
    /// for questions then.
    ready: Rc<Cell<bool>>,
    calls: Arc<AtomicU64>,
    /// Answers instead of the provider in the settings (tests).
    #[cfg(test)]
    pub(super) provider: Option<Arc<dyn llm::Provider>>,
}

impl Workspace {
    /// Asks the assistant `question`: shows its panel and answers in the chat shown
    /// (`new_chat`: in a new one).
    pub(super) fn ask_assistant(
        &mut self,
        question: String,
        new_chat: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_panel(Panel::Assistant, cx);
        let panel = self.panels.assistant.clone();
        if new_chat {
            panel.update(cx, |p, cx| p.new_chat(cx));
        }
        let run = match self.chat_run(cx) {
            Ok(run) => run,
            Err(why) => {
                panel.update(cx, |p, cx| p.cannot_start(&question, why, window, cx));
                return;
            }
        };
        let title = title_for(&question);
        let chat = match panel.read(cx).chat() {
            Some(chat) => chat,
            None => match self
                .data
                .local
                .create_chat(self.active_db.as_deref(), &title, now_ms())
            {
                Ok(chat) => chat,
                Err(e) => {
                    let why = format!("Could not keep the chat: {e:#}");
                    panel.update(cx, |p, cx| p.cannot_start(&question, why, window, cx));
                    return;
                }
            },
        };
        panel.update(cx, |p, cx| p.start(chat, title, question, run, cx));
        self.refresh_chats(cx);
    }

    /// Whether the chat is set up (provider, model, key).
    pub(super) fn assistant_ready(&self) -> bool {
        self.assistant.ready.get()
    }

    /// How a tab tells a question from SQL: surely not SQL for its connection's
    /// database (words of its schema count as SQL). Only while the chat is set up
    /// (provider, model, key) and the setting is on; otherwise everything runs.
    pub(super) fn question_check(&self) -> QuestionCheck {
        let (knowledge, ready) = (self.knowledge.clone(), self.assistant.ready.clone());
        Rc::new(move |text, conn, dialect, cx| {
            if !ready.get() || !settings::current(cx).ai.questions_in_editor {
                return false;
            }
            let schema = knowledge.for_conn(conn).map(|(schema, _)| schema);
            is_question(text, dialect, |word| {
                schema.as_ref().is_some_and(|s| s.knows_name(word))
            })
        })
    }

    /// The model and tools a question runs with, or why there are none.
    fn chat_run(&mut self, cx: &mut Context<Self>) -> Result<ChatRun, String> {
        let ai = settings::current(cx).ai.clone();
        let kind = ai
            .provider
            .ok_or("Choose a model provider in the AI settings first.")?;
        let model = ai
            .model(kind)
            .ok_or_else(|| format!("Choose a {} model in the AI settings first.", kind.label()))?;
        let provider = llm::provider(self.llm_config(kind, cx)).map_err(|e| match e {
            llm::Error::NoKey => format!("Add your {} API key in the AI settings.", kind.label()),
            e => format!("{}: {e}", kind.label()),
        });
        #[cfg(test)]
        let provider = self.assistant.provider.clone().map_or(provider, Ok);
        let provider = provider?;
        // From now on the databases are offered (and their AI connections open).
        self.ai.assistant_used = true;
        self.expose_to_ai();
        if self.assistant.events.is_none() {
            self.assistant.events = Some(self.ai_events(cx));
        }
        let events = self
            .assistant
            .events
            .as_ref()
            .map(|(_, tx)| tx.clone())
            .ok_or("no event channel")?;
        let ctx = self.ai_ctx_source("assistant", events, self.assistant.calls.clone())();
        Ok(ChatRun {
            provider,
            ctx,
            settings: ChatSettings {
                model,
                max_rounds: ai.max_tool_rounds(),
                focus: self.ai_focus(),
            },
        })
    }

    /// The name tools know the active database by, if it is offered.
    fn ai_focus(&self) -> Option<String> {
        let db = self.active_db.as_deref()?;
        self.ai_exposed()
            .into_iter()
            .find(|e| self.find_user(&e.user).is_some_and(|(d, _)| d.id == db))
            .map(|e| e.name)
    }

    /// Tells the panel whether it can answer, and which database it is about.
    pub(super) fn sync_assistant(&mut self, cx: &mut Context<Self>) {
        let ai = settings::current(cx).ai.clone();
        let ready = match ai.provider {
            Some(p) => ai.model(p).is_some() && (!p.needs_key() || self.ai_keys().contains(&p)),
            None => false,
        };
        self.assistant.ready.set(ready);
        self.panels
            .assistant
            .update(cx, |p, cx| p.set_ready(ready, cx));
    }

    fn refresh_chats(&mut self, cx: &mut Context<Self>) {
        let chats = self.data.local.chats(CHATS_LISTED);
        self.panels
            .assistant
            .update(cx, |p, cx| p.set_chats(chats, cx));
    }

    pub(super) fn on_chat_event(
        &mut self,
        panel: &Entity<ChatPanel>,
        event: &ChatPanelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.clone() {
            ChatPanelEvent::Ask(question) => self.ask_assistant(question, false, window, cx),
            ChatPanelEvent::Save { chat, message } => {
                let stored = serde_json::to_string(&for_storage(&message)).unwrap_or_default();
                if let Err(e) = self.data.local.add_chat_message(chat, &stored, now_ms()) {
                    eprintln!("could not keep the chat: {e:#}");
                }
            }
            ChatPanelEvent::Open(chat) => {
                let Some(summary) = self.data.local.chat(chat) else {
                    return;
                };
                let messages: Vec<Message> = self
                    .data
                    .local
                    .chat_messages(chat)
                    .iter()
                    .filter_map(|m| serde_json::from_str(m).ok())
                    .collect();
                panel.update(cx, |p, cx| p.load(summary, messages, cx));
            }
            ChatPanelEvent::Delete(chat) => {
                if let Err(e) = self.data.local.delete_chat(chat) {
                    window.push_notification(
                        Notification::error(format!("Could not delete the chat: {e:#}")),
                        cx,
                    );
                }
                self.refresh_chats(cx);
            }
            ChatPanelEvent::ListChats => self.refresh_chats(cx),
            ChatPanelEvent::OpenSql(sql) => self.open_sql("From the assistant", &sql, window, cx),
            ChatPanelEvent::AddToBoard(sql) => self.add_answer_to_board(&sql, window, cx),
            ChatPanelEvent::InsertSql(sql) => match self.tabs.active_query().cloned() {
                Some(tab) => tab.update(cx, |t, cx| t.insert_sql(&sql, window, cx)),
                None => self.open_sql("From the assistant", &sql, window, cx),
            },
            ChatPanelEvent::OpenSettings => self.open_ai_settings(Page::Assistant, window, cx),
        }
    }
}
