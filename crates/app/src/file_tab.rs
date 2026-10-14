//! A tab editing a file instead of SQL (`settings.json`, a theme, a database's
//! settings): only the editor, `⌘S` saves. Values that can't be applied are
//! underlined with what would do. In a database's file, hovering its rules says
//! what they hit, and typing a pattern hints its parts and offers the database's
//! names.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::base::input::{Diagnostic, DiagnosticSeverity};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{
        CompletionProvider, Editor, EditorState, HoverProvider, IndentInline, InputEvent, Rope,
        RopeExt as _,
    },
    notification::Notification,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    Hover, HoverContents, InlineCompletionContext, InlineCompletionItem, InlineCompletionResponse,
    MarkupContent, MarkupKind, TextEdit,
};
use tangul_assist::ai_rules;
use tangul_assist::dialect::Namespace;
use tangul_assist::json::{self, Problem, Severity};
use tangul_assist::schema::SchemaIndex;
use tangul_core::TabId;
use tangul_service::knowledge::Knowledge;

use crate::{SaveFile, settings};

/// What a file is, to check it when saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Settings,
    /// A color theme (applied when the folder is read again, within a second).
    Theme,
    /// A database's own settings (read whenever needed).
    Database,
}

impl FileKind {
    /// Why `text` can't be applied, if it can't.
    fn problem(self, text: &str) -> Option<String> {
        match self {
            FileKind::Settings => settings::problem(text),
            FileKind::Theme => tangul_data::themes::Theme::parse(text).err(),
            FileKind::Database => tangul_data::database_settings::parse(text).err(),
        }
    }

    /// What to underline in `text`.
    fn problems(self, text: &str, database: Option<&DatabaseFile>) -> Vec<Problem> {
        match self {
            FileKind::Settings => json::check(text, &settings::defaults(), &|v| {
                settings::value_problem(v).map_or(Ok(()), Err)
            }),
            FileKind::Theme => json::parse(text).err().into_iter().collect(),
            FileKind::Database => {
                use tangul_data::database_settings as db;
                let mut problems = json::check(text, &db::defaults(), &|v| {
                    db::parse_value(v.clone()).map(|_| ())
                });
                if let Ok(root) = json::parse(text) {
                    let schema = database.and_then(DatabaseFile::schema);
                    problems.extend(ai_rules::problems(&root, schema.as_deref()));
                    // One popup per place: what a marked pattern means goes below
                    // the problem (hovering it shows only this).
                    let namespace = database.map_or(Namespace::Schema, |d| d.namespace);
                    for p in &mut problems {
                        let explained =
                            ai_rules::hover(&root, p.range.start, schema.as_deref(), namespace)
                                .filter(|(range, _)| *range == p.range);
                        if let Some((_, text)) = explained {
                            p.message = format!("{}\n\n{text}", p.message);
                        }
                    }
                }
                problems
            }
        }
    }

    /// Every key at its default; `None` for files without defaults (themes).
    fn defaults(self) -> Option<String> {
        let value = match self {
            FileKind::Settings => settings::defaults(),
            FileKind::Database => tangul_data::database_settings::defaults(),
            FileKind::Theme => return None,
        };
        Some(tangul_data::upgrade::pretty(&value))
    }

    /// Whether `text` already has this version's layout: nothing to upgrade.
    fn is_latest(self, text: &str) -> bool {
        use tangul_data::{migrations, upgrade};
        match self {
            FileKind::Settings => upgrade::is_latest(text, migrations::SETTINGS),
            FileKind::Database => upgrade::is_latest(text, migrations::DATABASE),
            FileKind::Theme => true,
        }
    }

    /// `text` in this version's layout, keeping what the user set.
    fn upgrade(self, text: &str) -> Option<Result<String, String>> {
        match self {
            FileKind::Settings => Some(settings::upgrade(text)),
            FileKind::Database => Some(tangul_data::database_settings::upgrade(text)),
            FileKind::Theme => None,
        }
    }
}

/// Pause in typing after which the text is checked again.
const CHECK_AFTER: Duration = Duration::from_millis(300);

/// What a database's settings file knows of its database: its schema as tangul
/// last read it (from the one pool), and what the level above tables is called.
#[derive(Clone)]
pub struct DatabaseFile {
    pub id: String,
    pub knowledge: Knowledge,
    pub namespace: Namespace,
}

impl DatabaseFile {
    fn schema(&self) -> Option<Arc<SchemaIndex>> {
        self.knowledge.database_schema(&self.id)
    }
}

