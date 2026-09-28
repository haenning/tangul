//! Query history: recording runs (and learning from them) and the sidebar panel.

use super::*;

impl Workspace {
    pub(super) fn refresh_history(&mut self, cx: &mut Context<Self>) {
        let search = self
            .panels
            .history
            .search
            .read(cx)
            .value()
            .trim()
            .to_string();
        let sources = &self.panels.history.sources;
        self.panels.history.entries = match &self.active_db {
            Some(db) => self.data.local.history(db, &search, sources, HISTORY_LIMIT),
            None => Vec::new(),
        };
        cx.notify();
    }

    /// Shows or hides `source`'s runs among the chosen ones.
    pub(super) fn toggle_history_source(&mut self, source: Source, cx: &mut Context<Self>) {
        let sources = &mut self.panels.history.sources;
        match sources.iter().position(|s| *s == source) {
            Some(ix) => {
                sources.remove(ix);
            }
            None => sources.push(source),
        }
        self.refresh_history(cx);
    }

    /// Records a finished query in history and, if it succeeded, learns from it.
    pub(super) fn record_run(
        &mut self,
        run: RunningQuery,
        error: Option<String>,
        elapsed_ms: i64,
        cx: &mut Context<Self>,
    ) {
        let Some(db_id) = self.find_user(&run.user_id).map(|(db, _)| db.id.clone()) else {
            return;
        };
        let tab = run.tab.upgrade();
        let rows = tab
            .as_ref()
            .and_then(|t| t.read(cx).row_count(cx))
            .map(|n| n as i64);
        // Passwords (`CREATE ROLE … PASSWORD '…'`) are never kept.
        let sql = tangul_assist::redact::redact(&run.sql);
        // It created or changed a user: maybe the AI's, whose rights are shown.
        let accounts_changed = matches!(sql, std::borrow::Cow::Owned(_));
        let fp = fingerprint(&sql);
        let succeeded = error.is_none();
        // A run of the tab's bookmark (the whole of it) counts for its metrics.
        let bookmark = tab
            .as_ref()
            .and_then(|t| t.read(cx).bookmark_id.clone())
            .and_then(|id| self.data.bookmark_run(&id, &run.sql));
        if let Some(b) = bookmark {
            let record = metrics::Run {
                bookmark_id: b.id,
                bookmark_name: b.name,
                started_at: run.started_at,
                duration_ms: elapsed_ms,
                rows,
                error: error.clone(),
                scheduled: false,
            };
            self.data
                .metrics
                .record(&record, &settings::current(cx).monitoring, now_ms());
        }
        let entry = NewHistory {
            database_id: &db_id,
            user_id: &run.user_id,
            sql: &sql,
            fingerprint: &fp,
            entry: HistoryEntry {
                sql: sql.to_string(),
                source: Source::App,
                started_at: run.started_at,
                duration_ms: elapsed_ms,
                rows,
                error,
            },
        };
        if let Err(e) = self.data.local.add_history(&entry) {
            eprintln!("could not record history: {e:#}");
        }
        if succeeded && accounts_changed {
            self.check_ai_user(cx);
        }
        if succeeded {
            if self.conns.contains_key(&run.user_id) {
                let schema = self.schema_of(&run.user_id);
                let learned = learn(&run.sql, &schema);
                let now = now_ms() / 1000;
                if let Err(e) = self.data.local.record_usage(&db_id, &learned, now) {
                    eprintln!("could not record usage: {e:#}");
                }
                let usage = Arc::new(Usage::from_stats(now, self.data.local.usage(&db_id)));
                self.knowledge.set_usage(&db_id, usage.clone());
                // Related rows (foreign keys, most used first) and UPDATE / DELETE
                // statements in the result's context menu.
                if let Some(tab) = &tab {
                    let columns = tab.read(cx).result_columns(cx);
                    let mut links = related_links(&run.sql, &columns, &schema);
                    if links.len() > 1 {
                        let bookmarked: Vec<_> = self
                            .data
                            .local
                            .bookmarks_for(Some(&db_id))
                            .iter()
                            .flat_map(|b| learn(&b.sql, &schema))
                            .collect();
                        rank_links(&mut links, &usage, &bookmarked);
                    }
                    let edit = row_edit(&run.sql, &columns, &schema);
                    let facts = column_facts(&run.sql, &columns, &schema);
                    tab.update(cx, |t, cx| t.set_row_actions(links, edit, facts, cx));
                }
            }
            let runs = self.data.local.runs(&db_id, &fp);
            let hint = (runs >= BOOKMARK_HINT_RUNS && !self.data.local.is_bookmarked(&db_id, &fp))
                .then_some(runs);
            if let Some(tab) = tab {
                tab.update(cx, |t, cx| {
                    t.bookmark_hint = hint;
                    cx.notify();
                });
            }
        }
        if self.active_db.as_deref() == Some(db_id.as_str()) {
            self.refresh_history(cx);
        }
    }

