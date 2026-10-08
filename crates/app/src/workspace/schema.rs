//! The schema sidebar: tree, search, favorites and actions on tables.

use super::*;
use crate::ui::{Severity, alert_card, hover_actions};

impl Workspace {
    /// `schema.table`, quoted only where needed.
    pub(super) fn qualified_name(&self, table: &TableInfo) -> Option<String> {
        let dialect = dialect_for(self.active_database()?.driver);
        Some(format!(
            "{}.{}",
            quote_ident(dialect, &table.schema),
            quote_ident(dialect, &table.name)
        ))
    }

    /// The table's name for generated SQL: without the default schema (`public`, or
    /// the MySQL connection's database) and quoted only where needed.
    pub(super) fn sql_name(&self, table: &TableInfo) -> Option<String> {
        let schema = self.active_schema()?;
        match schema.resolve(Some(&table.schema), &table.name) {
            Some(ix) => Some(schema.sql_name(ix)),
            None => self.qualified_name(table),
        }
    }

    pub(super) fn open_table(
        &mut self,
        table: &TableInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(name), Some(db)) = (self.sql_name(table), self.active_database()) else {
            return;
        };
        let sql = select_rows(dialect_for(db.driver), &name, OPEN_ROWS);
        self.run_in_new_tab(&table.name, sql, window, cx);
    }

    pub(super) fn count_table(
        &mut self,
        table: &TableInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = self.sql_name(table) else {
            return;
        };
        self.run_in_new_tab(
            &table.name,
            format!("SELECT COUNT(*) FROM {name}"),
            window,
            cx,
        );
    }

    pub(super) fn run_in_new_tab(
        &mut self,
        title: &str,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = self.add_tab(&sql, window, cx);
        let title: SharedString = title.to_string().into();
        tab.update(cx, |t, cx| {
            t.title = title;
            cx.emit(TabEvent::Run(sql));
        });
    }

    pub(super) fn build_sidebar_rows(&self, search: &str) -> Vec<SidebarRow> {
        let Some(user_id) = self
            .active_user
            .as_ref()
            .filter(|id| self.conns.contains_key(id.as_str()))
        else {
            return Vec::new();
        };
        let schema_ix = self.schema_of(user_id);
        if !search.is_empty() {
            return self.search_rows(&schema_ix, search);
        }
        let mut rows = Vec::new();
        let favorites: Vec<SidebarRow> = self
            .panels
            .schema
            .favorites
            .iter()
            .filter_map(|(schema, name)| {
                let ix = schema_ix.resolve(Some(schema), name)?;
                Some(SidebarRow::Favorite {
                    table: schema_ix.table(ix).clone(),
                })
            })
            .collect();
        if !favorites.is_empty() {
            rows.push(SidebarRow::Heading("Favorites".into()));
            rows.extend(favorites);
            let title = match self.namespace() {
                Namespace::Database => "Databases",
                Namespace::Schema => "Schemas",
            };
            rows.push(SidebarRow::Heading(title.into()));
        }
        // Drivers return tables ordered by schema, then name.
        let schemas: Vec<&[TableInfo]> = schema_ix
            .tables()
            .chunk_by(|a, b| a.schema == b.schema)
            .collect();
        let only_schema = schemas.len() == 1;
        for tables in schemas {
            let schema: SharedString = tables[0].schema.clone().into();
            let expanded = self
                .panels
                .schema
                .open_schemas
                .get(&(user_id.clone(), schema.clone()))
                .copied()
                .unwrap_or(only_schema);
            rows.push(SidebarRow::Schema {
                schema: schema.clone(),
                expanded,
            });
            if !expanded {
                continue;
            }
            let only_kind = tables
                .iter()
                .all(|t| t.kind.group() == tables[0].kind.group());
            for kind in [TableKind::Table, TableKind::View] {
                let items: Vec<&TableInfo> =
                    tables.iter().filter(|t| t.kind.group() == kind).collect();
                if items.is_empty() {
                    continue;
                }
                let expanded = self
                    .panels
                    .schema
                    .open_groups
                    .get(&(user_id.clone(), schema.clone(), kind))
                    .copied()
                    .unwrap_or(only_kind);
                rows.push(SidebarRow::Group {
                    schema: schema.clone(),
                    kind,
                    count: items.len(),
                    expanded,
                });
                if expanded {
                    rows.extend(
                        items
                            .into_iter()
                            .map(|t| SidebarRow::Table { table: t.clone() }),
                    );
                }
            }
        }
        rows
    }

    /// Search results: schemas and tables whose names match, then matching columns.
    /// Case-insensitive; names starting with the text come first.
    pub(super) fn search_rows(&self, schema: &SchemaIndex, search: &str) -> Vec<SidebarRow> {
        let needle = search.to_lowercase();
        let rank = |name: &str| {
            let name = name.to_lowercase();
            if name == needle {
                Some(0)
            } else if name.starts_with(&needle) {
                Some(1)
            } else if name.contains(&needle) {
                Some(2)
            } else {
                None
            }
        };
        let mut schemas: Vec<(u8, SharedString)> = Vec::new();
        for tables in schema.tables().chunk_by(|a, b| a.schema == b.schema) {
            if let Some(r) = rank(&tables[0].schema) {
                schemas.push((r, tables[0].schema.clone().into()));
            }
        }
        let mut tables: Vec<(u8, &TableInfo)> = schema
            .tables()
            .iter()
            .filter_map(|t| Some((rank(&t.name)?, t)))
            .collect();
        let mut columns: Vec<(u8, &ColumnMeta)> = schema
            .info()
            .columns
            .iter()
            .filter_map(|c| Some((rank(&c.name)?, c)))
            .collect();
        // Stable: keeps schema / table order within a rank.
        schemas.sort_by_key(|(r, _)| *r);
        tables.sort_by_key(|(r, _)| *r);
        columns.sort_by_key(|(r, _)| *r);

        let mut rows = Vec::new();
        if !schemas.is_empty() || !tables.is_empty() {
            let title = match self.namespace() {
                Namespace::Database => "Databases and tables",
                Namespace::Schema => "Schemas and tables",
            };
            rows.push(SidebarRow::Heading(title.into()));
            rows.extend(
                schemas
                    .into_iter()
                    .map(|(_, schema)| SidebarRow::SchemaHit { schema }),
            );
            rows.extend(
                tables
                    .into_iter()
                    .map(|(_, t)| SidebarRow::TableHit { table: t.clone() }),
            );
        }
        if !columns.is_empty() {
            rows.push(SidebarRow::Heading("Columns".into()));
            rows.extend(columns.into_iter().filter_map(|(_, c)| {
                let ix = schema.resolve(Some(&c.schema), &c.table)?;
                Some(SidebarRow::ColumnHit {
                    table: schema.table(ix).clone(),
                    column: c.name.clone().into(),
                    data_type: c.data_type.clone().into(),
                })
            }));
        }
        rows
    }

    /// Clears the search and shows `schema` (and `table` in it) in the tree.
    pub(super) fn reveal(
        &mut self,
        schema: SharedString,
        table: Option<&TableInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(user_id) = self.active_user.clone() {
            self.panels
                .schema
                .open_schemas
                .insert((user_id.clone(), schema.clone()), true);
            if let Some(t) = table {
                self.panels
                    .schema
                    .open_groups
                    .insert((user_id, schema.clone(), t.kind), true);
            }
        }
        let target = (schema, table.map(|t| t.name.clone()));
        self.panels.schema.reveal = Some(target.clone());
        self.panels
            .schema
            .search
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.panels.schema.revealed = Some(target);
        cx.notify();
    }

    pub(super) fn is_favorite(&self, table: &TableInfo) -> bool {
        self.panels
            .schema
            .favorites
            .iter()
            .any(|(schema, name)| *schema == table.schema && *name == table.name)
    }

    pub(super) fn toggle_favorite(
        &mut self,
        table: &TableInfo,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(db_id) = self.active_db.clone() else {
            return;
        };
        let on = !self.is_favorite(table);
        if let Err(e) = self
            .data
            .local
            .set_favorite(&db_id, &table.schema, &table.name, on)
        {
            window.push_notification(
                Notification::error(format!("Could not save favorite: {e:#}")),
                cx,
            );
        }
        cx.notify();
    }

    /// Star toggling `table` as a favorite (filled for a favorite). `id` tells the
    /// always shown star of a favorite from the one among the hover actions.
    pub(super) fn favorite_button(
        &self,
        ix: usize,
        table: &TableInfo,
        id: &'static str,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        let favorite = self.is_favorite(table);
        let (icon, tooltip) = if favorite {
            (Lucide::StarFill, "Remove from favorites")
        } else {
            (Lucide::Star, "Add to favorites")
        };
        let table = table.clone();
        div()
            .id((id, ix))
            // Keep the click from also opening the table.
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                Button::new("star")
                    .icon(Icon::new(icon))
                    .ghost()
                    .xsmall()
                    .tooltip(tooltip)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.toggle_favorite(&table, window, cx)
                    })),
            )
    }

    /// A favorite's star, always shown (left of the row count); while the row is
    /// hovered the same star among the hover actions takes its place.
    fn fixed_star(
        &self,
        ix: usize,
        table: &TableInfo,
        group: SharedString,
        cx: &Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        self.is_favorite(table).then(|| {
            div()
                .group_hover(group, |s| s.invisible())
                .child(self.favorite_button(ix, table, "table-favorite", cx))
        })
    }

    pub(super) fn render_sidebar_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let user_id = self.active_user.clone().unwrap_or_default();
        let namespace = self.namespace();
        // Zed-style tree: indentation plus one thin guide line per ancestor level,
        // expansion shown by the folder icon instead of chevrons.
        let guide = theme.sidebar_border;
        let row = |depth: usize| {
            h_flex()
                .id(("sidebar-row", ix))
                .test_support()
                .relative()
                .w_full()
                .h(px(ROW_HEIGHT))
                .pl(px(TREE_PAD + depth as f32 * TREE_INDENT))
                .pr_2()
                .gap_1p5()
                .items_center()
                .text_sm()
                .hover(|s| s.bg(theme.sidebar_accent))
                .cursor_pointer()
                .children((0..depth).map(|level| {
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(TREE_PAD + level as f32 * TREE_INDENT + TREE_ICON / 2.))
                        .w(px(1.))
                        .bg(guide)
                }))
        };
        let folder = |expanded: bool| {
            Icon::new(if expanded {
                IconName::FolderOpen
            } else {
                IconName::FolderClosed
            })
        };

        let row_kind = self.panels.schema.rows[ix].clone();
        let in_favorites = matches!(row_kind, SidebarRow::Favorite { .. });
        match row_kind {
            SidebarRow::Schema { schema, expanded } => {
                let icon: Icon = match namespace {
                    Namespace::Database => Icon::new(Lucide::Database),
                    Namespace::Schema => folder(expanded),
                };
                let key = (user_id, schema.clone());
                let revealed = self.panels.schema.revealed == Some((schema.clone(), None));
                row(0)
                    .when(revealed, |r| r.bg(theme.sidebar_accent))
                    .child(icon.small().text_color(theme.muted_foreground))
                    .child(div().min_w_0().truncate().child(schema))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.panels
                            .schema
                            .open_schemas
                            .insert(key.clone(), !expanded);
                        cx.notify();
                    }))
                    .into_any_element()
            }
            SidebarRow::Group {
                schema,
                kind,
                count,
                expanded,
            } => {
                let label = if kind.is_view() { "Views" } else { "Tables" };
                let key = (user_id, schema, kind);
                row(1)
                    .child(folder(expanded).small().text_color(theme.muted_foreground))
                    .child(div().min_w_0().truncate().child(label))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(count.to_string()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.panels
                            .schema
                            .open_groups
                            .insert(key.clone(), !expanded);
                        cx.notify();
                    }))
                    .into_any_element()
            }
            SidebarRow::Table { table } | SidebarRow::Favorite { table } => {
                let icon = kind_icon(table.kind);
                let info = table_info(&table);
                let estimate = table.row_estimate.map(short_count);
                let name = table.name.clone();
                let group: SharedString = format!("table-row-{ix}").into();
                let weak = cx.entity().downgrade();
                let menu_table = table.clone();
                let suggestions = Button::new(("table-suggest", ix))
                    .icon(Icon::new(Lucide::Lightbulb))
                    .ghost()
                    .xsmall()
                    .tooltip("Suggested queries")
                    .dropdown_menu(move |menu, _, cx| {
                        let Some(ws) = weak.upgrade() else {
                            return menu;
                        };
                        let list = ws.read(cx).table_suggestions(&menu_table);
                        suggestions_menu(menu, &menu_table.name, list, &weak)
                    });
                let revealed = !in_favorites
                    && self.panels.schema.revealed.as_ref().is_some_and(|(s, t)| {
                        *s == table.schema && t.as_ref() == Some(&table.name)
                    });
                let star = self.favorite_button(ix, &table, "table-favorite-hover", cx);
                let fixed_star = self.fixed_star(ix, &table, group.clone(), cx);
                row(if in_favorites { 0 } else { 2 })
                    .when(revealed, |r| r.bg(theme.sidebar_accent))
                    .group(group.clone())
                    .child(icon.small().text_color(theme.muted_foreground))
                    .map(|r| {
                        if in_favorites {
                            r.child(div().min_w_0().truncate().child(name)).child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(table.schema.clone()),
                            )
                        } else {
                            r.child(div().flex_1().min_w_0().truncate().child(name))
                        }
                    })
                    // Left of the row count.
                    .children(fixed_star)
                    .when_some(estimate, |r, estimate| {
                        r.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(estimate),
                        )
                    })
                    // The kind, estimate and comment: wrapped, long comments behind
                    // "Show all".
                    .when_some(info, |r, info| r.hoverable_tooltip(TextCard::builder(info)))
                    .child(
                        hover_actions(group, theme.sidebar_accent)
                            .child(
                                div()
                                    .id(("table-suggest-wrap", ix))
                                    // Keep the click from also opening the table.
                                    .on_click(|_, _, cx| cx.stop_propagation())
                                    .child(suggestions),
                            )
                            .child(star),
                    )
                    .on_click({
                        let table = table.clone();
                        cx.listener(move |this, _, window, cx| this.open_table(&table, window, cx))
                    })
                    .context_menu({
                        let weak = cx.entity().downgrade();
                        move |menu, _, cx| table_menu(menu, &table, &weak, cx)
                    })
                    .into_any_element()
            }
            SidebarRow::Heading(title) => h_flex()
                .h(px(ROW_HEIGHT))
                .px_2()
                .items_end()
                .pb_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(title)
                .into_any_element(),
            SidebarRow::SchemaHit { schema } => {
                let icon = match namespace {
                    Namespace::Database => Icon::new(Lucide::Database),
                    Namespace::Schema => folder(false),
                };
                let target = schema.clone();
                row(0)
                    .child(icon.small().text_color(theme.muted_foreground))
                    .child(div().min_w_0().truncate().child(schema))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.reveal(target.clone(), None, window, cx)
                    }))
                    .into_any_element()
            }
            SidebarRow::TableHit { table } => {
                let icon = kind_icon(table.kind);
                let group: SharedString = format!("table-hit-{ix}").into();
                let star = self.favorite_button(ix, &table, "table-favorite-hover", cx);
                let fixed_star = self.fixed_star(ix, &table, group.clone(), cx);
                row(0)
                    .group(group.clone())
                    .child(icon.small().text_color(theme.muted_foreground))
                    .child(div().min_w_0().truncate().child(table.name.clone()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(table.schema.clone()),
                    )
                    .children(fixed_star)
                    .child(hover_actions(group, theme.sidebar_accent).child(star))
                    // Opens the table; the search stays.
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.open_table(&table, window, cx)),
                    )
                    .into_any_element()
            }
            SidebarRow::ColumnHit {
                table,
                column,
                data_type,
            } => row(0)
                .child(
                    Icon::new(IconName::Minus)
                        .small()
                        .text_color(theme.muted_foreground),
                )
                .child(div().flex_none().child(column))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{}.{} · {data_type}", table.schema, table.name)),
                )
                .on_click(
                    cx.listener(move |this, _, window, cx| this.open_table(&table, window, cx)),
                )
                .into_any_element(),
        }
    }

    /// Placeholder for the sidebar when there is nothing to list yet.
    pub(super) fn render_sidebar_empty(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let message = |text: SharedString| {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let body = v_flex().px_3().py_2().gap_2().items_start();
        let Some(db) = self.active_database() else {
            return Some(
                body.child(message("No databases yet.".into()))
                    .child(
                        Button::new("empty-add-db")
                            .label("Add database")
                            .icon(IconName::Plus)
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_form(FormMode::NewDatabase, None, None, window, cx)
                            })),
                    )
                    .into_any_element(),
            );
        };
        let db_id = db.id.clone();
        let Some(user_id) = self.active_user.clone() else {
            return Some(
                body.child(message("No user for this database.".into()))
                    .child(
                        Button::new("empty-add-user")
                            .label("Add user")
                            .icon(IconName::Plus)
                            .small()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_form(
                                    FormMode::User,
                                    Some(db_id.clone()),
                                    None,
                                    window,
                                    cx,
                                )
                            })),
                    )
                    .into_any_element(),
            );
        };
        let connect = |label: &'static str| {
            Button::new("empty-connect")
                .label(label)
                .small()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.connect(&user_id, cx);
                    this.assign_active_tab(&user_id, window, cx);
                }))
        };
        let no_tables = self.active_schema().is_none_or(|s| s.tables().is_empty());
        match self.active_conn(cx).map(|c| (&c.status, no_tables)) {
            None => Some(
                body.child(message("Not connected.".into()))
                    .child(connect("Connect"))
                    .into_any_element(),
            ),
            Some((ConnStatus::Connecting, _)) => {
                Some(body.child(message("Connecting…".into())).into_any_element())
            }
            Some((ConnStatus::Failed(e), _)) => Some(
                body.child(alert_card(Severity::Error, e.clone(), cx))
                    .child(connect("Retry"))
                    .into_any_element(),
            ),
            Some((ConnStatus::Connected, true)) => Some(
                body.child(message("No tables or views.".into()))
                    .into_any_element(),
            ),
            Some((ConnStatus::Connected, false)) => None,
        }
    }

    pub(super) fn render_schema_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let search = self
            .panels
            .schema
            .search
            .read(cx)
            .value()
            .trim()
            .to_string();
        self.panels.schema.favorites = self
            .active_db
            .as_deref()
            .map(|db| self.data.local.favorites(db))
            .unwrap_or_default();
        self.panels.schema.rows = self.build_sidebar_rows(&search);
        if let Some((schema, table)) = self.panels.schema.reveal.take() {
            let target = self.panels.schema.rows.iter().position(|row| match row {
                SidebarRow::Table { table: t } => {
                    table.as_ref() == Some(&t.name) && t.schema == schema.as_ref()
                }
                SidebarRow::Schema { schema: s, .. } => table.is_none() && *s == schema,
                _ => false,
            });
            if let Some(ix) = target {
                self.panels
                    .schema
                    .scroll
                    .scroll_to_item(ix, ScrollStrategy::Center);
            }
        }
        let has_schema = self.active_conn(cx).is_some()
            && self.active_schema().is_some_and(|s| !s.tables().is_empty());
        let no_hits = !search.is_empty() && self.panels.schema.rows.is_empty();
        let empty = self.render_sidebar_empty(cx);
        let theme = cx.theme();
        let count = self.panels.schema.rows.len();
        let heading = match self.namespace() {
            Namespace::Database => "DATABASES",
            Namespace::Schema => "SCHEMAS",
        };
        let refresh = self
            .active_conn(cx)
            .filter(|c| c.is_connected())
            .map(|c| c.id);
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                h_flex()
                    .h_9()
                    .px_3()
                    .items_center()
                    .justify_between()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(heading)
                    .when_some(refresh, |this, conn| {
                        this.child(
                            Button::new("refresh-schema")
                                .icon(IconName::RotateCw)
                                .ghost()
                                .xsmall()
                                .tooltip("Refresh")
                                .on_click(cx.listener(move |this, _, _, _| {
                                    this.engine.send(Command::RefreshSchema { conn });
                                })),
                        )
                    }),
            )
            .when(has_schema, |this| {
                this.child(
                    div().px_2().pb_2().child(
                        Input::new(&self.panels.schema.search)
                            .small()
                            .cleanable(true),
                    ),
                )
            })
            .children(empty)
            .when(no_hits, |this| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("Nothing matches."),
                )
            })
            .child(
                uniform_list(
                    "sidebar",
                    count,
                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|ix| this.render_sidebar_row(ix, cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.panels.schema.scroll)
                .px_1()
                .flex_1(),
            )
    }
}