pub struct FileTab {
    kind: FileKind,
    database: Option<DatabaseFile>,
    /// Where the hint of a pattern's parts shows: cursor and text length. Tab
    /// doesn't take it (it is no text to insert).
    hint_at: Rc<Cell<Option<(usize, usize)>>>,
    /// Whether the text has this version's layout (Upgrade then has nothing to do).
    latest: bool,
    _check: Task<()>,
    _cursor: Subscription,
    pub id: TabId,
    pub title: SharedString,
    path: PathBuf,
    editor: Entity<EditorState>,
    /// The text as last read or saved, to show unsaved changes.
    saved: String,
    /// Why the saved text isn't applied (invalid settings), if it isn't.
    problem: Option<SharedString>,
    _changes: Subscription,
}

impl FileTab {
    pub fn new(
        id: TabId,
        path: PathBuf,
        kind: FileKind,
        database: Option<DatabaseFile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("json")
                .line_number(true)
                .default_value(text.clone())
        });
        let hint_at = Rc::new(Cell::new(None));
        if let Some(database) = database.clone().filter(|_| kind == FileKind::Database) {
            let rules = Rc::new(RulesEditing {
                database,
                hint_at: hint_at.clone(),
            });
            editor.update(cx, |e, _| {
                let lsp = e.lsp_mut();
                lsp.completion_provider = Some(rules.clone());
                lsp.hover_provider = Some(rules);
            });
        }
        let changes = cx.subscribe(&editor, |this: &mut Self, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                this.check_soon(cx);
                cx.notify();
            }
        });
        // Typing over a pattern's closing quote moves the cursor without changing the
        // text, and the hint would follow it out of the pattern: cleared then.
        let cursor = cx.observe(&editor, |this: &mut Self, editor, cx| {
            let Some((at, len)) = this.hint_at.get() else {
                return;
            };
            let (range, now) = {
                let e = editor.read(cx);
                (e.selected_range(), e.value().len())
            };
            if now == len && range != (at..at) {
                this.hint_at.set(None);
                if range.is_empty() {
                    editor.update(cx, |e, cx| e.set_selected_range(range, cx));
                }
            }
        });
        let mut tab = Self {
            kind,
            database,
            hint_at,
            latest: false,
            _check: Task::ready(()),
            _cursor: cursor,
            id,
            title: path
                .file_name()
                .map_or("settings.json".into(), |n| n.to_string_lossy().to_string())
                .into(),
            path,
            editor,
            saved: text,
            problem: None,
            _changes: changes,
        };
        tab.check(cx);
        tab
    }

    fn check_soon(&mut self, cx: &mut Context<Self>) {
        self._check = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CHECK_AFTER).await;
            let _ = this.update(cx, |this, cx| this.check(cx));
        });
    }

    /// Underlines what can't be applied (red) and what looks wrong (yellow), the
    /// reason as hover text.
    fn check(&mut self, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        let problems = self.kind.problems(&text, self.database.as_ref());
        self.latest = self.kind.is_latest(&text);
        self.editor.update(cx, |editor, cx| {
            let rope = editor.text().clone();
            let Some(diagnostics) = editor.diagnostics_mut() else {
                return;
            };
            diagnostics.reset(&rope);
            for p in problems {
                if p.range.end > rope.len() {
                    continue;
                }
                diagnostics.push(Diagnostic {
                    range: rope.offset_to_position(p.range.start)
                        ..rope.offset_to_position(p.range.end),
                    severity: match p.severity {
                        Severity::Error => DiagnosticSeverity::Error,
                        Severity::Warning => DiagnosticSeverity::Warning,
                    },
                    message: p.message.into(),
                    ..Default::default()
                });
            }
            cx.notify();
        });
    }

    /// The problems underlined now: (text, message).
    #[cfg(test)]
    pub fn marked(&self, cx: &App) -> Vec<(String, String)> {
        let text = self.editor.read(cx).value().to_string();
        self.kind
            .problems(&text, self.database.as_ref())
            .into_iter()
            .map(|p| (text[p.range].to_string(), p.message))
            .collect()
    }

    #[cfg(test)]
    pub fn place_cursor(&self, at: usize, window: &mut Window, cx: &mut App) {
        self.editor.update(cx, |e, cx| {
            e.focus(window, cx);
            e.set_selected_range(at..at, cx);
        });
    }

    /// Whether a pattern's parts are hinted at the cursor.
    #[cfg(test)]
    pub fn hint_shown(&self, cx: &App) -> bool {
        let editor = self.editor.read(cx);
        self.hint_at.get() == Some((editor.cursor(), editor.value().len()))
    }

    /// Tab while a pattern's parts are hinted: nothing (the hint is no text).
    fn keep_hint(&mut self, _: &IndentInline, _: &mut Window, cx: &mut Context<Self>) {
        let editor = self.editor.read(cx);
        if self.hint_at.get() == Some((editor.cursor(), editor.value().len())) {
            cx.stop_propagation();
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub fn text(&self, cx: &App) -> String {
        self.editor.read(cx).value().to_string()
    }

    pub fn has_unsaved_changes(&self, cx: &App) -> bool {
        self.saved != self.editor.read(cx).value().as_ref()
    }

    pub fn focus(tab: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let editor = tab.read(cx).editor.clone();
        editor.update(cx, |e, cx| e.focus(window, cx));
    }

    /// Puts `text` into the editor, unsaved: the user looks at it, then saves.
    fn replace_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |e, cx| e.set_value(text, window, cx));
        cx.notify();
    }

    fn revert_to_default(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.kind.defaults() {
            self.replace_text(text, window, cx);
        }
    }

    fn upgrade(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        match self.kind.upgrade(&text) {
            Some(Ok(new)) => self.replace_text(new, window, cx),
            Some(Err(e)) => {
                window.push_notification(Notification::error(format!("Could not upgrade: {e}")), cx)
            }
            None => {}
        }
    }

    fn save_action(&mut self, _: &SaveFile, window: &mut Window, cx: &mut Context<Self>) {
        self.save(window, cx);
    }

    /// Writes the text; `false` (with a notification) if that failed. Settings that
    /// don't parse are saved anyway (it is the user's file) but not applied; the tab
    /// says why.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let text = self.editor.read(cx).value().to_string();
        if let Err(e) = std::fs::write(&self.path, &text) {
            window.push_notification(
                Notification::error(format!("Could not save {}: {e}", self.path.display())),
                cx,
            );
            return false;
        }
        self.problem = self.kind.problem(&text).map(Into::into);
        self.saved = text;
        cx.notify();
        true
    }
}

