//! Scheduled bookmark runs on hidden connections, and the metrics tab.

use super::*;

impl Workspace {
    /// Starts the next due scheduled bookmark, if the app is quiet and nothing else
    /// runs. One at a time, on a hidden read-only connection.
    pub(super) fn monitor_tick(&mut self, cx: &mut Context<Self>) {
        let limits = settings::current(cx).monitoring.clone();
        if self
            .monitor
            .stuck(settings::current(cx).safety.max_query_time)
        {
            self.monitor_finish(0, Some("gave up waiting for the run".into()), cx);
        }
        if self.monitor.pending.is_some() || !self.monitor.is_quiet() || !self.queries.is_empty() {
            return;
        }
        let bookmarks = self.data.local.scheduled_bookmarks();
        let now = now_ms();
        let Some(bookmark) = monitor::due(
            &bookmarks,
            |id| self.data.metrics.last_run(id),
            now,
            limits.interval(),
        )
        .cloned() else {
            return;
        };
        let db = bookmark
            .database_id
            .as_deref()
            .and_then(|id| self.data.profiles.database(id))
            .cloned();
        // The user last used on the database, else its first one.
        let user = db.as_ref().and_then(|db| {
            let last = self.data.local.last_user(&db.id);
            db.users
                .iter()
                .find(|u| Some(&u.id) == last.as_ref())
                .or(db.users.first())
                .cloned()
        });
        let (Some(db), Some(user)) = (db, user) else {
            let record = metrics::Run {
                bookmark_id: bookmark.id,
                bookmark_name: bookmark.name,
                started_at: now,
                duration_ms: 0,
                rows: None,
                error: Some("no database user to run it with".into()),
                scheduled: true,
            };
            self.data.metrics.record(&record, &limits, now);
            return;
        };
        let (conn, ready) = match self.monitor.conn_for(&user.id) {
            Some(conn) => conn,
            None => {
                let id = ConnId(self.next_id());
                self.monitor.connecting(&user.id, id);
                let password = self.data.profiles.password(&user.id);
                self.engine.send(Command::Connect {
                    conn: id,
                    driver: db.driver,
                    cfg: tangul_core::ConnectConfig::new(&db, &user, password),
                    ssh: self.data.profiles.ssh_target(&db),
                });
                (id, false)
            }
        };
        self.monitor.start(bookmark, conn, now);
        if ready {
            self.monitor_execute(cx);
        }
    }

    /// Sends the pending scheduled run's query, read-only.
    pub(super) fn monitor_execute(&mut self, cx: &mut Context<Self>) {
        let timeout = settings::current(cx).safety.max_query_time;
        let query = QueryId(self.next_id());
        let Some(pending) = self.monitor.pending.as_mut() else {
            return;
        };
        pending.query = Some(query);
        self.engine.send(Command::Execute {
            query,
            // A tab of its own, so no session of a real tab is used.
            tab: TabId(query.0),
            conn: pending.conn,
            sql: pending.bookmark.sql.clone(),
            read_only: true,
            timeout,
        });
    }

    /// Records the pending scheduled run.
    pub(super) fn monitor_finish(
        &mut self,
        duration_ms: i64,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.monitor.pending.take() else {
            return;
        };
        if let Some(query) = pending.query {
            self.engine.send(Command::ReleaseTab {
                tab: TabId(query.0),
                conn: pending.conn,
            });
        }
        let record = metrics::Run {
            bookmark_id: pending.bookmark.id,
            bookmark_name: pending.bookmark.name,
            started_at: pending.started_at,
            duration_ms,
            rows: pending.rows.filter(|_| error.is_none()),
            error,
            scheduled: true,
        };
        self.data
            .metrics
            .record(&record, &settings::current(cx).monitoring, now_ms());
    }

    /// Handles events of the hidden connections, scheduled runs and the metrics
    /// connection; returns every other event.
    pub(super) fn monitor_event(
        &mut self,
        event: Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Event> {
        let pending_query = self.monitor.pending.as_ref().and_then(|p| p.query);
        let metrics_conn = self.metrics_conn.map(|(id, _)| id);
        match event {
            Event::Query { query, event } if Some(query) == pending_query => match event {
                QueryEvent::Exec(ExecEvent::StatementDone { rows }) => {
                    if let Some(p) = self.monitor.pending.as_mut() {
                        p.rows = Some(rows as i64);
                    }
                }
                QueryEvent::Finished { elapsed, error, .. } => {
                    self.monitor_finish(elapsed.as_millis() as i64, error, cx);
                }
                QueryEvent::Exec(_) => {}
            },
            Event::Connected { conn } if self.monitor.owns(conn) => {
                self.monitor.connected(conn);
                let waiting = self.monitor.pending.as_ref();
                if waiting.is_some_and(|p| p.conn == conn && p.query.is_none()) {
                    self.monitor_execute(cx);
                }
            }
            Event::ConnectFailed { conn, error } if self.monitor.owns(conn) => {
                self.monitor.failed(conn);
                self.monitor_finish(0, Some(error), cx);
            }
            Event::HostKeyUnknown { conn, .. } if self.monitor.owns(conn) => {
                self.monitor.failed(conn);
                let error = "unknown SSH host key: connect once to trust it".to_string();
                self.monitor_finish(0, Some(error), cx);
            }
            Event::Schema { conn, .. } | Event::PoolStats { conn, .. }
                if self.monitor.owns(conn) || Some(conn) == metrics_conn => {}
            Event::Connected { conn } if Some(conn) == metrics_conn => {
                self.metrics_conn = Some((conn, true));
                for tab in std::mem::take(&mut self.metrics_waiting) {
                    if let Some(tab) = tab.upgrade() {
                        tab.update(cx, |t, cx| cx.emit(TabEvent::Run(t.sql(cx))));
                    }
                }
            }
            Event::ConnectFailed { conn, error } if Some(conn) == metrics_conn => {
                self.metrics_conn = None;
                self.metrics_waiting.clear();
                window.push_notification(
                    Notification::error(format!("Could not open the metrics: {error}")),
                    cx,
                );
            }
            event => return Some(event),
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
        let (conn, ready) = match self.metrics_conn {
            Some(conn) => conn,
            None => {
                let id = ConnId(self.next_id());
                self.engine.send(Command::Connect {
                    conn: id,
                    driver: DriverKind::Sqlite,
                    cfg: tangul_core::ConnectConfig {
                        host: String::new(),
                        port: 0,
                        user: String::new(),
                        password: None,
                        database: path,
                        tls: tangul_core::TlsMode::Disable,
                    },
                    ssh: None,
                });
                self.metrics_conn = Some((id, false));
                (id, false)
            }
        };
        let sql = metrics::runs_sql(&bookmark.id);
        let tab = self.add_tab(&sql, window, cx);
        tab.update(cx, |t, cx| {
            t.title = format!("{} · runs", bookmark.name).into();
            let tab_conn = TabConn {
                id: conn,
                name: "tangul metrics".into(),
                driver: DriverKind::Sqlite,
            };
            t.set_conn(tab_conn, cx);
            // Duration over time; rows are one pick away in the chart menu.
            t.set_chart_view(
                Some(ChartView {
                    style: ChartStyle::Line,
                    series: Some(0),
                }),
                cx,
            );
            if ready {
                cx.emit(TabEvent::Run(sql.clone()));
            }
        });
        if !ready {
            self.metrics_waiting.push(tab.downgrade());
        }
    }
}
