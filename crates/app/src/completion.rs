//! Bridges `tangul_assist::complete` into the editor's completion menu, and
//! `tangul_assist::complete::continuation` into its ghost text (Tab takes it).

use std::sync::{Arc, Mutex, PoisonError};

use anyhow::Result;
use gpui_kit::component::input::{CompletionProvider, Rope, RopeExt as _};
use gpui_kit::*;
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    InlineCompletionContext, InlineCompletionItem, InlineCompletionResponse, TextEdit,
};
use tangul_assist::complete::{SuggestionKind, complete, continuation};
use tangul_service::knowledge::Knowledge;

use crate::query_tab::QueryTab;

/// Items of the last menu whose cursor goes inside them (`''`): the inserted text
/// and where in it the cursor goes. The editor moves the cursor there after such
/// an insert, which the menu itself can't.
#[derive(Clone, Default)]
pub struct Snippets(Arc<Mutex<Vec<(String, usize)>>>);

impl Snippets {
    pub(crate) fn set(&self, items: Vec<(String, usize)>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = items;
    }

    /// Where the cursor goes when `before` (the text before it) ends with one of
    /// the snippets: just inserted. Forgets them.
    pub fn cursor_after(&self, before: &str) -> Option<usize> {
        let mut items = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let at = items
            .iter()
            .find(|(insert, _)| before.ends_with(insert.as_str()))
            .map(|(insert, at)| before.len() - insert.len() + at);
        if at.is_some() {
            items.clear();
        }
        at
    }
}

pub struct SqlCompletion {
    /// Schema, usage and most run queries of the tab's connection, from the one pool.
    pub knowledge: Knowledge,
    pub tab: WeakEntity<QueryTab>,
    pub snippets: Snippets,
}

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
        let snippets = self.snippets.clone();
        cx.background_spawn(async move {
            let text = rope.to_string();
            let completion = complete(&text, offset, &schema, &usage);
            // Without a word being typed (after a space, `(`, `'`) only where what
            // follows is clear: a table, a unit, a value for the column, ….
            let typing = text
                .get(..offset)
                .and_then(|b| b.chars().next_back())
                .is_some_and(is_word_char);
            if !typing && !completion.eager {
                return Ok(CompletionResponse::Array(Vec::new()));
            }
            snippets.set(
                completion
                    .items
                    .iter()
                    .filter_map(|s| Some((s.insert.clone(), s.cursor?)))
                    .collect(),
            );
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
                        SuggestionKind::Value => CompletionItemKind::VALUE,
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

    /// The rest of the statement as it was run most often before, in ghost text.
    fn inline_completion(
        &self,
        rope: &Rope,
        offset: usize,
        _: InlineCompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<InlineCompletionResponse>> {
        let Some(conn) = self.tab.upgrade().and_then(|t| t.read(cx).conn_id(cx)) else {
            return Task::ready(Ok(InlineCompletionResponse::Array(Vec::new())));
        };
        let history = self.knowledge.history_for_conn(conn);
        let rope = rope.clone();
        cx.background_spawn(async move {
            let items = continuation(&rope.to_string(), offset, &history)
                .map(|rest| InlineCompletionItem {
                    insert_text: rest,
                    filter_text: None,
                    range: None,
                    command: None,
                    insert_text_format: None,
                })
                .into_iter()
                .collect();
            Ok(InlineCompletionResponse::Array(items))
        })
    }

    /// Word characters and `.` always ask; a space, `(`, `'` and `:` ask too, and
    /// `completions` then answers only where what follows is clear. (The editor is
    /// mid-update here, so its text cannot be read.)
    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        matches!(new_text, " " | "(" | "'" | ":")
            || new_text.chars().any(|c| is_word_char(c) || c == '.')
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '.'
}
