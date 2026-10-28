//! The value viewer (one per query tab): the inspected cell (A) in full and, with a
//! second cell (B), how the two differ. It keeps copies of the values, so they stay
//! after the result changes. Knows nothing about sidebars or tabs; the workspace
//! feeds it cells and acts on its [`ValueEvent`]s.

use gpui_kit::base::input::{TextDecoration, TextDecorationCollection};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Editor, EditorState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_assist::refine::{ColumnFilter, FilterOp, Refine, distinct_sql, refined_sql};
use tangul_assist::value::{self, Granularity, ValueKind};
use tangul_core::TabId;

use crate::assets::Lucide;
use crate::result_view::CellValue;

/// Rows the "same / different / distinct" queries return at most.
const ROWS_LIMIT: usize = 100;

/// A picked cell and where it is from.
#[derive(Clone)]
pub struct Picked {
    pub tab: TabId,
    /// E.g. "Query 3 · email · row 12".
    pub source: SharedString,
    pub cell: CellValue,
}

pub enum ValueEvent {
    /// B was dropped: the grid showing it removes its mark.
    ClearCompared(TabId),
    /// Open (and run) `sql` in a new tab on the connection of `tab`.
    OpenRows {
        tab: TabId,
        title: String,
        sql: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    SideBySide,
    Inline,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowsQuery {
    Same,
    Different,
    Distinct,
}

/// A read-only editor and its highlights.
struct Viewer {
    state: Entity<EditorState>,
    marks: TextDecorationCollection,
}

impl Viewer {
    fn new(window: &mut Window, cx: &mut Context<ValuePanel>) -> Self {
        let state = cx.new(|cx| {
            let mut editor = EditorState::new(window, cx)
                .language("json")
                .line_number(true)
                .folding(true)
                .soft_wrap(true);
            editor.set_readonly(true, cx);
            editor
        });
        let marks = state.update(cx, |e, cx| e.create_decorations_collection(Vec::new(), cx));
        Self { state, marks }
    }

    fn show(
        &self,
        text: &str,
        json: bool,
        marks: Vec<TextDecoration>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.state.update(cx, |e, cx| {
            e.set_highlighter(if json { "json" } else { "text" }, cx);
            e.set_value(text.to_string(), window, cx);
        });
        self.marks.set(marks, cx);
    }

    fn render(&self) -> impl IntoElement {
        Editor::new(&self.state).h_full().bordered(false)
    }
}

pub struct ValuePanel {
    a: Option<Picked>,
    b: Option<Picked>,
    layout: Layout,
    /// A is the original (A → B); otherwise B is.
    a_first: bool,
    /// Compare JSON with sorted keys and fixed formatting.
    normalize: bool,
    granularity: Granularity,
    /// The single value, or the original side by side.
    left: Viewer,
    right: Viewer,
    inline: Viewer,
    /// Places that differ between A and B.
    changes: usize,
}

impl EventEmitter<ValueEvent> for ValuePanel {}

impl ValuePanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            a: None,
            b: None,
            layout: Layout::SideBySide,
            a_first: true,
            normalize: true,
            granularity: Granularity::Line,
            left: Viewer::new(window, cx),
            right: Viewer::new(window, cx),
            inline: Viewer::new(window, cx),
            changes: 0,
        }
    }

    /// Shows `picked` as A. The tab A was in before, if it was another one.
    pub fn set_a(
        &mut self,
        picked: Picked,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<TabId> {
        let old = self.a.replace(picked).map(|p| p.tab);
        self.refresh(window, cx);
        old.filter(|&t| Some(t) != self.a.as_ref().map(|p| p.tab))
    }

    /// Compares A with `picked` (B). The tab B was in before, if it was another one.
    pub fn set_b(
        &mut self,
        picked: Picked,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<TabId> {
        let old = self.b.replace(picked).map(|p| p.tab);
        self.refresh(window, cx);
        old.filter(|&t| Some(t) != self.b.as_ref().map(|p| p.tab))
    }

    #[cfg(test)]
    pub fn a(&self) -> Option<&Picked> {
        self.a.as_ref()
    }

    #[cfg(test)]
    pub fn b(&self) -> Option<&Picked> {
        self.b.as_ref()
    }

    /// Places that differ between A and B as shown now.
    #[cfg(test)]
    pub fn changes(&self) -> usize {
        self.changes
    }

    fn clear_b(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(b) = self.b.take() {
            cx.emit(ValueEvent::ClearCompared(b.tab));
        }
        self.refresh(window, cx);
    }

    fn comparing(&self) -> bool {
        self.a.is_some() && self.b.is_some()
    }

    /// Both values are JSON: normalizing applies.
    fn both_json(&self) -> bool {
        let json = |p: &Option<Picked>| {
            p.as_ref()
                .is_some_and(|p| ValueKind::of(p.cell.value.as_deref()) == ValueKind::Json)
        };
        json(&self.a) && json(&self.b)
    }

    /// Puts the values into the editors, with the differences marked.
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let theme = cx.theme();
        let (removed, added) = (theme.danger.opacity(0.25), theme.success.opacity(0.25));
        let marks = |ranges: &[std::ops::Range<usize>], color: Hsla| -> Vec<TextDecoration> {
            ranges
                .iter()
                .map(|r| {
                    TextDecoration::new(
                        r.clone(),
                        HighlightStyle {
                            background_color: Some(color),
                            ..Default::default()
                        },
                    )
                })
                .collect()
        };
        self.changes = 0;
        match (&self.a, &self.b) {
            (Some(a), Some(b)) => {
                let (old, new) = if self.a_first { (a, b) } else { (b, a) };
                let json = self.both_json();
                let text = |p: &Picked| match (&p.cell.value, self.normalize && json) {
                    (Some(v), true) => value::normalized(v),
                    _ => shown(&p.cell),
                };
                let (old, new) = (text(old), text(new));
                match self.layout {
                    Layout::SideBySide => {
                        let d = value::diff(&old, &new, self.granularity);
                        self.changes = d.changes;
                        self.left
                            .show(&old, json, marks(&d.removed, removed), window, cx);
                        self.right
                            .show(&new, json, marks(&d.added, added), window, cx);
                    }
                    Layout::Inline => {
                        let i = value::inline(&old, &new, self.granularity);
                        self.changes = i.changes;
                        let mut all = marks(&i.removed, removed);
                        all.extend(marks(&i.added, added));
                        // Line prefixes aren't JSON; words keep it readable as such.
                        let highlight = json && self.granularity == Granularity::Word;
                        self.inline.show(&i.text, highlight, all, window, cx);
                    }
                }
            }
            (Some(one), None) | (None, Some(one)) => {
                let json = ValueKind::of(one.cell.value.as_deref()) == ValueKind::Json;
                self.left
                    .show(&shown(&one.cell), json, Vec::new(), window, cx);
            }
            (None, None) => {}
        }
        cx.notify();
    }

    /// Opens the rows of A's result that have A's value, don't have it, or the
    /// column's distinct values.
    fn open_rows(&mut self, query: RowsQuery, cx: &mut Context<Self>) {
        let Some(a) = &self.a else { return };
        let Some((title, sql)) = rows_sql(&a.cell, query) else {
            return;
        };
        cx.emit(ValueEvent::OpenRows {
            tab: a.tab,
            title,
            sql,
        });
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        v_flex()
            .id("value-empty")
            .test_support()
            .size_full()
            .p_6()
            .gap_3()
            .items_center()
            .justify_center()
            .text_center()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(Icon::new(Lucide::Braces).size_6())
            .child(
                div()
                    .text_color(theme.foreground)
                    .child("Select a value to see it here"),
            )
            .child("Double-click a cell in a result, or select it and press Space.")
            .child("⌥-click a second cell to compare the two.")
            .into_any_element()
    }

    /// "A" or "B", where the value is from, what it is, and a way to copy it.
    fn render_header(
        &self,
        label: &'static str,
        picked: &Picked,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let color = if label == "A" {
            theme.info
        } else {
            theme.warning
        };
        let value = picked.cell.value.clone();
        let kind = ValueKind::of(value.as_deref());
        let size = value
            .as_ref()
            .map(|v| format!(" · {} chars", v.chars().count()))
            .unwrap_or_default();
        let kind = match &picked.cell.data_type {
            Some(t) => format!("{} ({t}){size}", kind.label()),
            None => format!("{}{size}", kind.label()),
        };
        h_flex()
            .id(("value-header", label.len() + (label == "B") as usize))
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .child(
                div()
                    .px_1p5()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(color)
                    .bg(color.opacity(0.15))
                    .text_color(color)
                    .child(label),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().truncate().child(picked.source.clone()))
                    .child(div().text_color(theme.muted_foreground).child(kind)),
            )
            .child(
                Button::new(("copy-value", (label == "B") as usize))
                    .icon(IconName::Copy)
                    .xsmall()
                    .ghost()
                    .tooltip("Copy value")
                    .disabled(value.is_none())
                    .on_click(move |_, _, cx| {
                        if let Some(v) = &value {
                            cx.write_to_clipboard(ClipboardItem::new_string(v.clone()));
                        }
                    }),
            )
            .when(label == "B", |this| {
                this.child(
                    Button::new("clear-compared")
                        .icon(IconName::Close)
                        .xsmall()
                        .ghost()
                        .tooltip("Stop comparing")
                        .on_click(cx.listener(|this, _, window, cx| this.clear_b(window, cx))),
                )
            })
    }

    /// Where B goes before there is one: a faint "B" and how to pick it.
    fn render_b_hint(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .id("value-b-hint")
            .test_support()
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .px_1p5()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.warning.opacity(0.35))
                    .text_color(theme.warning.opacity(0.5))
                    .child("B"),
            )
            .child("⌥ + click another cell to compare it with A")
    }

    /// A and B are from columns of different names: usually a misclick.
    fn render_column_warning(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (a, b) = (self.a.as_ref()?, self.b.as_ref()?);
        if a.cell.column == b.cell.column {
            return None;
        }
        let theme = cx.theme();
        Some(
            h_flex()
                .id("compare-columns-warning")
                .test_support()
                .px_2()
                .py_1()
                .gap_1p5()
                .items_center()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.warning.opacity(0.12))
                .text_xs()
                .text_color(theme.warning)
                .child(Icon::new(IconName::TriangleAlert).xsmall())
                .child(format!(
                    "Comparing different columns: {} and {}",
                    a.cell.column, b.cell.column
                )),
        )
    }

    /// The compare options: layout, base, normalizing, granularity, changes.
    fn render_compare_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let option = |id: &'static str, label: &'static str, selected: bool| {
            Button::new(id)
                .label(label)
                .xsmall()
                .ghost()
                .selected(selected)
        };
        let changes = match self.changes {
            0 => "Identical".to_string(),
            1 => "1 change".to_string(),
            n => format!("{n} changes"),
        };
        let json = self.both_json();
        h_flex()
            .px_2()
            .py_1()
            .gap_1()
            .flex_wrap()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .child(
                option(
                    "compare-side",
                    "Side by side",
                    self.layout == Layout::SideBySide,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.layout = Layout::SideBySide;
                    this.refresh(window, cx);
                })),
            )
            .child(
                option("compare-inline", "Inline", self.layout == Layout::Inline).on_click(
                    cx.listener(|this, _, window, cx| {
                        this.layout = Layout::Inline;
                        this.refresh(window, cx);
                    }),
                ),
            )
            .child(div().w(px(1.)).h_3p5().mx_1().bg(theme.border))
            .child(
                Button::new("compare-base")
                    .icon(Icon::new(Lucide::ArrowRightLeft))
                    .label(if self.a_first { "A → B" } else { "B → A" })
                    .xsmall()
                    .ghost()
                    .tooltip("Which value is the original")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.a_first = !this.a_first;
                        this.refresh(window, cx);
                    })),
            )
            .child(div().w(px(1.)).h_3p5().mx_1().bg(theme.border))
            .child(
                option(
                    "compare-lines",
                    "Lines",
                    self.granularity == Granularity::Line,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.granularity = Granularity::Line;
                    this.refresh(window, cx);
                })),
            )
            .child(
                option(
                    "compare-words",
                    "Words",
                    self.granularity == Granularity::Word,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.granularity = Granularity::Word;
                    this.refresh(window, cx);
                })),
            )
            .when(json, |this| {
                this.child(div().w(px(1.)).h_3p5().mx_1().bg(theme.border))
                    .child(
                        option("compare-normalize", "Ignore key order", self.normalize)
                            .tooltip("Compare JSON with sorted keys and the same formatting")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.normalize = !this.normalize;
                                this.refresh(window, cx);
                            })),
                    )
            })
            .child(div().flex_1())
            .child(
                div()
                    .id("compare-changes")
                    .test_support()
                    .text_color(theme.muted_foreground)
                    .child(changes),
            )
    }

    /// Rows of A's result by A's value.
    fn render_rows_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let queryable = self.a.as_ref().is_some_and(|a| a.cell.rows.is_some());
        let why = "Only for results of a single SELECT";
        let button =
            |id: &'static str, icon: Lucide, label: &'static str, tip: &'static str, query| {
                Button::new(id)
                    .icon(Icon::new(icon))
                    .label(label)
                    .xsmall()
                    .ghost()
                    .disabled(!queryable)
                    .tooltip(if queryable { tip } else { why })
                    .on_click(cx.listener(move |this, _, _, cx| this.open_rows(query, cx)))
            };
        h_flex()
            .px_2()
            .py_1()
            .gap_1()
            .flex_wrap()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .child(
                div()
                    .text_color(theme.muted_foreground)
                    .mr_1()
                    .child("Rows"),
            )
            .child(button(
                "rows-same",
                Lucide::Equal,
                "Same value",
                "Rows of this result with A's value in this column (up to 100), in a new tab",
                RowsQuery::Same,
            ))
            .child(button(
                "rows-different",
                Lucide::EqualNot,
                "Different value",
                "Rows of this result with another value in this column, NULL included (up to 100)",
                RowsQuery::Different,
            ))
            .child(button(
                "rows-distinct",
                Lucide::Rows3,
                "Distinct values",
                "The column's distinct values with their counts (up to 100)",
                RowsQuery::Distinct,
            ))
    }
}

