//! The title bar (database and user menus) and the status bar (connection details,
//! panels, chart button).

use super::*;
use crate::result_view::{SetView, chart_menu};
use crate::ui::{
    Severity, USER_INFO, VALUES_INFO, YOUR_USER, alert_tooltip, menu_alert, menu_choice,
    menu_section,
};

impl Workspace {
    /// Zed-style title bar content: database switcher, then user switcher.
    pub(super) fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let weak = cx.entity().downgrade();
        let db = self.active_database();
        let active_db = self.active_db.clone();
        let active_user = self.active_user.clone();

        let databases: Vec<(String, String)> = self
            .data
            .profiles
            .databases
            .iter()
            .map(|d| (d.id.clone(), d.display_name()))
            .collect();
        let db_menu = {
            let weak = weak.clone();
            Button::new("db-switcher")
                .label(db.map_or("No database".into(), |d| d.display_name()))
                .icon(Icon::new(Lucide::Database).text_color(theme.muted_foreground))
                .ghost()
                .small()
                .dropdown_menu(move |menu, _, _| {
                    database_menu(menu, &databases, active_db.as_deref(), &weak)
                })
        };

        // The AI's user: the database's assistant user, or the tab's user.
        let ai_menu = db.map(|db| {
            let users: Vec<(String, String)> = db
                .users
                .iter()
                .map(|u| (u.id.clone(), u.user.clone()))
                .collect();
            let ai_user = db.assistant_user.clone();
            // A chosen user that is gone: the AI has no connection (never yours
            // instead).
            let missing = ai_user.as_deref().filter(|id| db.user(id).is_none());
            let label = match missing {
                Some(_) => "Missing user".into(),
                None => ai_user
                    .as_deref()
                    .or(active_user.as_deref())
                    .and_then(|id| db.user(id))
                    .map_or("No user".into(), |u| u.user.clone()),
            };
            let (db_id, values, weak) = (db.id.clone(), db.ai_values, weak.clone());
            // Schema only (the database's or every caller's ceiling): no AI user, so
            // nothing to warn about; the button says what the AI sees instead.
            let access = Access {
                database: db.ai_access,
                ceiling: self.ai_ceiling(),
            };
            let schema_only = !self.ai_access(db).is_data();
            // How many tables the AI's user may change; `None` until checked.
            let checked = self.knowledge.ai_writable(&db.id);
            let writable = checked.filter(|n| *n > 0);
            let problem = match missing {
                Some(_) => Some(
                    "The AI's user no longer exists, so the AI can't use this database. \
                     Choose another below."
                        .to_string(),
                ),
                None => self.ai_problems.get(&db.id).cloned(),
            };
            let alerts: Vec<String> = problem
                .into_iter()
                .chain(writable.map(ai_user_warning))
                .collect();
            let warning = (!alerts.is_empty() && !schema_only).then(|| alerts.join("\n\n"));
            // One piece of advice: red when the AI's user may write, none when it
            // is read-only on the server, and on your own user, until that is
            // known, what it means.
            let note = (ai_user.is_none() && checked.is_none() && !schema_only)
                .then(|| SAME_USER_NOTE.to_string());
            // The menu's cards: each problem on its own.
            let alerts: Vec<(Severity, String)> = if schema_only {
                Vec::new()
            } else {
                alerts
                    .iter()
                    .map(|a| (Severity::Error, a.clone()))
                    .chain(note.clone().map(|n| (Severity::Warning, n)))
                    .collect()
            };
            let (color, tooltip): (Hsla, SharedString) = match (&warning, &note) {
                (Some(w), _) => (theme.danger, w.clone().into()),
                (None, Some(n)) => (theme.warning, n.clone().into()),
                (None, None) if schema_only => (
                    theme.muted_foreground,
                    AiAccess::Schema.description().into(),
                ),
                (None, None) => (
                    theme.muted_foreground,
                    "The database user the AI connects as".into(),
                ),
            };
            let label = if schema_only {
                "Schema only".to_string()
            } else {
                label
            };
            let button = Button::new("ai-user-switcher")
                .label(label)
                .icon(Icon::new(Lucide::Bot).text_color(color))
                .ghost()
                .small()
                .text_color(color)
                // A long tooltip is the wrapper's (it wraps).
                .when(warning.is_none() && note.is_none(), |b| {
                    b.tooltip(tooltip.clone())
                })
                .dropdown_menu(move |menu, _, _| {
                    ai_menu(
                        menu,
                        &db_id,
                        &users,
                        ai_user.as_deref(),
                        values,
                        access,
                        &alerts,
                        &weak,
                    )
                });
            match (&warning, &note) {
                // Pulses three times when the answer comes, then stays red.
                (Some(_), _) => div()
                    .id("ai-user-warning")
                    .test_support()
                    .tooltip(alert_tooltip(Severity::Error, tooltip))
                    .child(div().child(button).with_animation(
                        ("ai-user-pulse", self.ai_warning_pulse as usize),
                        Animation::new(Duration::from_millis(1800)),
                        |el, t| el.opacity(pulse(t)),
                    ))
                    .into_any_element(),
                (None, Some(_)) => div()
                    .id("ai-user-note")
                    .test_support()
                    .tooltip(alert_tooltip(Severity::Warning, tooltip))
                    .child(button)
                    .into_any_element(),
                (None, None) => button.into_any_element(),
            }
        });

