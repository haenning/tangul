//! Bookmarks: the dialog, saving, deleting, opening, and the sidebar panel.

use super::*;

impl Workspace {
    /// Asks for a name and saves `sql` as a bookmark (updating `existing` if set).
    /// `tab` gets linked to the bookmark, so saving again from it updates it.
    pub(super) fn open_bookmark_dialog(
        &mut self,
        existing: Option<String>,
        sql: String,
        tab: Option<Entity<QueryTab>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = existing.and_then(|id| self.data.local.bookmark(&id));
        let title = if existing.is_some() {
            "Update bookmark"
        } else {
            "Bookmark query"
        };
        let db_id = self.active_db.clone();
        // From a tab: the bookmark shows its result the way the tab does now.
        let view = tab.as_ref().map(|t| t.read(cx).chart_shown(cx));
        let form = cx.new(|cx| BookmarkForm::new(existing.as_ref(), sql, db_id, view, window, cx));
        let workspace = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let save = {
                let (form, workspace, tab) = (form.clone(), workspace.clone(), tab.clone());
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let Some(bookmark) = form.update(cx, |f, cx| f.submit(cx)) else {
                        return;
                    };
                    if let Some(tab) = &tab {
                        tab.update(cx, |t, cx| {
                            t.bookmark_id = Some(bookmark.id.clone());
                            t.bookmark_hint = None;
                            t.title = bookmark.name.clone().into();
                            cx.notify();
                        });
                    }
                    let _ = workspace.update(cx, |ws, cx| ws.save_bookmark(bookmark, window, cx));
                    window.close_dialog(cx);
                }
            };
            dialog.title(title).w(px(460.)).child(form.clone()).footer(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .ghost()
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(Button::new("save").label("Save").primary().on_click(save)),
            )
        });
    }

    pub(super) fn save_bookmark(
        &mut self,
        mut bookmark: Bookmark,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Passwords (`CREATE ROLE … PASSWORD '…'`) are never kept.
        bookmark.sql = tangul_assist::redact::redact(&bookmark.sql).into_owned();
        let title: SharedString = bookmark.name.clone().into();
        for tab in self.all_query_tabs() {
            if tab.read(cx).bookmark_id.as_deref() == Some(bookmark.id.as_str()) {
                tab.update(cx, |t, _| t.title = title.clone());
            }
        }
        let fingerprint = fingerprint(&bookmark.sql);
        if let Err(e) = self.data.local.upsert_bookmark(&bookmark, &fingerprint) {
            window.push_notification(
                Notification::error(format!("Could not save bookmark: {e:#}")),
                cx,
            );
        }
        self.refresh_ai_bookmarks();
        cx.notify();
    }

    pub(super) fn delete_bookmark(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(e) = self.data.delete_bookmark(id) {
            window.push_notification(
                Notification::error(format!("Could not delete bookmark: {e:#}")),
                cx,
            );
        }
        self.refresh_ai_bookmarks();
        self.check_board_links(cx);
        for tab in self.all_query_tabs() {
            if tab.read(cx).bookmark_id.as_deref() == Some(id) {
                tab.update(cx, |t, _| t.bookmark_id = None);
            }
        }
        cx.notify();
    }

    /// Opens a bookmark in a new tab, optionally running it right away.
    pub(super) fn open_bookmark(
        &mut self,
        id: &str,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(bookmark) = self.data.local.bookmark(id) else {
            return;
        };
        let tab = self.add_tab(&bookmark.sql, window, cx);
        tab.update(cx, |t, cx| {
            t.title = bookmark.name.clone().into();
            t.bookmark_id = Some(bookmark.id.clone());
            t.set_chart_view(bookmark.view, cx);
            if run && t.conn.is_some() {
                cx.emit(TabEvent::Run(bookmark.sql.clone()));
            }
        });
    }

    pub(super) fn render_bookmarks_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let search = self
            .panels
            .bookmark_search
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let all: Vec<Bookmark> = self.data.local.bookmarks_for(self.active_db.as_deref());
        let total = all.len();
        let bookmarks: Vec<Bookmark> = all
            .into_iter()
            .filter(|b| {
                search.is_empty()
                    || b.name.to_lowercase().contains(&search)
                    || b.sql.to_lowercase().contains(&search)
            })
            .collect();
        let count = bookmarks.len();
        let empty = match (total, count) {
            (0, _) => Some("No bookmarks yet. Press ⌘D in a query tab to save one."),
            (_, 0) => Some("No matching bookmarks."),
            _ => None,
        };
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                h_flex()
                    .h_9()
                    .px_3()
                    .items_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("BOOKMARKS"),
            )
            .when(total > 0, |this| {
                this.child(
                    div().px_2().pb_2().child(
                        Input::new(&self.panels.bookmark_search)
                            .small()
                            .cleanable(true),
                    ),
                )
            })
            .children(empty.map(|text| {
                div()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(text)
            }))
            .child(
                uniform_list(
                    "bookmarks",
                    count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|ix| this.render_bookmark_row(ix, &bookmarks[ix], cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .px_1()
                .flex_1(),
            )
    }

    pub(super) fn render_bookmark_row(
        &self,
        ix: usize,
        bookmark: &Bookmark,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let preview: String = bookmark
            .sql
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let global = bookmark.database_id.is_none();
        let (open_id, run_id, edit_id, delete_id) = (
            bookmark.id.clone(),
            bookmark.id.clone(),
            bookmark.id.clone(),
            bookmark.id.clone(),
        );
        let board_id = bookmark.id.clone();
        let edit_sql = bookmark.sql.clone();
        let metrics_id = bookmark.id.clone();
        let group: SharedString = format!("bookmark-{ix}").into();
        let scheduled = schedule::schedulable(bookmark).then(|| {
            let every = settings::current(cx).monitoring.interval();
            format!("Runs about every {}", schedule::human_duration(every))
        });
        let actions = h_flex()
            .gap_0p5()
            .invisible()
            .group_hover(group.clone(), |s| s.visible())
            .child(
                Button::new(("bookmark-metrics", ix))
                    .icon(Icon::new(Lucide::ChartLine))
                    .ghost()
                    .xsmall()
                    .tooltip("Duration and rows over time")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_metrics(&metrics_id, window, cx)
                    })),
            )
            .when(self.active_db.is_some(), |this| {
                this.child(
                    Button::new(("bookmark-board", ix))
                        .icon(Icon::new(Lucide::LayoutDashboard))
                        .ghost()
                        .xsmall()
                        .tooltip("Add to board")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.add_bookmark_to_board(&board_id, window, cx)
                        })),
                )
            })
            .child(
                Button::new(("bookmark-run", ix))
                    .icon(IconName::Play)
                    .ghost()
                    .xsmall()
                    .tooltip("Open and run")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_bookmark(&run_id, true, window, cx)
                    })),
            )
            .child(
                Button::new(("bookmark-edit", ix))
                    .icon(Icon::new(Lucide::Pencil))
                    .ghost()
                    .xsmall()
                    .tooltip("Edit")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_bookmark_dialog(
                            Some(edit_id.clone()),
                            edit_sql.clone(),
                            None,
                            window,
                            cx,
                        )
                    })),
            )
            .child(
                Button::new(("bookmark-delete", ix))
                    .icon(IconName::Delete)
                    .ghost()
                    .xsmall()
                    .tooltip("Delete")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.delete_bookmark(&delete_id, window, cx)
                    })),
            );
        h_flex()
            .id(("bookmark-row", ix))
            .group(group)
            .w_full()
            .h(px(BOOKMARK_ROW_HEIGHT))
            .px_2()
            .gap_1p5()
            .items_center()
            .rounded(theme.radius)
            .hover(|s| s.bg(theme.sidebar_accent))
            .cursor_pointer()
            // How its result is shown: the grid or the chart type.
            .child(
                Icon::new(style_icon(bookmark.view.map(|v| v.style)))
                    .xsmall()
                    .text_color(theme.muted_foreground),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_1()
                            .text_sm()
                            .child(div().min_w_0().truncate().child(bookmark.name.clone()))
                            .when_some(scheduled, |this, tooltip| {
                                this.child(
                                    div()
                                        .id(("bookmark-scheduled", ix))
                                        .tooltip(move |window, cx| {
                                            Tooltip::new(tooltip.clone()).build(window, cx)
                                        })
                                        .child(
                                            Icon::new(Lucide::Clock)
                                                .xsmall()
                                                .text_color(theme.muted_foreground),
                                        ),
                                )
                            })
                            .when(global, |this| {
                                this.child(
                                    Icon::new(IconName::Globe)
                                        .xsmall()
                                        .text_color(theme.muted_foreground),
                                )
                            }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(preview),
                    ),
            )
            .child(actions)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_bookmark(&open_id, false, window, cx)
            }))
            .into_any_element()
    }
}