impl Render for ValuePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (Some(first), second) = (self.a.clone().or(self.b.clone()), self.b.clone()) else {
            return self.render_empty(cx);
        };
        let theme = cx.theme();
        let comparing = self.comparing();
        let body = if !comparing {
            div()
                .flex_1()
                .min_h_0()
                .child(self.left.render())
                .into_any_element()
        } else if self.layout == Layout::Inline {
            div()
                .flex_1()
                .min_h_0()
                .child(self.inline.render())
                .into_any_element()
        } else {
            let (l, r) = if self.a_first { ("A", "B") } else { ("B", "A") };
            let side = |label: &'static str, viewer: &Viewer| {
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .px_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(label),
                    )
                    .child(div().flex_1().min_h_0().child(viewer.render()))
            };
            h_flex()
                .flex_1()
                .min_h_0()
                .items_stretch()
                .child(side(l, &self.left))
                .child(div().w(px(1.)).h_full().bg(theme.border))
                .child(side(r, &self.right))
                .into_any_element()
        };
        let a_label = if self.a.is_some() { "A" } else { "B" };
        v_flex()
            .id("value-panel")
            .test_support()
            .size_full()
            .child(self.render_header(a_label, &first, cx))
            .when(self.a.is_some() && self.b.is_none(), |this| {
                this.child(self.render_b_hint(cx))
            })
            .when_some(second.filter(|_| comparing), |this, b| {
                this.children(self.render_column_warning(cx))
                    .child(self.render_header("B", &b, cx))
                    .child(self.render_compare_bar(cx))
            })
            .when(self.a.is_some(), |this| {
                this.child(self.render_rows_bar(cx))
            })
            .child(body)
            .into_any_element()
    }
}