        let user_menu = db.map(|db| {
            let users: Vec<(String, String)> = db
                .users
                .iter()
                .map(|u| (u.id.clone(), u.user.clone()))
                .collect();
            let label = active_user
                .as_deref()
                .and_then(|id| db.user(id))
                .map_or("No user".into(), |u| u.user.clone());
            let db_id = db.id.clone();
            let active_user = active_user.clone();
            let weak = weak.clone();
            Button::new("user-switcher")
                .label(label)
                .icon(IconName::User)
                .ghost()
                .small()
                .text_color(theme.muted_foreground)
                .dropdown_menu(move |menu, _, _| {
                    user_menu(menu, &db_id, &users, active_user.as_deref(), &weak)
                })
        });

        let read_only = self.active_user_read_only(cx);
        let dot = self.active_conn(cx).map(|c| match c.status {
            ConnStatus::Connected => theme.success,
            ConnStatus::Connecting => theme.warning,
            ConnStatus::Failed(_) => theme.danger,
        });

        h_flex()
            .gap_1()
            .items_center()
            .child(db_menu)
            // db / 🤖 AI user | 👤 user · read-only · ●: your user next to its
            // read-only mode.
            .when_some(ai_menu, |this, ai_menu| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground.opacity(0.6))
                        .child("/"),
                )
                .child(ai_menu)
            })
            .when_some(user_menu, |this, user_menu| {
                this.child(div().w(px(1.)).h_3p5().mx_0p5().bg(theme.border))
                    .child(user_menu)
            })
            .when_some(read_only, |this, on| {
                let color = if on {
                    theme.danger
                } else {
                    theme.muted_foreground
                };
                this.child(
                    Button::new("read-only")
                        .icon(
                            Icon::new(if on {
                                Lucide::PencilOff
                            } else {
                                Lucide::Pencil
                            })
                            .text_color(color),
                        )
                        .ghost()
                        .xsmall()
                        .tooltip(if on {
                            "Read-only mode is on: statements that change data are not run. Click to turn off"
                        } else {
                            "Read-only mode is off. Click to turn on"
                        })
                        .on_click(
                            cx.listener(|this, _, window, cx| this.toggle_read_only(window, cx)),
                        ),
                )
            })
            .when_some(dot, |this, dot| {
                let weak = cx.entity().downgrade();
                this.child(
                    HoverCard::new("connection-details")
                        .anchor(Anchor::TopLeft)
                        .open_delay(Duration::from_millis(150))
                        .trigger(
                            // A larger hit area than the dot itself.
                            div()
                                .size_4()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(div().size_1p5().rounded_full().bg(dot)),
                        )
                        .content(move |_, _, cx| {
                            let sections = weak
                                .upgrade()
                                .map(|ws| ws.read(cx).connection_details(cx))
                                .unwrap_or_default();
                            render_details(sections, cx)
                        }),
                )
            })
    }

    /// Zed-style bottom bar: sidebar panels, then (planned) AI and schema-graph
    /// tools; the active query's timing on the right.
    pub(super) fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let button = |panel: Panel, cx: &mut Context<Self>| {
            let shown = self.left.shows(panel) || self.right.shows(panel);
            Button::new(("panel", panel as usize))
                .icon(Icon::new(panel.icon()))
                .ghost()
                .xsmall()
                .selected(shown)
                .tooltip(panel.label())
                .on_click(cx.listener(move |this, _, window, cx| {
                    if panel == Panel::Value {
                        this.release_viewer_focus(window, cx);
                    }
                    this.toggle_panel(panel, cx)
                }))
        };
        let left: Vec<_> = self.left.panels.iter().map(|&p| button(p, cx)).collect();
        // Right: the AI (the assistant, the MCP server), then the value viewer.
        let (assistant, value) = (button(Panel::Assistant, cx), button(Panel::Value, cx));
        let mcp = self.render_mcp_button(cx).into_any_element();
        let theme = cx.theme();
        // File tabs (settings) have no results to time or chart.
        let (query_text, running, chart) = self
            .tabs
            .active_query()
            .map(|tab| {
                let t = tab.read(cx);
                let chart = (t.chart(cx), t.chart_shown(cx));
                (
                    t.status_text(cx),
                    t.is_running(cx),
                    Some((tab.clone(), chart)),
                )
            })
            .unwrap_or_default();
        let chart_button = chart.map(|(tab, (data, shown))| {
            chart_button(
                tab,
                data,
                shown,
                crate::theme::chart_colors(1, cx)[0],
                theme.muted_foreground,
            )
        });
        let separator = || div().w(px(1.)).h_3p5().mx_1().bg(theme.border);

        let planned = |id: &'static str, icon: Lucide, tooltip: &'static str| {
            Button::new(id)
                .icon(Icon::new(icon))
                .ghost()
                .xsmall()
                .disabled(true)
                .tooltip(tooltip)
        };
        h_flex()
            .h_7()
            .px_1()
            .gap_0p5()
            .items_center()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.title_bar)
            .text_xs()
            .text_color(theme.muted_foreground)
            .children(left)
            .child(separator())
            .child(planned(
                "schema-graph",
                Lucide::Network,
                "Schema graph (tables and their relations) — planned",
            ))
            .child(div().flex_1())
            .when(running, |this| this.child(Spinner::new().xsmall()))
            .child(div().pr_1().child(query_text))
            .children(chart_button)
            .child(assistant)
            .child(mcp)
            .child(separator())
            .child(value)
            .child(div().w_1())
    }

    /// The status dot's hover card: live state only (status, pool, what the schema
    /// holds); connection settings are in the database dialog.
    pub(super) fn connection_details(
        &self,
        cx: &App,
    ) -> Vec<(&'static str, Vec<(&'static str, String)>)> {
        let Some(state) = self.active_conn(cx) else {
            return Vec::new();
        };
        let mut sections = Vec::new();
        let status = match &state.status {
            ConnStatus::Connected => "Connected".to_string(),
            ConnStatus::Connecting => "Connecting…".to_string(),
            ConnStatus::Failed(e) => format!("Failed: {e}"),
        };
        sections.push(("Status", vec![("State", status)]));
        let s = state.stats;
        sections.push((
            "Pool",
            vec![
                ("Open", format!("{} of {}", s.open, s.max)),
                ("Idle", s.idle.to_string()),
                ("In transaction", s.pinned.to_string()),
            ],
        ));
        let Some(schema) = self.active_schema() else {
            return sections;
        };
        let tables = schema.tables();
        let views = tables.iter().filter(|t| t.kind.is_view()).count();
        sections.push((
            "Schema",
            vec![
                ("Tables", (tables.len() - views).to_string()),
                ("Views", views.to_string()),
                ("Foreign keys", schema.foreign_keys().len().to_string()),
            ],
        ));
        sections
    }
}