    pub(super) fn render_history_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let sources = self.render_history_sources(cx);
        let theme = cx.theme();
        let entries = self.panels.history.entries.clone();
        let count = entries.len();
        let empty: Option<&str> = if self.active_db.is_none() {
            Some("Select a database to see its history.")
        } else if count == 0
            && self
                .panels
                .history
                .search
                .read(cx)
                .value()
                .trim()
                .is_empty()
        {
            Some("Queries run on this database show up here.")
        } else if count == 0 {
            Some("No matching queries.")
        } else {
            None
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
                    .child("HISTORY"),
            )
            .child(
                div()
                    .px_2()
                    .pb_1()
                    .child(Input::new(&self.panels.history.search).small()),
            )
            .child(sources)
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
                    "history",
                    count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|ix| this.render_history_row(ix, &entries[ix], cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .px_1()
                .flex_1(),
            )
    }

    /// Whose runs to show: you, the assistant (AI), MCP clients. None
    /// chosen shows everyone's.
    fn render_history_sources(&self, cx: &mut Context<Self>) -> AnyElement {
        let chosen = &self.panels.history.sources;
        h_flex()
            .px_2()
            .pb_2()
            .gap_1()
            .children(Source::ALL.map(|source| {
                let (label, tooltip) = match source {
                    Source::App => ("You", "Queries you ran"),
                    Source::Ai => ("AI", "Queries the assistant ran"),
                    Source::Mcp => ("MCP", "Queries MCP clients (Claude Code, …) ran"),
                };
                Button::new(("history-source", source as usize))
                    .icon(Icon::new(source_icon(source)))
                    .label(label)
                    .xsmall()
                    .ghost()
                    .selected(chosen.contains(&source))
                    .tooltip(tooltip)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_history_source(source, cx)),
                    )
            }))
            .into_any_element()
    }

    pub(super) fn render_history_row(
        &self,
        ix: usize,
        entry: &HistoryEntry,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let group: SharedString = format!("history-{ix}").into();
        let mut detail = format!("{} · {} ms", ago(entry.started_at), entry.duration_ms);
        if let (Some(rows), None) = (entry.rows, &entry.error) {
            detail.push_str(&format!(" · {rows} rows"));
        }
        let (open_sql, bookmark_sql) = (entry.sql.clone(), entry.sql.clone());
        h_flex()
            .id(("history-row", ix))
            .group(group.clone())
            .w_full()
            .h(px(BOOKMARK_ROW_HEIGHT))
            .px_2()
            .gap_1p5()
            .items_center()
            .rounded(theme.radius)
            .hover(|s| s.bg(theme.sidebar_accent))
            .cursor_pointer()
            // Who ran it, whatever is filtered.
            .child(
                div()
                    .id(("history-source-icon", ix))
                    .test_support()
                    .flex_none()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(source_icon(entry.source)).xsmall())
                    .tooltip({
                        let who: SharedString = match entry.source {
                            Source::App => "You ran it".into(),
                            Source::Ai => "The assistant ran it".into(),
                            Source::Mcp => "An MCP client ran it".into(),
                        };
                        move |window, cx| Tooltip::new(who.clone()).build(window, cx)
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .child(one_line(&entry.sql, 200)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(if entry.error.is_some() {
                                theme.danger
                            } else {
                                theme.muted_foreground
                            })
                            .child(match &entry.error {
                                Some(e) => format!("{detail} · {}", one_line(e, 80)),
                                None => detail,
                            }),
                    ),
            )
            .child(
                div().invisible().group_hover(group, |s| s.visible()).child(
                    Button::new(("history-bookmark", ix))
                        .icon(Icon::new(Lucide::Bookmark))
                        .ghost()
                        .xsmall()
                        .tooltip("Bookmark")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_bookmark_dialog(None, bookmark_sql.clone(), None, window, cx)
                        })),
                ),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_sql("History", &open_sql, window, cx)
            }))
            .into_any_element()
    }
}

/// Who ran a query: a user (you), the robot (the assistant), the plug (MCP).
fn source_icon(source: Source) -> Lucide {
    match source {
        Source::App => Lucide::User,
        Source::Ai => Lucide::Bot,
        Source::Mcp => Lucide::Plug,
    }
}

/// "5 min ago"-style age of a unix-millisecond timestamp.
pub(super) fn ago(ms: i64) -> String {
    let secs = (now_ms() - ms).max(0) / 1000;
    match secs {
        0..60 => "just now".into(),
        60..3_600 => format!("{} min ago", secs / 60),
        3_600..86_400 => format!("{} h ago", secs / 3_600),
        _ => format!("{} d ago", secs / 86_400),
    }
}
