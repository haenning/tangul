//! The filter and sort popover of a result column (the funnel in its header). One
//! panel per result, filled for the column it is opened on. What it offers depends on
//! the column's kind: comparisons and ranges for numbers, text matching for text,
//! true / false for booleans, and a from / to range with a calendar for dates.

use chrono::NaiveDate;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    calendar::{Calendar, CalendarState, Date},
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_assist::refine::{ColumnFilter, ColumnKind, FilterOp, Sort};

use super::ResultView;

pub struct FilterPanel {
    view: WeakEntity<ResultView>,
    column: usize,
    name: SharedString,
    kind: ColumnKind,
    op: FilterOp,
    /// The value, or the lower end of a range.
    value: Entity<InputState>,
    /// The upper end of a range.
    to: Entity<InputState>,
    /// Picks a date range; the inputs stay editable (to add a time, say).
    calendar: Entity<CalendarState>,
    /// The calendar's selection last copied into the inputs.
    synced: Date,
    /// The column has a filter now (so it can be removed).
    filtered: bool,
    /// Current sort of this column: descending or not.
    sort: Option<bool>,
    _subscriptions: Vec<Subscription>,
}

impl FilterPanel {
    pub fn new(view: WeakEntity<ResultView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let value = cx.new(|cx| InputState::new(window, cx).placeholder("Value"));
        let to = cx.new(|cx| InputState::new(window, cx).placeholder("To"));
        let calendar = cx.new(|cx| CalendarState::new(window, cx));
        let on_enter = |this: &mut Self, _, ev: &InputEvent, cx: &mut Context<Self>| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                this.apply(cx);
            }
        };
        let subscriptions = vec![
            cx.subscribe(&value, on_enter),
            cx.subscribe(&to, on_enter),
            cx.observe_in(&calendar, window, |this, calendar, window, cx| {
                let date = calendar.read(cx).date();
                if date != this.synced {
                    this.synced = date;
                    let day = |d: Option<NaiveDate>| d.map(|d| d.to_string()).unwrap_or_default();
                    this.value
                        .update(cx, |i, cx| i.set_value(day(date.start()), window, cx));
                    this.to
                        .update(cx, |i, cx| i.set_value(day(date.end()), window, cx));
                }
            }),
        ];
        Self {
            view,
            column: 0,
            name: SharedString::default(),
            kind: ColumnKind::Text,
            op: FilterOp::Contains,
            value,
            to,
            calendar,
            synced: Date::Range(None, None),
            filtered: false,
            sort: None,
            _subscriptions: subscriptions,
        }
    }

    /// Fills the panel for `column`, from its current filter and sort.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        column: usize,
        name: SharedString,
        kind: ColumnKind,
        filter: Option<ColumnFilter>,
        sort: Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.column = column;
        self.name = name;
        self.kind = kind;
        self.filtered = filter.is_some();
        self.sort = sort;
        let (op, value, to) = match filter {
            // A filter made while the column looked like another kind.
            Some(f) if kind.ops().iter().any(|ops| ops.contains(&f.op)) => (f.op, f.value, f.to),
            _ => (kind.default_op(), String::new(), String::new()),
        };
        self.op = op;
        let day = |v: &str| NaiveDate::parse_from_str(v.get(..10)?, "%Y-%m-%d").ok();
        self.synced = Date::Range(day(&value), day(&to));
        let synced = self.synced;
        self.calendar
            .update(cx, |c, cx| c.set_date(synced, window, cx));
        self.to.update(cx, |i, cx| i.set_value(to, window, cx));
        self.value.update(cx, |input, cx| {
            input.set_value(value, window, cx);
            if !kind.is_date() {
                input.focus(window, cx);
            }
        });
        cx.notify();
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        let value = self.value.read(cx).value().trim().to_string();
        let to = self.to.read(cx).value().trim().to_string();
        let column = self.column;
        let filter = if self.op == FilterOp::Between && value.is_empty() && to.is_empty() {
            None
        } else {
            Some(ColumnFilter {
                column,
                op: self.op,
                value,
                to,
            })
        };
        let _ = self
            .view
            .update(cx, |view, cx| view.set_filter(column, filter, cx));
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        let column = self.column;
        let _ = self
            .view
            .update(cx, |view, cx| view.set_filter(column, None, cx));
    }

    /// Sorts by this column; picking the current direction again removes the sort.
    fn sort(&mut self, descending: bool, cx: &mut Context<Self>) {
        let sort = (self.sort != Some(descending)).then_some(Sort {
            column: self.column,
            descending,
        });
        let _ = self.view.update(cx, |view, cx| view.set_sort(sort, cx));
    }

    /// The value and the upper end as typed (tests).
    #[cfg(test)]
    pub fn inputs(&self, cx: &App) -> (String, String) {
        let text = |i: &Entity<InputState>| i.read(cx).value().to_string();
        (text(&self.value), text(&self.to))
    }

    fn op_button(&self, op: FilterOp, cx: &mut Context<Self>) -> Button {
        Button::new(SharedString::from(format!("op-{op:?}")))
            .label(op.label())
            .xsmall()
            .ghost()
            .selected(self.op == op)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.op = op;
                if op.takes_value() {
                    this.value.update(cx, |input, cx| input.focus(window, cx));
                } else {
                    // Nothing to type: apply right away.
                    this.apply(cx);
                }
                cx.notify();
            }))
    }

    /// The inputs for the chosen operator: one value, or a from / to range (with a
    /// calendar for dates).
    fn render_value(&self, muted: Hsla) -> Option<AnyElement> {
        if !self.op.takes_value() {
            return None;
        }
        if self.op != FilterOp::Between {
            return Some(Input::new(&self.value).small().into_any_element());
        }
        let (from, to) = if self.kind.is_date() {
            ("From (YYYY-MM-DD)", "To, including that day")
        } else {
            ("From", "To")
        };
        Some(
            v_flex()
                .gap_1()
                .when(self.kind.is_date(), |this| {
                    this.child(Calendar::new(&self.calendar).small())
                })
                .child(
                    h_flex()
                        .gap_1()
                        .child(div().flex_1().child(Input::new(&self.value).small()))
                        .child("–")
                        .child(
                            div()
                                .id("filter-to")
                                .test_support()
                                .flex_1()
                                .child(Input::new(&self.to).small()),
                        ),
                )
                .child(
                    div()
                        .text_color(muted)
                        .child(format!("{from} · {to}; either may stay empty")),
                )
                .into_any_element(),
        )
    }
}

