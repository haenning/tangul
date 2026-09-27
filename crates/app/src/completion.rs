//! Bridges `tangul_assist::complete` into the editor's completion menu.

use anyhow::Result;
use gpui_kit::component::input::{CompletionProvider, Rope, RopeExt as _};
use gpui_kit::*;
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    TextEdit,
};
use tangul_assist::complete::{SuggestionKind, complete};
use tangul_service::knowledge::Knowledge;

use crate::query_tab::QueryTab;

pub struct SqlCompletion {
    /// Schema and usage of the tab's connection, from the one pool.
    pub knowledge: Knowledge,
    pub tab: WeakEntity<QueryTab>,
}

/// After these words a space opens the menu (a table, join or condition follows).
const SPACE_TRIGGERS: &[&str] = &["from", "join", "on", "into", "update"];

impl CompletionProvider for SqlCompletion {
    fn completions(
        &self,
        rope: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let empty = || Task::ready(Ok(CompletionResponse::Array(Vec::new())));
        let Some(conn) = self.tab.upgrade().and_then(|t| t.read(cx).conn_id(cx)) else {
            return empty();
        };
        let Some((schema, usage)) = self.knowledge.for_conn(conn) else {
            return empty();
        };
        let rope = rope.clone();
        cx.background_spawn(async move {
            let text = rope.to_string();
            // Typing a space only opens the menu where a table, join or condition follows.
            let before = text.get(..offset).unwrap_or_default();
            if before.ends_with(char::is_whitespace) {
                let word = before
                    .trim_end()
                    .rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .next()
                    .unwrap_or_default();
                if !SPACE_TRIGGERS.iter().any(|w| word.eq_ignore_ascii_case(w)) {
                    return Ok(CompletionResponse::Array(Vec::new()));
                }
            }
            let completion = complete(&text, offset, &schema, &usage);
            let range = lsp_types::Range {
                start: rope.offset_to_position(completion.replace.start),
                end: rope.offset_to_position(completion.replace.end),
            };
            let typed = text[completion.replace.clone()].to_string();
            let items = completion
                .items
                .into_iter()
                .map(|s| CompletionItem {
                    label: s.label,
                    kind: Some(match s.kind {
                        SuggestionKind::Keyword => CompletionItemKind::KEYWORD,
                        SuggestionKind::Function => CompletionItemKind::FUNCTION,
                        SuggestionKind::Table => CompletionItemKind::CLASS,
                        SuggestionKind::View => CompletionItemKind::INTERFACE,
                        SuggestionKind::Column => CompletionItemKind::FIELD,
                        SuggestionKind::Join => CompletionItemKind::SNIPPET,
                    }),
                    detail: Some(s.detail),
                    filter_text: Some(typed.clone()),
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                        range,
                        new_text: s.insert,
                    })),
                    ..Default::default()
                })
                .collect();
            Ok(CompletionResponse::Array(items))
        })
    }

    /// Word characters and `.` always ask; a space asks too, and `completions` then
    /// answers only after FROM/JOIN/ON/…. (The editor is mid-update here, so its text
    /// cannot be read.)
    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        new_text == " "
            || new_text
                .chars()
                .any(|c| c.is_alphanumeric() || c == '_' || c == '.')
    }
}
