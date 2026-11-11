//! Drawn over an editor, after it in the same frame (so it never lags behind
//! scrolling): notes after a line's end in the muted color (when a statement last
//! ran). Positions come from the editor's own layout (`range_to_bounds`), so
//! soft-wrapped lines are placed right.

use std::ops::Range;
use std::rc::Rc;

use gpui_kit::component::input::EditorState;
use gpui_kit::*;
/// Space between a line's end and its note, in widths of a character.
const NOTE_GAP: f32 = 2.;

/// What to draw over an editor.
#[derive(Clone, Default)]
pub struct Overlay {
    /// (byte offset on the line, text): drawn after that line's end.
    pub notes: Rc<Vec<(usize, SharedString)>>,
    pub note_color: Hsla,
}

impl Overlay {
    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    /// An element filling its parent that paints this over `editor`.
    pub fn element(self, editor: Entity<EditorState>) -> impl IntoElement {
        div()
            .id("editor-overlay")
            .test_support()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .child(self.canvas(editor))
    }

    fn canvas(self, editor: Entity<EditorState>) -> impl IntoElement {
        canvas(
            |_, _, _| (),
            move |_, _, window, cx| {
                let state = editor.read(cx);
                let Some(text_bounds) = state.text_bounds() else {
                    return;
                };
                let text = state.value();
                let lines = Lines::of(&text);
                let Some(geometry) = Geometry::of(state, &text, &lines) else {
                    return;
                };
                let notes = self.note_places(&geometry, &text, &lines);
                window.with_content_mask(
                    Some(ContentMask {
                        bounds: text_bounds,
                    }),
                    |window| {
                        let style = window.text_style();
                        let font_size = style.font_size.to_pixels(window.rem_size()) * 0.85;
                        for (origin, note) in notes {
                            let run = TextRun {
                                len: note.len(),
                                font: style.font(),
                                color: self.note_color,
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            };
                            let line =
                                window
                                    .text_system()
                                    .shape_line(note, font_size, &[run], None);
                            // Too long for the room after the line: left out.
                            if origin.x + line.width > text_bounds.right() {
                                continue;
                            }
                            let _ = line.paint(
                                origin,
                                geometry.line_height,
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            );
                        }
                    },
                );
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// Where each visible note starts: after the end of its line.
    fn note_places(
        &self,
        g: &Geometry,
        text: &str,
        lines: &Lines,
    ) -> Vec<(Point<Pixels>, SharedString)> {
        self.notes
            .iter()
            .filter_map(|(at, note)| {
                let line = lines.line_of(*at);
                if !g.visible.contains(&line) {
                    return None;
                }
                let end = line_end(text, *at);
                let b = g.bounds(end..end)?;
                Some((
                    point(b.left() + g.char_width * NOTE_GAP, b.top()),
                    note.clone(),
                ))
            })
            .collect()
    }
}

/// The editor's last layout, as the overlay needs it.
struct Geometry<'a> {
    state: &'a EditorState,
    /// Visible buffer lines.
    visible: Range<usize>,
    line_height: Pixels,
    /// One character's width (the editor's font is monospaced).
    char_width: Pixels,
}

impl<'a> Geometry<'a> {
    fn of(state: &'a EditorState, text: &str, lines: &Lines) -> Option<Self> {
        let visible = state.visible_row_range()?;
        let line_height = state.line_height()?;
        // A visible line with a character tells where columns are.
        let char_width = visible.clone().find_map(|line| {
            let start = lines.start(line)?;
            let next = text[start..].chars().next().filter(|c| *c != '\n')?;
            let b = state.range_to_bounds(&(start..start + next.len_utf8()))?;
            (b.size.width > px(0.)).then_some(b.size.width)
        })?;
        Some(Self {
            state,
            visible,
            line_height,
            char_width,
        })
    }

    fn bounds(&self, range: Range<usize>) -> Option<Bounds<Pixels>> {
        self.state.range_to_bounds(&range)
    }
}

/// Where each line of a text starts, to find lines without counting from the top.
struct Lines {
    starts: Vec<usize>,
    len: usize,
}

impl Lines {
    fn of(text: &str) -> Self {
        let starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        Self {
            starts,
            len: text.len(),
        }
    }

    /// The line (from 0) byte `at` is on.
    fn line_of(&self, at: usize) -> usize {
        let at = at.min(self.len);
        self.starts.partition_point(|&s| s <= at) - 1
    }

    /// The byte where `line` starts; `None` past the end.
    fn start(&self, line: usize) -> Option<usize> {
        self.starts.get(line).copied()
    }
}

/// The end of the line byte `at` is on (before its newline).
fn line_end(text: &str, at: usize) -> usize {
    let at = at.min(text.len());
    text[at..].find('\n').map_or(text.len(), |i| at + i)
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` attribute would shadow the standard one.
    use super::{Lines, line_end};

    #[test]
    fn lines_and_their_ends() {
        let text = "select (\n  x\n) from t";
        let lines = Lines::of(text);
        assert_eq!(lines.line_of(0), 0);
        assert_eq!(lines.line_of(8), 0, "the newline ends its line");
        assert_eq!(lines.line_of(text.find('x').unwrap()), 1);
        assert_eq!(lines.line_of(text.len() + 5), 2);
        assert_eq!(lines.start(1), Some(9));
        assert_eq!(lines.start(3), None);
        assert_eq!(line_end(text, 2), 8);
        assert_eq!(line_end(text, text.len()), text.len());
    }
}
