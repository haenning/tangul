//! Scheduled bookmark runs on hidden connections, and the metrics tab.

use super::*;

impl Workspace {
    /// Starts the next due scheduled bookmark, if the app is quiet and nothing else
    /// runs. One at a time, on a hidden connection; the engine's gate only lets
    /// reads through for schedules.
    pub(super) fn monitor_tick(&mut self, cx: &mut Context<Self>) {
        let settings = settings::current(cx).clone();
        if self.monitor.stuck(settings.safety.max_query_time) {
            self.monitor_finish(Err("gave up waiting for the run".into()), cx);
        }
        if self.monitor.pending.is_some() || !self.monitor.is_quiet() || !self.router.is_idle() {
            return;
        }
        let bookmarks = self.data.local.scheduled_bookmarks();
        let now = now_ms();
        let Some(bookmark) = schedule::due(
            &bookmarks,
            |id| self.data.metrics.last_run(id),
            now,
            settings.monitoring.interval(),
        )
        .cloned() else {
            return;
        };
        // The user last used on the database, else its first one.
        let user = bookmark
            .database_id
            .as_deref()
            .and_then(|id| self.data.profiles.database(id))
            .and_then(|db| {
                let last = self.data.local.last_user(&db.id);
                db.users
                    .iter()
                    .find(|u| Some(&u.id) == last.as_ref())
                    .or(db.users.first())
                    .cloned()
            });
        let Some(user) = user else {
            self.record_scheduled(
                &bookmark,
                now,
                0,
                Err("no database user to run it with".into()),
                cx,
            );
            return;
        };
        let conn = match self.connections.get(Purpose::Background, &user.id) {
            Some(conn) => conn,
            None => {
                let Some(conn) = self.connections.connect_user(
                    &self.engine,
                    &mut self.data.profiles,
                    &user.id,
                    Purpose::Background,
                ) else {
                    return;
                };
                conn
            }
        };
        let (engine, query, sql) = (
            self.engine.clone(),
            QueryId(self.next_id()),
            bookmark.sql.clone(),
        );
        // The engine's gate makes it read-only and refuses anything but reads.
        let options = QueryOptions::new(Caller::Schedule);
        let task = cx.spawn(async move |this, cx| {
            let result = engine.query(query, conn, sql, options).await;
            let _ = this.update(cx, |ws, cx| {
                ws.monitor_finish(result.map_err(|e| e.message), cx)
            });
        });
        self.monitor.start(bookmark, conn, now, task);
    }

    /// Records the pending scheduled run. A failed run forgets its connection, so
    /// the next one connects again.
    pub(super) fn monitor_finish(
        &mut self,
        result: Result<QueryOutput, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.monitor.pending.take() else {
            return;
        };
        if result.is_err() {
            self.engine.send(Command::Disconnect { conn: pending.conn });
            self.connections.forget(pending.conn);
        }
        let duration_ms = match &result {
            Ok(out) => out.elapsed.as_millis() as i64,
            Err(_) => pending.elapsed().as_millis() as i64,
        };
        let rows = result.map(|out| out.rows.map(|n| n as i64));
        self.record_scheduled(&pending.bookmark, pending.started_at, duration_ms, rows, cx);
    }

    fn record_scheduled(
        &self,
        bookmark: &Bookmark,
        started_at: i64,
        duration_ms: i64,
        rows: Result<Option<i64>, String>,
        cx: &App,
    ) {
        let (rows, error) = match rows {
            Ok(rows) => (rows, None),
            Err(e) => (None, Some(e)),
        };
        let record = metrics::Run {
            bookmark_id: bookmark.id.clone(),
            bookmark_name: bookmark.name.clone(),
            started_at,
            duration_ms,
            rows,
            error,
            scheduled: true,
        };
        self.data
            .metrics
            .record(&record, &settings::current(cx).monitoring, now_ms());
    }

    /// Handles the events of connections the UI doesn't show (scheduled runs, the
    /// assistant, local files); returns the tabs' events.
    pub(super) fn hidden_event(
        &mut self,
        event: Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Event> {
        let conn = match &event {
            Event::Connected { conn }
            | Event::ConnectFailed { conn, .. }
            | Event::HostKeyUnknown { conn, .. }
            | Event::Schema { conn, .. }
            | Event::PoolStats { conn, .. } => *conn,
            Event::Query { .. } | Event::Validated { .. } => return Some(event),
        };
        match (self.connections.purpose(conn), event) {
            (None | Some(Purpose::Tab), event) => return Some(event),
            (Some(Purpose::LocalFile), Event::ConnectFailed { error, .. }) => {
                self.connections.forget(conn);
                window.push_notification(
                    Notification::error(format!("Could not open the metrics: {error}")),
                    cx,
                );
            }
            // Connected again the next time the AI's databases are offered.
            (Some(Purpose::Assistant), Event::ConnectFailed { error, .. }) => {
                eprintln!("the AI's connection failed: {error}");
                self.connections.forget(conn);
            }
            // A scheduled run on it fails by itself and forgets it.
            (Some(_), _) => {}
        }
        None
    }

    /// Opens a tab querying the bookmark's runs over time in `metrics.db`
    /// (read-only), shown as a line chart of the duration.
    pub(super) fn open_metrics(
        &mut self,
        bookmark_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(bookmark) = self.data.local.bookmark(bookmark_id) else {
            return;
        };
        let Some(path) = self
            .data
            .metrics
            .path()
            .map(|p| p.to_string_lossy().into_owned())
        else {
            window.push_notification(
                Notification::error("Metrics are only kept in memory; there is no file to query."),
                cx,
            );
            return;
        };
        // Runs right away: the engine opens the file before it takes the query.
        let (conn, _) = self.connections.open_file(&self.engine, &path);
        let sql = metrics::runs_sql(&bookmark.id);
        let tab = self.add_tab(&sql, window, cx);
        tab.update(cx, |t, cx| {
            t.title = format!("{} · runs", bookmark.name).into();
            let file = cx.new(|_| Connection::local_file(conn, "tangul metrics"));
            t.set_conn(file, cx);
            // Duration over time; rows are one pick away in the chart menu.
            t.set_chart_view(
                Some(ChartView {
                    style: ChartStyle::Line,
                    series: Some(0),
                }),
                cx,
            );
            cx.emit(TabEvent::Run(sql.clone()));
        });
    }
}
