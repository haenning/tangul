//! A tab editing a file instead of SQL (`settings.json`, a theme): only the editor,
//! `⌘S`
//! saves.

use std::path::{Path, PathBuf};

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Editor, EditorState, InputEvent},
    notification::Notification,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_core::TabId;

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

    /// Every key at its default; `None` for files without defaults (themes).
    fn defaults(self) -> Option<String> {
        let value = match self {
            FileKind::Settings => settings::defaults(),
            FileKind::Database => tangul_data::database_settings::defaults(),
            FileKind::Theme => return None,
        };
        Some(tangul_data::upgrade::pretty(&value))
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

pub struct FileTab {
    kind: FileKind,
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
        let changes = cx.subscribe(&editor, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });
        Self {
            kind,
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
                                .tooltip("This version's layout, keeping your values, unsaved")
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
