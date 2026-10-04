//! The assistant's chat, shown in the right sidebar: the conversation, the answer
//! as it streams in, and the question being typed. It runs a conversation it is
//! handed ([`ChatRun`]: the model, the tools' context) with `tangul_ai::chat`, and
//! asks for everything else with a [`ChatPanelEvent`]: storing messages, opening
//! SQL in the editor, settings. It knows nothing about the workspace.

mod view;

use std::sync::Arc;

use gpui_kit::component::input::{InputEvent, TextareaState};
use gpui_kit::*;
use tangul_ai::Ctx;
use tangul_ai::Registry;
use tangul_ai::chat::{ChatError, ChatEvent, ChatSettings, answer};
use tangul_ai::llm::{Message, Part, Provider, Role};
use tangul_data::local::ChatSummary;

/// What a conversation needs to run, from the workspace.
pub struct ChatRun {
    pub provider: Arc<dyn Provider>,
    pub ctx: Ctx,
    pub settings: ChatSettings,
}

/// What the panel asks the workspace to do.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatPanelEvent {
    /// The user asks `question`: the workspace answers with [`ChatPanel::start`]
    /// (or [`ChatPanel::cannot_start`]).
    Ask(String),
    /// Keep `message` in conversation `chat`.
    Save {
        chat: i64,
        message: Message,
    },
    /// Show conversation `chat` (from the list).
    Open(i64),
    Delete(i64),
    /// Show the list of conversations (refresh it).
    ListChats,
    /// SQL from an answer: into a new tab, or into the editor at the cursor.
    OpenSql(String),
    InsertSql(String),
    /// SQL from an answer onto a board, as a tile or a link.
    AddToBoard(String),
    OpenSettings,
}

/// The answer being written.
#[derive(Default)]
struct Streaming {
    /// Its text so far (since the last finished message).
    text: String,
    /// The tool the model is calling now.
    tool: Option<String>,
}

