//! The pages' building blocks: sections of rows, and what both pages show (the
//! databases' AI settings, the latest tool calls).

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenuItem},
    switch::Switch,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tangul_core::ValueAccess;

use super::{AiSettingsEvent, AiSettingsTab};

/// A titled group of rows.
pub(super) fn section(
    title: &'static str,
    description: Option<&'static str>,
    rows: impl IntoIterator<Item = AnyElement>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    v_flex()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .when_some(description, |this, d| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .whitespace_normal()
                    .child(d),
            )
        })
        .child(
            v_flex()
                .border_1()
                .border_color(theme.border)
                .rounded_md()
                .children(rows.into_iter().enumerate().map(|(i, row)| {
                    div()
                        .when(i > 0, |d| d.border_t_1().border_color(theme.border))
                        .child(row)
                })),
        )
}

/// A row: what it is (and what it means) on the left, its control on the right.
pub(super) fn row(
    label: impl Into<SharedString>,
    description: Option<SharedString>,
    control: impl IntoElement,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .px_3()
        .py_2()
        .gap_4()
        .items_center()
        .justify_between()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().child(label.into()))
                .when_some(description, |this, d| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .whitespace_normal()
                            .child(d),
                    )
                }),
        )
        .child(div().flex_none().child(control))
        .into_any_element()
}

/// A line of muted text in a section.
pub(super) fn note(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    div()
        .px_3()
        .py_2()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .whitespace_normal()
        .child(text.into())
        .into_any_element()
}

impl AiSettingsTab {
    /// The saved databases and what the AI may do with each: the same settings as
    /// the title bar's AI menu and the connection form.
    pub(super) fn render_databases(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let rows: Vec<AnyElement> = if self.state.databases.is_empty() {
            vec![note("No databases yet.", cx)]
        } else {
            self.state
                .databases
                .iter()
                .enumerate()
                .map(|(ix, db)| {
                    let user_label = match &db.ai_user {
                        None => "Your user".to_string(),
                        Some(id) => db
                            .users
                            .iter()
                            .find(|(uid, _)| uid == id)
                            .map_or("Missing user".into(), |(_, name)| name.clone()),
                    };
                    let (users, current, id) =
                        (db.users.clone(), db.ai_user.clone(), db.id.clone());
                    let weak = cx.entity().downgrade();
                    let user_menu = Button::new(("ai-user", ix))
                        .label(user_label)
                        .icon(IconName::ChevronDown)
                        .xsmall()
                        .outline()
                        .dropdown_menu(move |mut menu, _, _| {
                            menu = menu.label("The AI connects as");
                            let choices = std::iter::once((None, "Same as your user".to_string()))
                                .chain(users.iter().map(|(id, n)| (Some(id.clone()), n.clone())));
                            for (user, name) in choices {
                                let (weak, database) = (weak.clone(), id.clone());
                                menu = menu.item(
                                    PopupMenuItem::new(name).checked(current == user).on_click(
                                        move |_, _, cx| {
                                            let (database, user) = (database.clone(), user.clone());
                                            let _ = weak.update(cx, |_, cx| {
                                                cx.emit(AiSettingsEvent::SetAiUser {
                                                    database,
                                                    user,
                                                })
                                            });
                                        },
                                    ),
                                );
                            }
                            menu
                        });
                    let (weak, id, values) = (cx.entity().downgrade(), db.id.clone(), db.values);
                    let values_menu = Button::new(("ai-values", ix))
                        .label(values.label())
                        .icon(IconName::ChevronDown)
                        .xsmall()
                        .outline()
                        .dropdown_menu(move |mut menu, _, _| {
                            menu = menu.label("Value profiles for the AI");
                            for access in ValueAccess::ALL {
                                let (weak, database) = (weak.clone(), id.clone());
                                menu = menu.item(
                                    PopupMenuItem::new(access.label())
                                        .checked(values == access)
                                        .on_click(move |_, _, cx| {
                                            let database = database.clone();
                                            let _ = weak.update(cx, |_, cx| {
                                                cx.emit(AiSettingsEvent::SetValues {
                                                    database,
                                                    values: access,
                                                })
                                            });
                                        }),
                                );
                            }
                            menu
                        });
                    let (tools_id, edit_id) = (db.id.clone(), db.id.clone());
                    v_flex()
                        .px_3()
                        .py_2()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(div().flex_1().text_sm().child(db.name.clone()))
                                .child(
                                    Switch::new(("ai-tools", ix))
                                        .small()
                                        .checked(db.ai_tools)
                                        .label("AI tools")
                                        .tooltip(
                                            "Available to AI tools (the assistant, MCP clients)",
                                        )
                                        .on_click(cx.listener(move |_, on: &bool, _, cx| {
                                            cx.emit(AiSettingsEvent::SetAiTools {
                                                database: tools_id.clone(),
                                                on: *on,
                                            })
                                        })),
                                )
                                .child(
                                    Button::new(("edit-database", ix))
                                        .icon(IconName::Settings)
                                        .ghost()
                                        .xsmall()
                                        .tooltip("Connection settings")
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.emit(AiSettingsEvent::EditDatabase(edit_id.clone()))
                                        })),
                                ),
                        )
                        .when(db.ai_tools, |this| {
                            this.child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .flex_wrap()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Connects as")
                                    .child(user_menu)
                                    .child("Value profiles")
                                    .child(values_menu),
                            )
                        })
                        .when_some(db.warning.clone(), |this, w| {
                            this.child(
                                h_flex()
                                    .gap_2()
                                    .items_start()
                                    .text_xs()
                                    .text_color(theme.danger)
                                    .child(Icon::new(IconName::TriangleAlert).xsmall().mt_0p5())
                                    .child(div().flex_1().whitespace_normal().child(w)),
                            )
                        })
                        .into_any_element()
                })
                .collect()
        };
        section(
            "Databases",
            Some(
                "What AI tools may use, for the assistant and MCP clients alike. The same \
                 settings are in the title bar's AI menu and each connection's settings. A \
                 database user that may only read on the server is the safer choice.",
            ),
            rows,
            cx,
        )
    }

    /// The latest tool calls of `caller` (`assistant`, `mcp`), from the audit log.
    pub(super) fn render_calls(&self, caller: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let calls: Vec<_> = self
            .state
            .recent_calls
            .iter()
            .filter(|c| c.caller == caller)
            .take(20)
            .collect();
        let rows: Vec<AnyElement> = if calls.is_empty() {
            vec![note("No tool calls yet.", cx)]
        } else {
            calls
                .into_iter()
                .map(|c| {
                    let color = match c.outcome.as_str() {
                        "ran" => theme.muted_foreground,
                        "refused" | "failed" => theme.danger,
                        _ => theme.warning,
                    };
                    h_flex()
                        .px_3()
                        .py_1()
                        .gap_3()
                        .text_xs()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(c.target.trim_start_matches("tool:").to_string()),
                        )
                        .child(div().text_color(color).child(c.outcome.clone()))
                        .into_any_element()
                })
                .collect()
        };
        section(
            "Latest tool calls",
            Some("Every call is kept in the audit log."),
            rows,
            cx,
        )
    }
}