/// Status-bar button of the active tab's result: tinted when the result can be
/// charted, with a menu of the table, charts of all value columns and charts of a
/// single one; the chosen chart's name next to the icon. Disabled, with the reason,
/// otherwise.
pub(super) fn chart_button(
    tab: Entity<QueryTab>,
    data: Result<Rc<ChartData>, SharedString>,
    shown: Option<ChartView>,
    available: Hsla,
    muted: Hsla,
) -> AnyElement {
    let data = match data {
        Ok(data) => data,
        Err(reason) => {
            return Button::new("chart")
                .icon(Icon::new(Lucide::ChartColumn).text_color(muted))
                .ghost()
                .xsmall()
                .disabled(true)
                .tooltip(reason)
                .into_any_element();
        }
    };
    let button = Button::new("chart")
        .icon(Icon::new(style_icon(shown.map(|v| v.style))).text_color(available))
        .xsmall()
        .selected(shown.is_some())
        .when_some(shown, |b, view| b.label(data.view_label(view)));
    let button = if data.grouped && shown.is_none() {
        button.outline().tooltip("Show as chart (grouped query)")
    } else {
        button.ghost().tooltip("Show as chart")
    };
    let set: SetView = Rc::new(move |view, cx| tab.update(cx, |t, cx| t.set_chart_view(view, cx)));
    button
        .dropdown_menu(move |menu, window, cx| {
            chart_menu(menu, &data, shown, set.clone(), window, cx)
        })
        .into_any_element()
}