pub struct ChatPanel {
    /// The conversation shown, once it is stored.
    chat: Option<i64>,
    pub title: Option<String>,
    messages: Vec<Message>,
    streaming: Option<Streaming>,
    /// Why the last answer stopped early, or why it can't start.
    problem: Option<String>,
    /// Whether a provider is set up, so the panel can offer a question.
    ready: bool,
    chats: Vec<ChatSummary>,
    input: Entity<TextareaState>,
    scroll: ScrollHandle,
    /// The answer running; dropping it stops it.
    running: Option<Task<()>>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ChatPanelEvent> for ChatPanel {}

impl ChatPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 10)
                .submit_on_enter(true)
                .placeholder("Ask about your data")
        });
        let submit = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { shift: false, .. } = ev {
                this.submit(window, cx);
            }
        });
        Self {
            chat: None,
            title: None,
            messages: Vec::new(),
            streaming: None,
            problem: None,
            ready: false,
            chats: Vec::new(),
            input,
            scroll: ScrollHandle::new(),
            running: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: vec![submit],
        }
    }

    pub fn focus_input(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |i, cx| i.focus(window, cx));
    }

    #[cfg(test)]
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub fn chat(&self) -> Option<i64> {
        self.chat
    }

    #[cfg(test)]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Whether a provider is set up.
    pub fn set_ready(&mut self, ready: bool, cx: &mut Context<Self>) {
        if self.ready != ready {
            self.ready = ready;
            cx.notify();
        }
    }

    pub fn set_chats(&mut self, chats: Vec<ChatSummary>, cx: &mut Context<Self>) {
        self.chats = chats;
        cx.notify();
    }

    /// Starts over with an empty conversation (stops a running answer).
    pub fn new_chat(&mut self, cx: &mut Context<Self>) {
        self.running = None;
        self.chat = None;
        self.title = None;
        self.messages.clear();
        self.streaming = None;
        self.problem = None;
        cx.notify();
    }

    /// Shows a stored conversation.
    pub fn load(&mut self, chat: ChatSummary, messages: Vec<Message>, cx: &mut Context<Self>) {
        self.new_chat(cx);
        self.chat = Some(chat.id);
        self.title = Some(chat.title);
        self.messages = messages;
        self.scroll.scroll_to_bottom();
    }

    /// Puts `text` in the input, as if typed.
    pub fn set_question(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |i, cx| i.set_value(text.to_string(), window, cx));
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        if text.is_empty() || self.running.is_some() {
            return;
        }
        self.input.update(cx, |i, cx| i.set_value("", window, cx));
        cx.emit(ChatPanelEvent::Ask(text));
    }

    /// Stops the answer being written; what came so far stays.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if self.running.take().is_some() {
            if let Some(s) = self.streaming.take().filter(|s| !s.text.is_empty()) {
                self.messages
                    .push(Message::assistant(vec![Part::Text { text: s.text }]));
            }
            self.problem = Some("Stopped.".into());
            cx.notify();
        }
    }

    /// The question can't be answered: `why`, and the question stays in the input.
    pub fn cannot_start(
        &mut self,
        question: &str,
        why: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.problem = Some(why);
        self.set_question(question, window, cx);
        cx.notify();
    }

    /// Answers `question` in conversation `chat` (new, or the one shown).
    pub fn start(
        &mut self,
        chat: i64,
        title: String,
        question: String,
        run: ChatRun,
        cx: &mut Context<Self>,
    ) {
        if self.chat != Some(chat) {
            self.new_chat(cx);
            self.chat = Some(chat);
        }
        self.title.get_or_insert(title);
        self.problem = None;
        let asked = Message::user(question);
        self.messages.push(asked.clone());
        cx.emit(ChatPanelEvent::Save {
            chat,
            message: asked,
        });
        self.streaming = Some(Streaming::default());
        self.scroll.scroll_to_bottom();

        let (tx, rx) = async_channel::unbounded::<Result<ChatEvent, ChatError>>();
        let history = self.messages.clone();
        let job = async move {
            let registry = Registry::standard();
            let done = answer(
                run.provider.as_ref(),
                &registry,
                &run.ctx,
                &run.settings,
                &history,
                &mut |e| {
                    let _ = tx.try_send(Ok(e));
                },
            )
            .await;
            if let Err(e) = done {
                let _ = tx.try_send(Err(e));
            }
        };
        // On the assistant's runtime (HTTP needs tokio). Tests keep it on their own
        // executor, which allows no wake-ups from other threads.
        #[cfg(not(test))]
        let work = tangul_ai::runtime::start(job);
        #[cfg(test)]
        let work = cx.background_spawn(job);
        self.running = Some(cx.spawn(async move |this, cx| {
            // Dropping the task (stop, new chat) stops the work too.
            let _work = work;
            while let Ok(event) = rx.recv().await {
                if this
                    .update(cx, |this, cx| this.on_chat_event(chat, event, cx))
                    .is_err()
                {
                    return;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.running = None;
                this.streaming = None;
                cx.emit(ChatPanelEvent::ListChats);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn on_chat_event(
        &mut self,
        chat: i64,
        event: Result<ChatEvent, ChatError>,
        cx: &mut Context<Self>,
    ) {
        let streaming = self.streaming.get_or_insert_default();
        match event {
            Ok(ChatEvent::Text(t)) => {
                streaming.text.push_str(&t);
                streaming.tool = None;
            }
            Ok(ChatEvent::ToolStarted(name)) => streaming.tool = Some(name),
            Ok(ChatEvent::ToolRan { .. }) => streaming.tool = None,
            Ok(ChatEvent::Message(message)) => {
                if message.role == Role::Assistant {
                    streaming.text.clear();
                }
                self.messages.push(message.clone());
                cx.emit(ChatPanelEvent::Save { chat, message });
            }
            Err(e) => self.problem = Some(e.to_string()),
        }
        self.scroll.scroll_to_bottom();
        cx.notify();
    }
}

impl Focusable for ChatPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
