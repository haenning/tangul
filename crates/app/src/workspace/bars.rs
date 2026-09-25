//! The title bar (database and user menus) and the status bar (connection details,
//! panels, chart button).

use super::*;

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
                .ghost()
                .small()
                .dropdown_menu(move |menu, _, _| {
                    database_menu(menu, &databases, active_db.as_deref(), &weak)
                })
        };

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
        let dot = self.active_conn().map(|c| match c.status {
            ConnStatus::Connected => theme.success,
            ConnStatus::Connecting => theme.warning,
            ConnStatus::Failed(_) => theme.danger,
        });

        h_flex()
            .gap_1()
            .items_center()
            .child(db_menu)
            .when_some(user_menu, |this, user_menu| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground.opacity(0.6))
                        .child("/"),
                )
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
                            "Read-only mode is on: writes ask first. Click to turn off"
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
                                .map(|ws| ws.read(cx).connection_details())
                                .unwrap_or_default();
                            render_details(sections, cx)
                        }),
                )
            })
    }

    /// Zed-style bottom bar: sidebar panels, then (planned) AI and schema-graph
    /// tools; the active query's timing on the right.
    pub(super) fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (query_text, running, chart) = self
            .tabs
            .get(self.active_tab)
            .map(|tab| {
                let t = tab.read(cx);
                let chart = (t.chart(), t.chart_shown());
                (
                    t.status_text(cx),
                    t.is_running(),
                    // File tabs (settings) have no results to chart.
                    t.file_path().is_none().then(|| (tab.clone(), chart)),
                )
            })
            .unwrap_or_default();
        let chart_button = chart.map(|(tab, (data, shown))| {
            chart_button(tab, data, shown, theme.chart_1, theme.muted_foreground)
        });
        let separator = || div().w(px(1.)).h_3p5().mx_1().bg(theme.border);
        let panels = SidebarView::ALL.map(|view| {
            let active = self.sidebar_open && self.sidebar_view == view;
            Button::new(("panel", view as usize))
                .icon(Icon::new(view.icon()))
                .ghost()
                .xsmall()
                .selected(active)
                .tooltip(view.label())
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_panel(view, cx)))
        });
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
            .children(panels)
            .child(separator())
            .child(planned("ai-agent", Lucide::Bot, "Agent via ACP — planned"))
            .child(planned(
                "ai-assist",
                Lucide::Sparkles,
                "AI query help — planned",
            ))
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
            .child(div().w_1())
    }

    /// The status dot's hover card: live state only (status, pool, what the schema
    /// holds); connection settings are in the database dialog.
    pub(super) fn connection_details(&self) -> Vec<(&'static str, Vec<(&'static str, String)>)> {
        let Some(state) = self.active_conn() else {
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
        let tables = state.schema.tables();
        let views = tables.iter().filter(|t| t.kind == TableKind::View).count();
        sections.push((
            "Schema",
            vec![
                ("Tables", (tables.len() - views).to_string()),
                ("Views", views.to_string()),
                (
                    "Foreign keys",
                    state.schema.foreign_keys().len().to_string(),
                ),
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
    button
        .dropdown_menu(move |mut menu, window, cx| {
            let item = |label: String, view: Option<ChartView>| {
                let tab = tab.clone();
                PopupMenuItem::new(label)
                    .icon(Icon::new(style_icon(view.map(|v| v.style))))
                    .checked(shown == view)
                    .on_click(move |_, _, cx| tab.update(cx, |t, cx| t.set_chart_view(view, cx)))
            };
            menu = menu.item(item("Table".into(), None));
            let multi = data.multi_styles();
            if !multi.is_empty() {
                menu = menu.separator().label("All columns");
                for style in multi {
                    let view = ChartView {
                        style,
                        series: None,
                    };
                    menu = menu.item(item(style.label().into(), Some(view)));
                }
            }
            menu = menu.separator().label("Single column");
            for style in data.single_styles() {
                let label = style.single_label();
                if data.series.len() < 2 {
                    let view = ChartView {
                        style,
                        series: Some(0),
                    };
                    menu = menu.item(item(label.into(), Some(view)));
                    continue;
                }
                // Several value columns: pick which one.
                let columns: Vec<(String, ChartView)> = data
                    .series
                    .iter()
                    .enumerate()
                    .map(|(i, s)| {
                        let view = ChartView {
                            style,
                            series: Some(i),
                        };
                        (s.name.clone(), view)
                    })
                    .collect();
                let tab = tab.clone();
                menu = menu.submenu_with_icon(
                    Some(Icon::new(style_icon(Some(style)))),
                    label,
                    window,
                    cx,
                    move |menu, _, _| {
                        columns.iter().fold(menu, |menu, (name, view)| {
                            let (tab, view) = (tab.clone(), *view);
                            menu.item(
                                PopupMenuItem::new(name.clone())
                                    .checked(shown == Some(view))
                                    .on_click(move |_, _, cx| {
                                        tab.update(cx, |t, cx| t.set_chart_view(Some(view), cx))
                                    }),
                            )
                        })
                    },
                );
            }
            menu
        })
        .into_any_element()
}

pub(super) fn style_icon(style: Option<ChartStyle>) -> Lucide {
    match style {
        None => Lucide::Table,
        Some(ChartStyle::Line) => Lucide::ChartLine,
        Some(ChartStyle::Area | ChartStyle::StackedArea) => Lucide::ChartArea,
        Some(ChartStyle::Bars) => Lucide::ChartColumn,
        Some(ChartStyle::StackedBars) => Lucide::ChartColumnStacked,
    }
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
    for (id, name) in databases {
        let (ws, id) = (workspace.clone(), id.clone());
        menu = menu.item(
            PopupMenuItem::new(name.clone())
                .checked(active == Some(id.as_str()))
                .on_click(move |_, window, cx| {
                    let _ = ws.update(cx, |ws, cx| ws.select_database(id.clone(), window, cx));
                }),
        );
    }
    if !databases.is_empty() {
        menu = menu.separator();
    }
    if let Some(id) = active {
        let (ws, id) = (workspace.clone(), id.to_string());
        menu = menu.item(
            PopupMenuItem::new("Edit database…")
                .icon(IconName::Settings)
                .on_click(move |_, window, cx| {
                    let _ = ws.update(cx, |ws, cx| {
                        ws.open_form(FormMode::EditDatabase, Some(id.clone()), None, window, cx)
                    });
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