pub(super) fn render_details(
    sections: Vec<(&'static str, Vec<(&'static str, String)>)>,
    cx: &App,
) -> Div {
    let theme = cx.theme();
    v_flex()
        .w(px(300.))
        .gap_3()
        .text_xs()
        .children(sections.into_iter().map(|(title, rows)| {
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(title.to_uppercase()),
                )
                .children(rows.into_iter().map(|(label, value)| {
                    h_flex()
                        .gap_2()
                        .items_start()
                        .child(
                            div()
                                .w(px(96.))
                                .flex_shrink_0()
                                .text_color(theme.muted_foreground)
                                .child(label),
                        )
                        .child(div().flex_1().min_w_0().child(value))
                }))
        }))
}

pub(super) fn database_menu(
    mut menu: PopupMenu,
    databases: &[(String, String)],
    active: Option<&str>,
    workspace: &WeakEntity<Workspace>,
) -> PopupMenu {
    if !databases.is_empty() {
        menu = menu.label("Databases");
    }
    for (ix, (id, name)) in databases.iter().enumerate() {
        let (ws, remove_ws, id, remove_id) =
            (workspace.clone(), workspace.clone(), id.clone(), id.clone());
        let name = name.clone();
        menu = menu.item(
            PopupMenuItem::element(move |_, _| {
                let (ws, id) = (remove_ws.clone(), remove_id.clone());
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap_6()
                    .child(name.clone())
                    .child(
                        Button::new(("remove-database", ix))
                            .icon(IconName::Delete)
                            .ghost()
                            .xsmall()
                            .tooltip("Remove database…")
                            .on_click(move |_, window, cx| {
                                // Not a click on the row: that would show the database.
                                cx.stop_propagation();
                                window.dispatch_action(Box::new(base::actions::Cancel), cx);
                                let _ = ws.update(cx, |ws, cx| {
                                    ws.ask_to_remove_database(&id, window, cx)
                                });
                            }),
                    )
            })
            .checked(active == Some(id.as_str()))
            .on_click(move |_, window, cx| {
                let _ = ws.update(cx, |ws, cx| ws.select_database(id.clone(), window, cx));
            }),
        );
    }
    if !databases.is_empty() {
        menu = menu.separator();
    }
    if let Some(active) = active {
        let (ws, id) = (workspace.clone(), active.to_string());
        menu = menu.item(
            PopupMenuItem::new("Edit database…")
                .icon(IconName::Settings)
                .on_click(move |_, window, cx| {
                    let _ = ws.update(cx, |ws, cx| {
                        ws.open_form(FormMode::EditDatabase, Some(id.clone()), None, window, cx)
                    });
                }),
        );
        let (ws, id) = (workspace.clone(), active.to_string());
        menu = menu.item(
            PopupMenuItem::new("Database settings file")
                .icon(IconName::File)
                .on_click(move |_, window, cx| {
                    let _ = ws.update(cx, |ws, cx| ws.open_database_settings(&id, window, cx));
                }),
        );
    }
    let ws = workspace.clone();
    menu.item(
        PopupMenuItem::new("Add database…")
            .icon(IconName::Plus)
            .on_click(move |_, window, cx| {
                let _ = ws.update(cx, |ws, cx| {
                    ws.open_form(FormMode::NewDatabase, None, None, window, cx)
                });
            }),
    )
}