/// A value as the viewer shows it: JSON indented, NULL spelled out.
fn shown(cell: &CellValue) -> String {
    match &cell.value {
        None => "NULL".to_string(),
        Some(v) => value::pretty(v),
    }
}

/// Title and SQL of a rows query on `cell`'s result, if it can be queried again.
fn rows_sql(cell: &CellValue, query: RowsQuery) -> Option<(String, String)> {
    let src = cell.rows.as_ref()?;
    let column = &cell.column;
    if query == RowsQuery::Distinct {
        let sql = distinct_sql(&src.base, &src.columns, src.column, src.dialect, ROWS_LIMIT);
        return Some((format!("{column}: distinct values"), sql));
    }
    let (op, title) = match (query, &cell.value) {
        (RowsQuery::Same, None) => (FilterOp::IsNull, format!("{column} is NULL")),
        (RowsQuery::Same, Some(_)) => (FilterOp::Eq, format!("{column} = A")),
        (_, None) => (FilterOp::NotNull, format!("{column} is not NULL")),
        (_, Some(_)) => (FilterOp::Differs, format!("{column} ≠ A")),
    };
    let refine = Refine {
        filters: vec![ColumnFilter {
            column: src.column,
            op,
            value: cell.value.clone().unwrap_or_default(),
            to: String::new(),
        }],
        sort: None,
    };
    let sql = refined_sql(
        &src.base,
        &src.columns,
        &src.kinds,
        &refine,
        src.dialect,
        0,
        ROWS_LIMIT,
    );
    Some((title, sql))
}

