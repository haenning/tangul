//! The AI page: the assistant's provider, its API key (Keychain), address and
//! model, and how it behaves.

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::Input,
    menu::{DropdownMenu as _, PopupMenuItem},
    switch::Switch,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_core::llm::ProviderKind;

use super::layout::{note, row, section};
use super::{AiSettingsEvent, AiSettingsTab, Models};
use crate::settings;

impl AiSettingsTab {
    pub(super) fn render_assistant(
        &mut self,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let chosen = Self::provider(cx);
        let providers = h_flex()
            .gap_1()
            .flex_wrap()
            .children(ProviderKind::ALL.map(|p| {
                let b = Button::new(SharedString::from(format!("ai-provider-{}", p.key())))
                    .label(p.label())
                    .small();
                let b = if chosen == Some(p) {
                    b.primary()
                } else {
                    b.outline()
                };
                b.on_click(cx.listener(move |this, _, window, cx| this.set_provider(p, window, cx)))
            }));
        let mut rows = vec![
            v_flex()
                .px_3()
                .py_2()
                .gap_2()
                .child(div().text_sm().child("Provider"))
                .child(providers)
                .into_any_element(),
        ];
        match chosen {
            None => rows.push(note(
                "Choose a provider to use the assistant. Your own API key, or a local model \
                 with Ollama (nothing leaves the machine).",
                cx,
            )),
            Some(p) => rows.extend(self.provider_rows(p, cx)),
        }
        let access = settings::current(cx).ai.access;
        let schema_only = self.schema_only_row("ai-schema-only", "ai", access, cx);
        let provider = section(
            "Assistant",
            Some(
                "The assistant answers in the right sidebar. It reads your databases through \
                 the same checks as MCP clients: reads only, rolled back, with a time limit; \
                 changes open in a tab for you to run. What it reads goes to the provider.",
            ),
            rows,
            cx,
        );
        let ai = settings::current(cx).ai.clone();
        let behavior = section(
            "Behavior",
            None,
            [
                row(
                    "Questions in the editor",
                    Some(
                        "Once the assistant is set up, running text that is surely a \
                         question, not SQL (comments aside), asks the assistant instead of \
                         the database."
                            .into(),
                    ),
                    Switch::new("ai-questions-in-editor")
                        .checked(ai.questions_in_editor)
                        .on_click(cx.listener(|this, on: &bool, window, cx| {
                            let result = settings::set_value(
                                &["ai", "questions_in_editor"],
                                (*on).into(),
                                cx,
                            );
                            this.report(result, window, cx);
                        })),
                    cx,
                ),
                row(
                    "Tool rounds per question",
                    Some(
                        format!(
                            "{}: then it stops and asks you to go on (ai.max_tool_rounds in \
                             settings.json).",
                            ai.max_tool_rounds()
                        )
                        .into(),
                    ),
                    div(),
                    cx,
                ),
                schema_only,
            ],
            cx,
        );
        v_flex()
            .gap_6()
            .text_color(cx.theme().foreground)
            .child(provider)
            .child(behavior)
            .child(self.render_databases(cx))
            .child(self.render_calls("assistant", cx))
    }

    /// Key, address and model of provider `p`.
    fn provider_rows(&self, p: ProviderKind, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        if p.needs_key() {
            let saved = self.state.keys.contains(&p);
            rows.push(row(
                "API key",
                Some(
                    if saved {
                        "Saved in the Keychain. Paste a new one to replace it."
                    } else {
                        "Kept in the Keychain, never in settings.json. Enter saves it."
                    }
                    .into(),
                ),
                h_flex()
                    .gap_1()
                    .child(
                        div()
                            .w(px(220.))
                            .child(Input::new(&self.key).small().mask_toggle()),
                    )
                    .when(saved, |this| {
                        this.child(
                            Button::new("ai-forget-key")
                                .icon(IconName::Delete)
                                .ghost()
                                .small()
                                .tooltip("Remove the key")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(AiSettingsEvent::ForgetKey(p))
                                })),
                        )
                    }),
                cx,
            ));
        }
        if p.custom_base_url() {
            rows.push(row(
                "Address",
                Some(
                    match p {
                        ProviderKind::Openai => {
                            "The API's base URL: OpenAI's when empty; or OpenRouter, Azure \
                             OpenAI, Groq, DeepSeek, LM Studio, …"
                        }
                        _ => "Where Ollama runs; this machine when empty.",
                    }
                    .into(),
                ),
                div().w(px(260.)).child(Input::new(&self.base_url).small()),
                cx,
            ));
        }
        let models = self.models.get(&p).cloned();
        let description: SharedString = match &models {
            Some(Models::Loading) => "Asking the provider for its models…".into(),
            Some(Models::Failed(e)) => format!("Could not list the models: {e}").into(),
            Some(Models::Loaded(list)) if list.is_empty() => "The provider lists no models.".into(),
            Some(Models::Loaded(_)) => "Type an id or pick one of the provider's models.".into(),
            None => match p.default_model() {
                Some(m) => format!("{m} when empty.").into(),
                None => "Type a model id, or list the provider's models.".into(),
            },
        };
        let list = match models {
            Some(Models::Loaded(list)) if !list.is_empty() => Some(list),
            _ => None,
        };
        let weak = cx.entity().downgrade();
        let can_list = !p.needs_key() || self.state.keys.contains(&p);
        rows.push(row(
            "Model",
            Some(description),
            h_flex()
                .gap_1()
                .child(div().w(px(220.)).child(Input::new(&self.model).small()))
                .map(|this| match list {
                    Some(list) => this.child(
                        Button::new("ai-models")
                            .icon(IconName::ChevronDown)
                            .small()
                            .outline()
                            .dropdown_menu(move |mut menu, _, _| {
                                for model in &list {
                                    let (weak, model) = (weak.clone(), model.clone());
                                    menu = menu.item(PopupMenuItem::new(model.clone()).on_click(
                                        move |_, window, cx| {
                                            let model = model.clone();
                                            let _ = weak.update(cx, |this, cx| {
                                                this.set_model(model, window, cx)
                                            });
                                        },
                                    ));
                                }
                                menu
                            }),
                    ),
                    None => this.child(
                        Button::new("ai-load-models")
                            .label("List models")
                            .small()
                            .outline()
                            .disabled(!can_list)
                            .on_click(cx.listener(move |this, _, _, cx| this.load_models(p, cx))),
                    ),
                }),
            cx,
        ));
        rows
    }
}
