//! How the chat looks: the header (title, new chat, earlier chats, settings), the
//! messages (questions, answers with their SQL blocks, the tools called), and the
//! question being typed.

use std::collections::HashMap;

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::Textarea,
    menu::{DropdownMenu as _, PopupMenuItem},
    spinner::Spinner,
    text::TextView,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_ai::chat::{Segment, segments};
use tangul_ai::llm::{Message, Part, Role, ToolCall};

use super::{ChatPanel, ChatPanelEvent};
use crate::assets::Lucide;

/// Most characters of a tool's input shown on its line.
const INPUT_CHARS: usize = 60;
/// The element key of the answer being written (finished ones count from 0).
const STREAMING: usize = 1 << 40;

impl Render for ChatPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("assistant")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .child(self.render_header(cx))
            .child(self.render_messages(cx))
            .child(self.render_input(cx))
    }
}

impl ChatPanel {
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let chats = self.chats.clone();
        let weak = cx.entity().downgrade();
        h_flex()
            .h_8()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .child(self.title.clone().unwrap_or_else(|| "Assistant".into())),
            )
            .child(
                Button::new("assistant-chats")
                    .icon(Icon::new(Lucide::RotateCcwClock))
                    .ghost()
                    .xsmall()
                    .tooltip("Earlier chats")
                    .dropdown_menu(move |mut menu, _, _| {
                        if chats.is_empty() {
                            return menu.label("No chats yet");
                        }
                        for chat in &chats {
                            let (weak, id) = (weak.clone(), chat.id);
                            menu = menu.item(PopupMenuItem::new(chat.title.clone()).on_click(
                                move |_, _, cx| {
                                    let _ =
                                        weak.update(cx, |_, cx| cx.emit(ChatPanelEvent::Open(id)));
                                },
                            ));
                        }
                        menu
                    }),
            )
            .when_some(self.chat, |this, id| {
                this.child(
                    Button::new("assistant-delete")
                        .icon(IconName::Delete)
                        .ghost()
                        .xsmall()
                        .tooltip("Delete this chat")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.new_chat(cx);
                            cx.emit(ChatPanelEvent::Delete(id));
                        })),
                )
            })
            .child(
                Button::new("assistant-new")
                    .icon(IconName::Plus)
                    .ghost()
                    .xsmall()
                    .tooltip("New chat")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.new_chat(cx);
                        this.focus_input(window, cx);
                    })),
            )
            .child(
                Button::new("assistant-settings")
                    .icon(IconName::Settings)
                    .ghost()
                    .xsmall()
                    .tooltip("AI settings")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatPanelEvent::OpenSettings))),
            )
    }

    fn render_messages(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // Whether each tool call worked, from the results sent back.
        let failed: HashMap<&str, bool> = self
            .messages
            .iter()
            .flat_map(|m| &m.parts)
            .filter_map(|p| match p {
                Part::ToolResult(r) => Some((r.call_id.as_str(), r.is_error)),
                _ => None,
            })
            .collect();
        let mut items: Vec<AnyElement> = Vec::new();
        for (ix, message) in self.messages.iter().enumerate() {
            match message.role {
                Role::User => {
                    let text = message.text();
                    if !text.trim().is_empty() {
                        items.push(question(ix, text, cx));
                    }
                }
                Role::Assistant => {
                    items.extend(self.answer(ix, message, &failed, cx));
                }
            }
        }
        if let Some(streaming) = &self.streaming {
            if !streaming.text.is_empty() {
                items.extend(self.prose(STREAMING, &streaming.text, cx));
            }
            items.push(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(Spinner::new().xsmall())
                    .child(match &streaming.tool {
                        Some(tool) => format!("Calling {tool}…"),
                        None => "Thinking…".into(),
                    })
                    .into_any_element(),
            );
        }
        if let Some(problem) = &self.problem {
            items.push(
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .id("assistant-problem")
                            .test_support()
                            .text_xs()
                            .text_color(theme.danger)
                            .whitespace_normal()
                            .child(problem.clone()),
                    )
                    .when(self.offer_sql, |this| {
                        this.child(
                            h_flex().child(
                                Button::new("assistant-run-as-sql")
                                    .label("Run as SQL anyway")
                                    .small()
                                    .outline()
                                    .on_click(
                                        cx.listener(|_, _, _, cx| {
                                            cx.emit(ChatPanelEvent::RunAsSql)
                                        }),
                                    ),
                            ),
                        )
                    })
                    .into_any_element(),
            );
        }
        // Until a provider is set up, the way to do it stays in view.
        if items.is_empty() || !self.ready {
            items.push(self.empty_state(cx));
        }
        div()
            .id("assistant-messages")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(v_flex().p_3().gap_3().children(items))
    }

    /// Before the first question: what to do.
    fn empty_state(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let text = if self.ready {
            "Ask about your data, have SQL written, errors explained or queries reviewed. \
             It reads only; changes open in a tab for you to run."
        } else {
            "Choose a model provider to use the assistant: your own API key, or a local \
             model with Ollama."
        };
        v_flex()
            .gap_2()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().whitespace_normal().child(text))
            .when(!self.ready, |this| {
                this.child(
                    h_flex().child(
                        Button::new("assistant-set-up")
                            .label("AI settings")
                            .small()
                            .primary()
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(ChatPanelEvent::OpenSettings)),
                            ),
                    ),
                )
            })
            .into_any_element()
    }

    /// An answer: its prose, SQL blocks and the tools it called.
    fn answer(
        &self,
        ix: usize,
        message: &Message,
        failed: &HashMap<&str, bool>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut out = Vec::new();
        for (part_ix, part) in message.parts.iter().enumerate() {
            match part {
                Part::Text { text } => out.extend(self.prose(ix * 1000 + part_ix, text, cx)),
                Part::ToolCall(call) => {
                    let ok = !failed.get(call.id.as_str()).copied().unwrap_or(false);
                    out.push(tool_line(call, ok, cx));
                }
                Part::ToolResult(_) => {}
            }
        }
        out
    }

    /// Markdown, with each SQL block as a block of its own with its actions.
    fn prose(&self, key: usize, text: &str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        segments(text)
            .into_iter()
            .enumerate()
            .map(|(i, segment)| match segment {
                Segment::Text(t) => TextView::markdown(("answer", key * 100 + i), t)
                    .selectable(true)
                    .text_sm()
                    .into_any_element(),
                Segment::Sql(sql) => sql_block(key * 100 + i, sql, cx),
            })
            .collect()
    }

    fn render_input(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let running = self.running.is_some();
        v_flex()
            .p_2()
            .gap_1()
            .border_t_1()
            .border_color(theme.border)
            .child(Textarea::new(&self.input))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(match &self.focus {
                                Some(db) => format!("About {db}"),
                                None => "No database available to AI tools".into(),
                            }),
                    )
                    .map(|this| {
                        if running {
                            this.child(
                                Button::new("assistant-stop")
                                    .label("Stop")
                                    .xsmall()
                                    .outline()
                                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                            )
                        } else {
                            this.child(
                                Button::new("assistant-send")
                                    .label("Send")
                                    .xsmall()
                                    .primary()
                                    .disabled(!self.ready)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.submit(window, cx)),
                                    ),
                            )
                        }
                    }),
            )
    }
}

