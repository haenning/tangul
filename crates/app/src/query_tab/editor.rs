//! The SQL editor: what a run takes (the selection, the statement under the cursor or
//! everything), the server's check of the statements while typing, error underlines,
//! and comments as the database reads them.

use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::base::input::{
    Diagnostic, DiagnosticSeverity, HighlightStyleResolver as _, TextDecoration,
    TextDecorationCollection,
};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{CompletionProvider, EditorState, InputEvent, RopeExt as _};
use gpui_kit::*;
use tangul_assist::analyze::statements;
use tangul_assist::dialect::Dialect;
use tangul_assist::lexer::{Lexing, TokenKind, tokenize, tokenize_with};
use tangul_assist::statement::statement_at;
use tangul_engine::Problem;

/// Pause in typing after which the SQL is checked on the server.
const VALIDATE_AFTER: Duration = Duration::from_secs(1);
/// Limits per check; larger scripts are only checked in part.
const MAX_VALIDATE_STATEMENTS: usize = 50;
const MAX_VALIDATE_BYTES: usize = 100_000;
/// How long the statement that ran flashes before it settles to a light background.
const FLASH: Duration = Duration::from_millis(600);
const FLASH_STEPS: u32 = 12;

/// What a run takes from the editor.
pub enum RunTake {
    Sql(String),
    /// No selection, and the cursor is between statements.
    NoStatement,
    /// Nothing to run.
    Empty,
}

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
    /// What the next run takes (the statement under the cursor or the selection),
    /// marked once it starts; `None` when it takes the whole editor.
    pending_mark: Option<Range<usize>>,
    /// Marks the text the current result came from, until the next run.
    ran: Option<TextDecorationCollection>,
    /// How the connection's database reads comments.
    lexing: Lexing,
    /// Comments in the comment color, also those the highlighter's grammar doesn't
    /// know (`#` on MySQL, nested and unclosed `/* */`).
    comments: TextDecorationCollection,
    _flash: Task<()>,
    /// Bumped on every edit; check results for older text are dropped.
    edit_generation: u64,
    /// The statements sent for the current generation's check.
    validating: Vec<(usize, String)>,
    _validation: Task<()>,
    _changes: Subscription,
}

impl EventEmitter<EditorEvent> for SqlEditor {}

