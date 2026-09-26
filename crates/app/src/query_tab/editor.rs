//! The SQL editor: what a run takes (the selection or everything), the server's
//! check of the statements while typing, and error underlines. Also edits plain
//! files (JSON), without the checks.

use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::base::input::{Diagnostic, DiagnosticSeverity};
use gpui_kit::component::input::{CompletionProvider, EditorState, InputEvent, RopeExt as _};
use gpui_kit::*;
use tangul_assist::analyze::statements;
use tangul_assist::lexer::tokenize;
use tangul_engine::Problem;

/// Pause in typing after which the SQL is checked on the server.
const VALIDATE_AFTER: Duration = Duration::from_secs(1);
/// Limits per check; larger scripts are only checked in part.
const MAX_VALIDATE_STATEMENTS: usize = 50;
const MAX_VALIDATE_BYTES: usize = 100_000;

pub enum EditorEvent {
    /// Check these statements (byte offset in the editor, text) on the server
    /// without running them; the answer goes to [`SqlEditor::apply_validation`].
    Validate {
        generation: u64,
        statements: Vec<(usize, String)>,
    },
}

pub struct SqlEditor {
    state: Entity<EditorState>,
    /// Where the SQL of the next / current run starts in the editor (errors are
    /// reported relative to the executed text), and that text.
    pending_origin: Option<usize>,
    run_origin: Option<(usize, String)>,
    /// Bumped on every edit; check results for older text are dropped.
    edit_generation: u64,
    /// The statements sent for the current generation's check.
    validating: Vec<(usize, String)>,
    _validation: Task<()>,
    _changes: Subscription,
}

impl EventEmitter<EditorEvent> for SqlEditor {}

impl SqlEditor {
    pub fn sql(text: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .default_value(text.to_string())
                .placeholder("SELECT ...   (⌘↵ to run)")
        });
        let changes = cx.subscribe(&state, |this: &mut Self, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                this.revalidate(cx);
            }
        });
        Self::with(state, changes)
    }

    /// A JSON file: no checks, no runs.
    pub fn json(text: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("json")
                .line_number(true)
                .default_value(text.to_string())
        });
        let changes = cx.subscribe(&state, |_: &mut Self, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });
        Self::with(state, changes)
    }

    fn with(state: Entity<EditorState>, changes: Subscription) -> Self {
        Self {
            state,
            pending_origin: None,
            run_origin: None,
            edit_generation: 0,
            validating: Vec::new(),
            _validation: Task::ready(()),
            _changes: changes,
        }
    }

    pub fn state(&self) -> &Entity<EditorState> {
        &self.state
    }

    pub fn text(&self, cx: &App) -> String {
        self.state.read(cx).value().to_string()
    }

    pub fn focus(editor: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let state = editor.read(cx).state.clone();
        state.update(cx, |e, cx| e.focus(window, cx));
    }

    pub fn set_completion(
        editor: &Entity<Self>,
        provider: Rc<dyn CompletionProvider>,
        cx: &mut App,
    ) {
        let state = editor.read(cx).state.clone();
        state.update(cx, |e, _| e.lsp_mut().completion_provider = Some(provider));
    }

    /// What a run takes: the selection if there is one, otherwise everything.
    /// `None` when that is empty.
    pub fn take_run(&mut self, cx: &App) -> Option<String> {
        let editor = self.state.read(cx);
        let selected = editor.selected_value();
        let (sql, origin) = if selected.trim().is_empty() {
            (editor.value(), 0)
        } else {
            (selected, editor.selected_range().start)
        };
        if sql.trim().is_empty() {
            return None;
        }
        self.pending_origin = Some(origin);
        Some(sql.to_string())
    }

    /// `sql` was sent: locates it in the editor (to put error underlines in place)
    /// and clears the last run's underline.
    pub fn started(&mut self, sql: &str, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let origin = self
            .pending_origin
            .take()
            .filter(|&o| text.get(o..).is_some_and(|t| t.starts_with(sql)))
            .or_else(|| text.find(sql));
        self.run_origin = origin.map(|o| (o, sql.to_string()));
        self.set_markers(Vec::new(), cx);
    }

    /// Underlines the token at `position` (byte offset in the run's SQL) with the
    /// error as hover text. Editing clears it (the editor resets diagnostics on every
    /// change).
    pub fn mark_error(&mut self, position: usize, message: String, cx: &mut Context<Self>) {
        let marker = self.run_origin.as_ref().and_then(|(origin, sql)| {
            let span = error_span(sql, position)?;
            Some((origin + span.start..origin + span.end, message))
        });
        self.set_markers(marker.into_iter().collect(), cx);
    }

    /// Checks the SQL once typing paused for [`VALIDATE_AFTER`].
    pub fn revalidate(&mut self, cx: &mut Context<Self>) {
        self.edit_generation += 1;
        let generation = self.edit_generation;
        self._validation = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(VALIDATE_AFTER).await;
            let _ = this.update(cx, |editor, cx| editor.request_validation(generation, cx));
        });
    }

    fn request_validation(&mut self, generation: u64, cx: &mut Context<Self>) {
        if generation != self.edit_generation {
            return;
        }
        let text = self.text(cx);
        self.validating = statements(&text)
            .into_iter()
            .filter(|&(s, e)| !text[s..e].trim().is_empty() && e - s <= MAX_VALIDATE_BYTES)
            .take(MAX_VALIDATE_STATEMENTS)
            .map(|(s, e)| (s, text[s..e].to_string()))
            .collect();
        if self.validating.is_empty() {
            self.set_markers(Vec::new(), cx);
            return;
        }
        cx.emit(EditorEvent::Validate {
            generation,
            statements: self.validating.clone(),
        });
    }

    /// Marks the problems the server found, unless the text changed meanwhile.
    /// Errors that only say the statement is unfinished are left out: that is the
    /// normal state while typing.
    pub fn apply_validation(
        &mut self,
        generation: u64,
        problems: Vec<Problem>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.edit_generation {
            return;
        }
        let markers = problems
            .into_iter()
            .filter_map(|p| {
                let (offset, sql) = self.validating.get(p.statement)?;
                if is_incomplete(sql, &p) {
                    return None;
                }
                let span = error_span(sql, p.position.unwrap_or(0))?;
                Some((offset + span.start..offset + span.end, p.message))
            })
            .collect();
        self.set_markers(markers, cx);
    }

    /// Replaces the error underlines (byte ranges in the editor text).
    fn set_markers(&mut self, markers: Vec<(Range<usize>, String)>, cx: &mut Context<Self>) {
        self.state.update(cx, |editor, cx| {
            let text = editor.text().clone();
            let Some(diagnostics) = editor.diagnostics_mut() else {
                return;
            };
            diagnostics.reset(&text);
            for (range, message) in markers {
                if range.end > text.len() {
                    continue;
                }
                diagnostics.push(Diagnostic {
                    range: text.offset_to_position(range.start)..text.offset_to_position(range.end),
                    severity: DiagnosticSeverity::Error,
                    message: message.into(),
                    ..Default::default()
                });
            }
            cx.notify();
        });
    }
}