/// What the AI may see of a database: its own level and the ceiling of every AI
/// tool.
#[derive(Clone, Copy)]
pub(super) struct Access {
    pub(super) database: AiAccess,
    pub(super) ceiling: AiAccess,
}

impl Access {
    /// What the settings' ceiling leaves of a database that allows data.
    fn limit(self) -> Option<&'static str> {
        (self.database.is_data() && !self.ceiling.is_data())
            .then_some("Schema only for every AI tool (AI settings)")
    }

    /// Whether AI tools may read data from it.
    fn data(self) -> bool {
        self.database.min(self.ceiling).is_data()
    }
}

/// What the AI sees of the database, its database user (the tab's user when none is
/// set) and the column values it may profile. `alerts`: what is wrong, as cards on
/// top.
#[allow(clippy::too_many_arguments)]
pub(super) fn ai_menu(
    mut menu: PopupMenu,
    db_id: &str,
    users: &[(String, String)],
    current: Option<&str>,
    values: ValueAccess,
    access: Access,
    alerts: &[(Severity, String)],
    workspace: &WeakEntity<Workspace>,
) -> PopupMenu {
    for (severity, text) in alerts {
        menu = menu.item(menu_alert(*severity, text.clone()));
    }
    if !alerts.is_empty() {
        menu = menu.separator();
    }
    menu = menu.item(menu_section("AI access", None));
    for level in AiAccess::ALL {
        let (ws, db_id) = (workspace.clone(), db_id.to_string());
        menu = menu.item(
            menu_choice(level.title(), Some(level.description()))
                .checked(access.database == level)
                .on_click(move |_, window, cx| {
                    let _ = ws.update(cx, |ws, cx| ws.set_ai_access(&db_id, level, window, cx));
                }),
        );
    }
    if let Some(limit) = access.limit() {
        menu = menu.item(menu_note(limit));
    }
    let ws = workspace.clone();
    let settings = PopupMenuItem::new("AI settings…")
        .icon(IconName::Settings)
        .on_click(move |_, window, cx| {
            let _ = ws.update(cx, |ws, cx| {
                ws.open_ai_settings(crate::ai_settings::Page::Assistant, window, cx)
            });
        });
    // Schema only: no AI user and no values to choose.
    if !access.data() {
        return menu.separator().item(settings);
    }
    menu = menu
        .separator()
        .item(menu_section("AI connects as", Some(USER_INFO)));
    let choices = std::iter::once((None, "Your user".to_string(), Some(YOUR_USER))).chain(
        users
            .iter()
            .map(|(id, name)| (Some(id.clone()), name.clone(), None)),
    );
    for (id, name, description) in choices {
        let (ws, db_id, chosen) = (workspace.clone(), db_id.to_string(), id.clone());
        menu = menu.item(
            menu_choice(name, description)
                .checked(current == id.as_deref())
                .on_click(move |_, window, cx| {
                    let _ = ws.update(cx, |ws, cx| {
                        ws.set_assistant_user(&db_id, chosen.clone(), window, cx)
                    });
                }),
        );
    }
    menu = menu
        .separator()
        .item(menu_section("Column values for the AI", Some(VALUES_INFO)));
    for access in ValueAccess::ALL {
        let (ws, db_id) = (workspace.clone(), db_id.to_string());
        menu = menu.item(
            menu_choice(access.title(), Some(access.description()))
                .checked(values == access)
                .on_click(move |_, window, cx| {
                    let _ = ws.update(cx, |ws, cx| ws.set_ai_values(&db_id, access, window, cx));
                }),
        );
    }
    menu.separator().item(settings)
}