impl SqlEditor {
    pub fn new(text: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .default_value(text.to_string())
                .placeholder("SELECT ...   (⌘↵ to run)")
        });
        let changes = cx.subscribe(&state, |this: &mut Self, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                this.color_comments(cx);
                this.revalidate(cx);
            }
        });
        let comments = state.update(cx, |e, cx| e.create_decorations_collection(Vec::new(), cx));
        let mut editor = Self {
            state,
            pending_origin: None,
            run_origin: None,
            pending_mark: None,
            ran: None,
            lexing: Lexing::default(),
            comments,
            _flash: Task::ready(()),
            edit_generation: 0,
            validating: Vec::new(),
            _validation: Task::ready(()),
            _changes: changes,
        };
        editor.color_comments(cx);
        editor
    }

    /// The connection's database changed: its comments may be spelled differently.
    pub fn set_lexing(&mut self, lexing: Lexing, cx: &mut Context<Self>) {
        if self.lexing != lexing {
            self.lexing = lexing;
            self.color_comments(cx);
        }
    }

    /// The ranges shown as comments (tests).
    #[cfg(test)]
    pub fn comments(&self, cx: &App) -> Vec<Range<usize>> {
        self.comments.get_ranges(cx)
    }

    fn color_comments(&mut self, cx: &mut Context<Self>) {
        let style = cx
            .theme()
            .highlight_theme
            .style("comment")
            .unwrap_or_else(|| HighlightStyle {
                color: Some(cx.theme().muted_foreground),
                ..Default::default()
            });
        let decorations = comment_ranges(&self.text(cx), self.lexing)
            .into_iter()
            .map(|range| TextDecoration::new(range, style))
            .collect();
        self.comments.set(decorations, cx);
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

    /// What ⌘↵ takes: the selection if there is one, otherwise the statement under
    /// the cursor (see [`statement_at`]).
    pub fn take_run(&mut self, dialect: &dyn Dialect, cx: &App) -> RunTake {
        let editor = self.state.read(cx);
        if !editor.selected_value().trim().is_empty() {
            return self.take_run_all(cx);
        }
        let text = editor.value().to_string();
        if text.trim().is_empty() {
            return RunTake::Empty;
        }
        let Some(statement) = statement_at(&text, editor.cursor(), dialect) else {
            return RunTake::NoStatement;
        };
        self.pending_origin = Some(statement.body.start);
        self.pending_mark = Some(statement.range.clone());
        RunTake::Sql(statement.text(&text).to_string())
    }

    /// What "Run all" takes: the selection if there is one, otherwise everything.
    pub fn take_run_all(&mut self, cx: &App) -> RunTake {
        let editor = self.state.read(cx);
        let selected = editor.selected_value();
        let (sql, mark) = if selected.trim().is_empty() {
            (editor.value(), None)
        } else {
            (selected, Some(editor.selected_range()))
        };
        if sql.trim().is_empty() {
            return RunTake::Empty;
        }
        self.pending_origin = Some(mark.as_ref().map_or(0, |m| m.start));
        self.pending_mark = mark;
        RunTake::Sql(sql.to_string())
    }

    /// `sql` was sent: locates it in the editor (to put error underlines in place),
    /// clears the last run's underline and marks what ran.
    pub fn started(&mut self, sql: &str, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let origin = self
            .pending_origin
            .take()
            .filter(|&o| text.get(o..).is_some_and(|t| t.starts_with(sql)))
            .or_else(|| text.find(sql));
        self.run_origin = origin.map(|o| (o, sql.to_string()));
        self.set_markers(Vec::new(), cx);
        let mark = self.pending_mark.take();
        self.mark_ran(mark, cx);
    }

    /// Marks `range` (what ran, terminator included) as the source of the result: it
    /// flashes, then keeps a light background until the next run. The mark follows
    /// edits. `None` clears the mark (a run of the whole editor needs none).
    fn mark_ran(&mut self, range: Option<Range<usize>>, cx: &mut Context<Self>) {
        let collection = match &self.ran {
            Some(c) => c.clone(),
            None => {
                let c = self
                    .state
                    .update(cx, |e, cx| e.create_decorations_collection(Vec::new(), cx));
                self.ran = Some(c.clone());
                c
            }
        };
        let Some(range) = range else {
            self._flash = Task::ready(());
            collection.clear(cx);
            return;
        };
        let (flash, rest) = (cx.theme().selection, cx.theme().muted);
        self._flash = cx.spawn(async move |_, cx| {
            for step in 0..=FLASH_STEPS {
                let fade = 1. - step as f32 / FLASH_STEPS as f32;
                let style = HighlightStyle {
                    background_color: Some(rest.blend(flash.opacity(fade))),
                    ..Default::default()
                };
                let decoration = TextDecoration::new(range.clone(), style);
                // A no-op once the editor is gone.
                cx.update(|cx| collection.set(vec![decoration], cx));
                if step < FLASH_STEPS {
                    cx.background_executor().timer(FLASH / FLASH_STEPS).await;
                }
            }
        });
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

/// Byte ranges of the comments in `sql`.
fn comment_ranges(sql: &str, lexing: Lexing) -> Vec<Range<usize>> {
    tokenize_with(sql, lexing)
        .into_iter()
        .filter(|t| t.kind == TokenKind::Comment)
        .map(|t| t.start..t.end)
        .collect()
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
    use super::{Problem, comment_ranges, error_span, is_incomplete};
    use tangul_assist::lexer::Lexing;

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

    #[test]
    fn comments_as_the_database_reads_them() {
        let mysql = Lexing {
            hash_comments: true,
            dollar_quotes: false,
            ..Lexing::default()
        };
        let postgres = Lexing {
            nested_comments: true,
            ..Lexing::default()
        };
        let texts = |sql: &str, lexing| -> Vec<String> {
            comment_ranges(sql, lexing)
                .into_iter()
                .map(|r| sql[r].to_string())
                .collect()
        };
        assert_eq!(
            texts("select 1 -- a\n/* b */ from t", Lexing::default()),
            ["-- a", "/* b */"]
        );
        assert_eq!(
            texts("# select * from t\nselect 1", mysql),
            ["# select * from t"]
        );
        assert_eq!(
            texts("# select 1", postgres),
            Vec::<String>::new(),
            "no # comments on Postgres"
        );
        assert_eq!(
            texts("/* a /* b */ select 1 */ select 2", postgres),
            ["/* a /* b */ select 1 */"]
        );
        // Unclosed: the rest is a comment.
        assert_eq!(
            texts("select 1 /* where a", Lexing::default()),
            ["/* where a"]
        );
        // Not in strings.
        assert_eq!(
            texts("select '-- a', '/* b */'", Lexing::default()),
            Vec::<String>::new()
        );
    }
}
