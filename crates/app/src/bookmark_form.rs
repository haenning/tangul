//! Form for naming a bookmarked query. Rendered inside a dialog.

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _,
    checkbox::Checkbox,
    input::{Input, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use tangul_assist::chart::ChartView;
use tangul_assist::guard;

use crate::settings;
use crate::ui::{Severity, alert_card};
use tangul_data::local::Bookmark;
use tangul_data::profiles::new_id;
use tangul_service::schedule;

pub struct BookmarkForm {
    id: Option<String>,
    sql: String,
    database_id: Option<String>,
    all_databases: bool,
    view: Option<ChartView>,
    scheduled: bool,
    /// The SQL only reads, so it may run on a schedule.
    reads_only: bool,
    name: Entity<InputState>,
    error: Option<SharedString>,
}

impl BookmarkForm {
    /// `existing` is updated in place (keeping its id); otherwise a new bookmark for
    /// `database_id` is created. `view`: how the result is shown now, if the form was
    /// opened from a tab; else the bookmark keeps its own.
    pub fn new(
        existing: Option<&Bookmark>,
        sql: String,
        database_id: Option<String>,
        view: Option<Option<ChartView>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let default_name = existing.map_or_else(|| suggest_name(&sql), |b| b.name.clone());
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Name")
                .default_value(default_name)
        });
        window.focus(&name.read(cx).focus_handle(cx), cx);
        let reads_only = !guard::check(&sql).iter().any(|c| c.writes);
        let scheduled = existing.map_or(
            settings::current(cx).monitoring.schedule_new_bookmarks,
            |b| b.scheduled,
        );
        Self {
            id: existing.map(|b| b.id.clone()),
            view: view.unwrap_or_else(|| existing.and_then(|b| b.view)),
            scheduled,
            reads_only,
            all_databases: existing.is_some_and(|b| b.database_id.is_none()),
            database_id: existing.map_or(database_id, |b| b.database_id.clone()),
            sql,
            name,
            error: None,
        }
    }

    pub fn submit(&mut self, cx: &mut Context<Self>) -> Option<Bookmark> {
        let name = self.name.read(cx).value().trim().to_string();
        if name.is_empty() {
            self.error = Some("Name is required".into());
            cx.notify();
            return None;
        }
        Some(Bookmark {
            id: self.id.clone().unwrap_or_else(new_id),
            name,
            sql: self.sql.clone(),
            database_id: if self.all_databases {
                None
            } else {
                self.database_id.clone()
            },
            view: self.view,
            scheduled: self.scheduled && self.can_schedule(),
        })
    }

    /// Only a bookmark that reads, on one database, runs on a schedule.
    fn can_schedule(&self) -> bool {
        self.reads_only && self.database_id.is_some() && !self.all_databases
    }
}

/// First line of the query, shortened, as a starting point for the name.
fn suggest_name(sql: &str) -> String {
    let line = sql
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    let mut name: String = line.chars().take(40).collect();
    if line.chars().count() > 40 {
        name.push('…');
    }
    name
}

impl Render for BookmarkForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .gap_3()
            .child(Input::new(&self.name))
            .child(
                div()
                    .max_h(px(120.))
                    .overflow_hidden()
                    .p_2()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .font_family(theme.mono_font_family.clone())
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(self.sql.clone()),
            )
            .when(self.database_id.is_some(), |this| {
                this.child(
                    Checkbox::new("all-databases")
                        .label("Show for all databases")
                        .checked(self.all_databases)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.all_databases = *checked;
                            cx.notify();
                        })),
                )
            })
            .child({
                let (allowed, interval) = (
                    self.can_schedule(),
                    settings::current(cx).monitoring.interval(),
                );
                let label = format!(
                    "Run about every {} to track time and rows",
                    schedule::human_duration(interval)
                );
                let checkbox = Checkbox::new("scheduled")
                    .label(label)
                    .checked(self.scheduled && allowed)
                    .disabled(!allowed)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.scheduled = *checked;
                        cx.notify();
                    }));
                let why = if !self.reads_only {
                    Some("Only a query that doesn't change data can run on a schedule.")
                } else if !allowed {
                    Some("Only a bookmark for one database can run on a schedule.")
                } else {
                    None
                };
                v_flex().gap_1().child(checkbox).children(why.map(|why| {
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(why)
                }))
            })
            .children(
                self.error
                    .clone()
                    .map(|e| alert_card(Severity::Error, e, cx)),
            )
    }
}