/// A line of small print in a menu, wrapped to its width.
fn menu_note(text: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_, cx| {
        div()
            .max_w(px(TEXT_WIDTH))
            .whitespace_normal()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(text)
    })
    .disabled(true)
}

/// What the title bar says while the AI connects as your own user.
const SAME_USER_NOTE: &str = "The AI uses your user: it can read what you can, and what \
    it reads goes to the AI model. A user that can only read on the server is safer.";

/// What the title bar says when the AI's user may change or drop tables.
pub(super) fn ai_user_warning(tables: u64) -> String {
    let noun = if tables == 1 { "table" } else { "tables" };
    format!(
        "The AI's user can change or drop {tables} {noun}. Use a user that can only \
         read on the server."
    )
}

/// Opacity over the warning's animation (0 to 1): three dips, ending fully visible.
fn pulse(t: f32) -> f32 {
    0.35 + 0.65 * (0.5 + 0.5 * (t * 3.0 * std::f32::consts::TAU).cos())
}

pub(super) fn user_menu(
    mut menu: PopupMenu,
    db_id: &str,
    users: &[(String, String)],
    active: Option<&str>,
    workspace: &WeakEntity<Workspace>,
) -> PopupMenu {
    if !users.is_empty() {
        menu = menu.label("Users");
    }
    for (id, name) in users {
        let (ws, db_id, id) = (workspace.clone(), db_id.to_string(), id.clone());
        menu = menu.item(
            PopupMenuItem::new(name.clone())
                .checked(active == Some(id.as_str()))
                .on_click(move |_, window, cx| {
                    let _ = ws.update(cx, |ws, cx| {
                        ws.select_user(db_id.clone(), id.clone(), window, cx)
                    });
                }),
        );
    }
    if !users.is_empty() {
        menu = menu.separator();
    }
    if let Some(id) = active {
        let (ws, db_id, id) = (workspace.clone(), db_id.to_string(), id.to_string());
        menu = menu.item(
            PopupMenuItem::new("Edit user…")
                .icon(IconName::Settings)
                .on_click(move |_, window, cx| {
                    let _ = ws.update(cx, |ws, cx| {
                        ws.open_form(
                            FormMode::User,
                            Some(db_id.clone()),
                            Some(id.clone()),
                            window,
                            cx,
                        )
                    });
                }),
        );
    }
    let (ws, db_id) = (workspace.clone(), db_id.to_string());
    menu.item(
        PopupMenuItem::new("Add user…")
            .icon(IconName::Plus)
            .on_click(move |_, window, cx| {
                let _ = ws.update(cx, |ws, cx| {
                    ws.open_form(FormMode::User, Some(db_id.clone()), None, window, cx)
                });
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::{ai_user_warning, pulse};

    #[test]
    fn the_warning_and_its_pulse() {
        assert_eq!(
            ai_user_warning(1),
            "The AI's user can change or drop 1 table. Use a user that can only read on \
             the server."
        );
        assert!(ai_user_warning(3).contains("3 tables"));
        // Starts and ends fully visible, dims in between.
        assert!((pulse(0.0) - 1.0).abs() < 1e-5 && (pulse(1.0) - 1.0).abs() < 1e-5);
        assert!(pulse(1.0 / 6.0) < 0.4);
    }
}