#[cfg(test)]
mod tests {
    use tangul_assist::dialect::dialect_for;
    use tangul_assist::refine::ColumnKind;
    use tangul_core::DriverKind;

    use super::{RowsQuery, rows_sql};
    use crate::result_view::{CellValue, RowsSource};

    fn cell(value: Option<&str>, queryable: bool) -> CellValue {
        CellValue {
            column: "email".into(),
            row: 1,
            value: value.map(str::to_string),
            data_type: None,
            rows: queryable.then(|| RowsSource {
                base: "select id, email from users".into(),
                columns: vec!["id".into(), "email".into()],
                kinds: vec![ColumnKind::Number, ColumnKind::Text],
                column: 1,
                dialect: dialect_for(DriverKind::Postgres),
            }),
        }
    }

    #[test]
    fn rows_by_the_value() {
        let a = cell(Some("a@x.io"), true);
        let (title, sql) = rows_sql(&a, RowsQuery::Same).unwrap();
        assert_eq!(title, "email = A");
        assert!(
            sql.contains("WHERE tangul_q.email = 'a@x.io'") && sql.ends_with("LIMIT 100"),
            "{sql}"
        );
        let (_, sql) = rows_sql(&a, RowsQuery::Different).unwrap();
        assert!(
            sql.contains("(tangul_q.email <> 'a@x.io' OR tangul_q.email IS NULL)"),
            "{sql}"
        );
        let (title, sql) = rows_sql(&a, RowsQuery::Distinct).unwrap();
        assert_eq!(title, "email: distinct values");
        assert!(sql.contains("GROUP BY tangul_q.email"), "{sql}");
    }

    #[test]
    fn rows_by_null_and_unqueryable_results() {
        let null = cell(None, true);
        assert!(
            rows_sql(&null, RowsQuery::Same)
                .unwrap()
                .1
                .contains("email IS NULL")
        );
        assert!(
            rows_sql(&null, RowsQuery::Different)
                .unwrap()
                .1
                .contains("email IS NOT NULL")
        );
        assert!(rows_sql(&cell(Some("a"), false), RowsQuery::Same).is_none());
    }
}