impl Render for FileTab {
    /// The path, the save state and the editor over the whole tab.
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let unsaved = self.has_unsaved_changes(cx);
        let (status, color) = match (&self.problem, unsaved) {
            (_, true) => ("Unsaved changes".into(), theme.muted_foreground),
            (Some(problem), false) => (format!("Saved, not applied: {problem}"), theme.danger),
            (None, false) => (
                "Saved; changes apply right away".into(),
                theme.muted_foreground,
            ),
        };
        v_flex()
            .size_full()
            .on_action(cx.listener(Self::save_action))
            .child(
                h_flex()
                    .h_9()
                    .px_2()
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_xs()
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(self.path.display().to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(color)
                            .child(status),
                    )
                    .when(self.kind.defaults().is_some(), |bar| {
                        bar.child(
                            Button::new("revert-file")
                                .label("Revert to default")
                                .small()
                                .ghost()
                                .tooltip("Every option at its default, unsaved")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.revert_to_default(window, cx);
                                })),
                        )
                        .child(
                            Button::new("upgrade-file")
                                .label("Upgrade")
                                .small()
                                .ghost()
                                .disabled(self.latest)
                                .tooltip(if self.latest {
                                    "Already on the latest version"
                                } else {
                                    "This version's layout, keeping your values, unsaved"
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.upgrade(window, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new("save-file")
                            .label("Save")
                            .small()
                            .when(unsaved, |b| b.primary())
                            .when(!unsaved, |b| b.ghost())
                            .tooltip("Save (⌘S)")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save(window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .key_context("QueryEditor")
                    .capture_action(cx.listener(Self::keep_hint))
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(Editor::new(&self.editor).h_full().bordered(false)),
            )
    }
}

impl Focusable for FileTab {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}

/// In a database's settings file: what its rules hit on hover, the parts of a
/// pattern as a hint while typing it, and the database's names for the part.
struct RulesEditing {
    database: DatabaseFile,
    hint_at: Rc<Cell<Option<(usize, usize)>>>,
}

impl HoverProvider for RulesEditing {
    fn hover(
        &self,
        rope: &Rope,
        offset: usize,
        _: &mut Window,
        _: &mut App,
    ) -> Task<Result<Option<Hover>>> {
        let text = rope.to_string();
        // Where something is marked, its popup says it all (see `problems`).
        let marked = FileKind::Database
            .problems(&text, Some(&self.database))
            .iter()
            .any(|p| p.range.contains(&offset));
        let schema = self.database.schema();
        let hover = json::parse(&text)
            .ok()
            .filter(|_| !marked)
            .and_then(|root| {
                ai_rules::hover(&root, offset, schema.as_deref(), self.database.namespace)
            });
        Task::ready(Ok(hover.map(|(range, value)| Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value,
            }),
            range: Some(lsp_types::Range::new(
                rope.offset_to_position(range.start),
                rope.offset_to_position(range.end),
            )),
        })))
    }
}