pub(super) fn suggestions_menu(
    mut menu: PopupMenu,
    table: &str,
    list: Vec<QuerySuggestion>,
    workspace: &WeakEntity<Workspace>,
) -> PopupMenu {
    let (learned, generated): (Vec<_>, Vec<_>) = list.into_iter().partition(|q| q.learned);
    for (heading, items) in [("Your queries", learned), ("Suggested", generated)] {
        if items.is_empty() {
            continue;
        }
        menu = menu.label(heading);
        for q in items {
            let (ws, table, sql) = (workspace.clone(), table.to_string(), q.sql);
            menu = menu.item(PopupMenuItem::new(q.title).on_click(move |_, window, cx| {
                let _ = ws.update(cx, |ws, cx| ws.open_sql(&table, &sql, window, cx));
            }));
        }
    }
    menu.max_w(px(420.))
}

pub(super) fn table_menu(
    menu: PopupMenu,
    table: &TableInfo,
    workspace: &WeakEntity<Workspace>,
    cx: &App,
) -> PopupMenu {
    let item =
        |label: &str, f: fn(&mut Workspace, &TableInfo, &mut Window, &mut Context<Workspace>)| {
            let (ws, table) = (workspace.clone(), table.clone());
            PopupMenuItem::new(label.to_string()).on_click(move |_, window, cx| {
                let _ = ws.update(cx, |ws, cx| f(ws, &table, window, cx));
            })
        };
    let name = workspace
        .upgrade()
        .and_then(|ws| ws.read(cx).qualified_name(table))
        .unwrap_or_else(|| table.name.clone());
    let copy = |label: &str, text: String| {
        PopupMenuItem::new(label.to_string()).on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
        })
    };
    menu.item(item("Select first 100 rows", Workspace::open_table))
        .item(item("Count rows", Workspace::count_table))
        .separator()
        .item(copy("Copy name", table.name.clone()))
        .item(copy("Copy qualified name", name))
}