/// Whether an error just means the statement isn't finished yet (the normal state
/// while typing): errors at its very end, or about unterminated quotes.
fn is_incomplete(sql: &str, problem: &Problem) -> bool {
    let end = sql.trim_end().trim_end_matches(';').trim_end().len();
    let message = problem.message.to_lowercase();
    problem.position.is_some_and(|p| p >= end)
        || message.contains("at end of input")
        || message.contains("near '' at line")
        || message.contains("unterminated")
}

/// The token an error position points at (or the last token if the error is at the
/// end, e.g. an unfinished statement).
fn error_span(sql: &str, position: usize) -> Option<Range<usize>> {
    let tokens: Vec<_> = tokenize(sql)
        .into_iter()
        .filter(|t| !t.is_trivia())
        .collect();
    tokens
        .iter()
        .find(|t| t.end > position)
        .or(tokens.last())
        .map(|t| t.start..t.end)
}

#[cfg(test)]
mod tests {
    use super::{Problem, error_span, is_incomplete};

    #[test]
    fn incomplete_statements_are_not_errors() {
        let p = |position, message: &str| Problem {
            statement: 0,
            position,
            message: message.into(),
        };
        let sql = "select * from ";
        assert!(is_incomplete(
            sql,
            &p(Some(14), "syntax error at end of input")
        ));
        // Position at the end, without the "end of input" wording.
        assert!(is_incomplete(
            "select * from users where;",
            &p(Some(25), "syntax error")
        ));
        assert!(is_incomplete(
            "select 'abc",
            &p(Some(7), "unterminated quoted string")
        ));
        assert!(!is_incomplete(
            "select * form users",
            &p(Some(9), "syntax error at or near \"form\"")
        ));
        assert!(!is_incomplete(
            "select nme from users",
            &p(None, "Unknown column 'nme'")
        ));
    }

    #[test]
    fn spans() {
        let sql = "select * form users";
        assert_eq!(error_span(sql, 9), Some(9..13));
        // Position in whitespace: the next token.
        assert_eq!(error_span(sql, 8), Some(9..13));
        // At the end: the last token.
        assert_eq!(error_span("select * from", 13), Some(9..13));
        assert_eq!(error_span("", 0), None);
    }
}