impl CompletionProvider for RulesEditing {
    /// The database's names for the pattern's part, once a letter is typed.
    fn completions(
        &self,
        rope: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        _: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let text = rope.to_string();
        let items = ai_rules::typing(&text, offset)
            .zip(self.database.schema())
            .map(|(typing, schema)| {
                let range = lsp_types::Range::new(
                    rope.offset_to_position(typing.prefix_start),
                    rope.offset_to_position(offset),
                );
                ai_rules::names(&typing, &schema)
                    .into_iter()
                    .map(|name| CompletionItem {
                        label: name.clone(),
                        kind: Some(match typing.part() {
                            2 => CompletionItemKind::FIELD,
                            1 => CompletionItemKind::STRUCT,
                            _ => CompletionItemKind::MODULE,
                        }),
                        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                            range,
                            new_text: name,
                        })),
                        ..Default::default()
                    })
                    .collect()
            })
            .unwrap_or_default();
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    /// The parts still to type, as ghost text (`schema.table.column`).
    fn inline_completion(
        &self,
        rope: &Rope,
        offset: usize,
        _: InlineCompletionContext,
        _: &mut Window,
        _: &mut App,
    ) -> Task<Result<InlineCompletionResponse>> {
        let text = rope.to_string();
        let hint = ai_rules::typing(&text, offset)
            .and_then(|t| ai_rules::placeholder(&t, self.database.namespace));
        self.hint_at
            .set(hint.as_ref().map(|_| (offset, rope.len())));
        // Ghost text covers what follows the cursor on its line, so that comes
        // along (the closing quote stays in view).
        let rest = &text[offset..];
        let rest = &rest[..rest.find('\n').unwrap_or(rest.len())];
        let items = hint
            .map(|hint| hint + rest)
            .map(|insert_text| InlineCompletionItem {
                insert_text,
                filter_text: None,
                range: None,
                command: None,
                insert_text_format: None,
            })
            .into_iter()
            .collect();
        Task::ready(Ok(InlineCompletionResponse::Array(items)))
    }

    /// Right away: the hint follows each key.
    fn inline_completion_debounce(&self) -> Duration {
        Duration::ZERO
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        new_text.chars().any(|c| c.is_alphanumeric() || c == '_')
    }
}

#[cfg(test)]
mod tests {
    use super::{FileKind, settings};

    fn marked(kind: FileKind, text: &str) -> Vec<(String, String)> {
        kind.problems(text, None)
            .into_iter()
            .map(|p| (text[p.range].to_string(), p.message))
            .collect()
    }

    #[test]
    fn every_kind_of_file_is_checked_value_by_value() {
        let m = marked(
            FileKind::Settings,
            r#"{"theme": {"appearance": "dim"}, "safety": {"relock_after": "5 min"}, "nope": 1}"#,
        );
        assert_eq!(m.len(), 3, "{m:?}");
        assert_eq!(m[0].0, "\"dim\"");
        assert!(m[0].1.contains("`auto`, `dark`, `light`"), "{m:?}");
        assert_eq!(m[1].0, "\"5 min\"");
        assert!(m[1].1.contains("expected e.g. \"30s\""), "{m:?}");
        assert_eq!(m[2].0, "\"nope\"");
        assert!(m[2].1.starts_with("unknown key, not used"), "{m:?}");
        assert!(marked(FileKind::Settings, &settings::defaults().to_string()).is_empty());

        let m = marked(
            FileKind::Database,
            r#"{"ai": {"content_blocking": {"redact": ["..email", "users", "!.."], "hdie": []}}}"#,
        );
        let texts: Vec<&str> = m.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(texts, ["\"users\"", "\"hdie\"", "\"!..\""], "{m:?}");
        assert!(m[0].1.contains("has no dot"), "{m:?}");
        // A pattern that can be read: its explanation in the same popup.
        assert!(m[2].1.contains("\n\n**Exception:**"), "{m:?}");
        assert!(m[1].1.contains("unknown field `hdie`"), "{m:?}");

        assert_eq!(marked(FileKind::Theme, "{").len(), 1);
    }

    #[test]
    fn only_an_older_layout_can_be_upgraded() {
        let current = settings::defaults().to_string();
        assert!(FileKind::Settings.is_latest(&current));
        assert!(!FileKind::Settings.is_latest(r#"{"editor": {}}"#));
        assert!(!FileKind::Database.is_latest(r#"{"version": "0.0.1"}"#));
    }
}