impl Render for FilterPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (border, muted) = (cx.theme().border, cx.theme().muted_foreground);
        let op_rows: Vec<Div> = self
            .kind
            .ops()
            .iter()
            .map(|ops| {
                h_flex()
                    .gap_0p5()
                    .flex_wrap()
                    .children(ops.iter().map(|&op| self.op_button(op, cx)))
            })
            .collect();
        let sort_button = |id: &'static str, label: &'static str, icon: IconName, desc: bool| {
            Button::new(id)
                .label(label)
                .icon(Icon::new(icon))
                .xsmall()
                .ghost()
                .selected(self.sort == Some(desc))
                .on_click(cx.listener(move |this, _, _, cx| this.sort(desc, cx)))
        };
        v_flex()
            .w(px(if self.kind.is_date() { 300. } else { 280. }))
            .gap_2()
            .text_xs()
            .child(div().text_color(muted).truncate().child(self.name.clone()))
            .child(
                h_flex()
                    .gap_1()
                    .child(sort_button(
                        "sort-asc",
                        "Ascending",
                        IconName::SortAscending,
                        false,
                    ))
                    .child(sort_button(
                        "sort-desc",
                        "Descending",
                        IconName::SortDescending,
                        true,
                    )),
            )
            .child(div().h(px(1.)).bg(border))
            .children(op_rows)
            .children(self.render_value(muted))
            .child(
                h_flex()
                    .gap_1()
                    .justify_end()
                    .when(self.filtered, |this| {
                        this.child(
                            Button::new("filter-clear")
                                .label("Remove filter")
                                .xsmall()
                                .ghost()
                                .on_click(cx.listener(|this, _, _, cx| this.clear(cx))),
                        )
                    })
                    .when(self.op.takes_value(), |this| {
                        this.child(
                            Button::new("filter-apply")
                                .label("Filter")
                                .xsmall()
                                .primary()
                                .on_click(cx.listener(|this, _, _, cx| this.apply(cx))),
                        )
                    }),
            )
    }
}