fn question(ix: usize, text: String, cx: &App) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .justify_end()
        .child(
            div()
                .id(("question", ix))
                .max_w(relative(0.85))
                .px_3()
                .py_2()
                .rounded_lg()
                .bg(theme.secondary)
                .text_sm()
                .whitespace_normal()
                .child(text),
        )
        .into_any_element()
}

/// A block of SQL from an answer, with what the user can do with it.
fn sql_block(key: usize, sql: String, cx: &mut Context<ChatPanel>) -> AnyElement {
    let theme = cx.theme();
    let (open, insert, copy) = (sql.clone(), sql.clone(), sql.clone());
    v_flex()
        .id(("sql-block", key))
        .rounded_md()
        .border_1()
        .border_color(theme.border)
        .bg(theme.muted)
        .child(
            div()
                .px_2()
                .py_1p5()
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .whitespace_normal()
                .child(sql),
        )
        .child(
            h_flex()
                .px_1()
                .py_0p5()
                .gap_1()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    Button::new(("sql-open", key))
                        .label("Open in new tab")
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(ChatPanelEvent::OpenSql(open.clone()))
                        })),
                )
                .child(
                    Button::new(("sql-insert", key))
                        .label("Insert into editor")
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(ChatPanelEvent::InsertSql(insert.clone()))
                        })),
                )
                .child(
                    Button::new(("sql-copy", key))
                        .icon(IconName::Copy)
                        .ghost()
                        .xsmall()
                        .tooltip("Copy")
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                        }),
                ),
        )
        .into_any_element()
}

/// A tool the assistant called, with its input in short.
fn tool_line(call: &ToolCall, ok: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let input = match &call.input {
        serde_json::Value::Object(map) if map.is_empty() => String::new(),
        serde_json::Value::Object(map) => map
            .values()
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    };
    let input = input.split_whitespace().collect::<Vec<_>>().join(" ");
    let input: String = if input.chars().count() > INPUT_CHARS {
        format!("{}…", input.chars().take(INPUT_CHARS).collect::<String>())
    } else {
        input
    };
    h_flex()
        .gap_1p5()
        .items_center()
        .text_xs()
        .text_color(theme.muted_foreground)
        .child(
            Icon::new(if ok { IconName::Check } else { IconName::Close })
                .xsmall()
                .text_color(if ok {
                    theme.muted_foreground
                } else {
                    theme.danger
                }),
        )
        .child(call.name.clone())
        .when(!input.is_empty(), |this| {
            this.child(div().min_w_0().truncate().child(input))
        })
        .into_any_element()
}
