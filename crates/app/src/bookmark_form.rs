//! Form for naming a bookmarked query. Rendered inside a dialog.

use gpui_kit::component::{
    ActiveTheme as _,
    checkbox::Checkbox,
    input::{Input, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::local::Bookmark;
use crate::store::new_id;

pub struct BookmarkForm {
    id: Option<String>,
    sql: String,
    database_id: Option<String>,
    all_databases: bool,
    name: Entity<InputState>,
    error: Option<SharedString>,
}

impl BookmarkForm {
    /// `existing` is updated in place (keeping its id); otherwise a new bookmark for
    /// `database_id` is created.
    pub fn new(
        existing: Option<&Bookmark>,
        sql: String,
        database_id: Option<String>,
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
        Self {
            id: existing.map(|b| b.id.clone()),
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
        })
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
            .children(
                self.error
                    .clone()
                    .map(|e| div().text_sm().text_color(theme.danger).child(e)),
            )
    }
}