fn kind_icon(kind: TableKind) -> Icon {
    if kind.is_view() {
        Icon::new(IconName::Eye)
    } else {
        Icon::new(Lucide::Table)
    }
}

/// What the row's tooltip says: the kind unless it's a plain table or view, the
/// row estimate and the server's comment. `None` when there is nothing to add.
fn table_info(table: &TableInfo) -> Option<SharedString> {
    let mut head = Vec::new();
    if !matches!(table.kind, TableKind::Table | TableKind::View) {
        head.push(table.kind.label().to_string());
    }
    if let Some(n) = table.row_estimate {
        head.push(format!(
            "about {} rows",
            short_count(n).trim_start_matches('~')
        ));
    }
    let lines: Vec<String> = [
        (!head.is_empty()).then(|| head.join(" · ")),
        table.comment.clone(),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!lines.is_empty()).then(|| lines.join("\n").into())
}

/// `~950`, `~12k`, `~1.2M`: a row estimate at a glance.
fn short_count(n: u64) -> String {
    match n {
        0..1_000 => format!("~{n}"),
        1_000..10_000 => format!("~{:.1}k", n as f64 / 1e3).replace(".0k", "k"),
        10_000..1_000_000 => format!("~{}k", n / 1_000),
        1_000_000..10_000_000 => format!("~{:.1}M", n as f64 / 1e6).replace(".0M", "M"),
        _ => format!("~{}M", n / 1_000_000),
    }
}

#[cfg(test)]
mod tests {
    use super::{short_count, table_info};
    use tangul_core::{TableInfo, TableKind};

    #[test]
    fn row_estimates_at_a_glance() {
        assert_eq!(short_count(0), "~0");
        assert_eq!(short_count(950), "~950");
        assert_eq!(short_count(1_000), "~1k");
        assert_eq!(short_count(1_250), "~1.2k");
        assert_eq!(short_count(300_000), "~300k");
        assert_eq!(short_count(1_500_000), "~1.5M");
        assert_eq!(short_count(42_000_000), "~42M");
    }

    #[test]
    fn tooltips_only_say_something_new() {
        let t = |kind, comment: Option<&str>, rows| TableInfo {
            kind,
            comment: comment.map(str::to_string),
            row_estimate: rows,
            ..Default::default()
        };
        assert_eq!(table_info(&t(TableKind::Table, None, None)), None);
        assert_eq!(
            table_info(&t(TableKind::Table, Some("Customer orders"), Some(300_000))).as_deref(),
            Some("about 300k rows\nCustomer orders")
        );
        assert_eq!(
            table_info(&t(TableKind::MaterializedView, None, None)).as_deref(),
            Some("materialized view")
        );
    }
}
