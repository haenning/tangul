//! Running statements: on a tab's session (streamed) or a spare one (whole result),
//! with the gate's read-only mode and time limit, and closing sessions.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tangul_core::{ConnId, QueryId, ResultSet, TabId};
use tangul_drivers::{CancelHandle, ExecEvent, SqlError};

use crate::audit::Trace;
use crate::gate::{Mode, Run};
use crate::pool::{Lease, Pool};
use crate::{Event, Problem, QueryError, QueryEvent, QueryOutput, ROW_LIMIT};

/// Upper bound for a graceful disconnect; after that the socket is simply dropped.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) type Events = async_channel::Sender<Event>;

pub(crate) struct RunningQuery {
    pub(crate) cancel: Arc<dyn CancelHandle>,
    pub(crate) cancel_requested: bool,
}

pub(crate) type Running = Arc<Mutex<HashMap<QueryId, RunningQuery>>>;

/// A statement to run on a tab's session.
pub(crate) struct Job {
    pub(crate) query: QueryId,
    pub(crate) tab: TabId,
    pub(crate) conn: ConnId,
    pub(crate) sql: String,
    pub(crate) read_only: bool,
    pub(crate) timeout: Option<Duration>,
    pub(crate) trace: Trace,
}

pub(crate) async fn execute(pool: Arc<Pool>, events: Events, running: Running, job: Job) {
    let Job {
        query,
        tab,
        conn,
        sql,
        read_only,
        timeout,
        trace,
    } = job;
    let started = Instant::now();
    let mut lease = match pool.acquire(Some(tab)).await {
        Ok(l) => l,
        Err(e) => {
            finish(
                &events,
                query,
                started.elapsed(),
                Some(describe(&e)),
                false,
                false,
            );
            return;
        }
    };
    let synced = match sync_read_only(&mut lease, read_only).await {
        Ok(()) => sync_timeout(&mut lease, timeout).await,
        Err(e) => Err(e),
    };
    if let Err(e) = synced {
        let in_tx = lease.session.in_transaction();
        finish(
            &events,
            query,
            started.elapsed(),
            Some(describe(&e)),
            in_tx,
            false,
        );
        pool.release(Some(tab), lease);
        send_stats(&events, conn, &pool);
        return;
    }
    send_stats(&events, conn, &pool);
    running.lock().unwrap().insert(
        query,
        RunningQuery {
            cancel: lease.session.cancel_handle(),
            cancel_requested: false,
        },
    );

    let mut sink = |ev: ExecEvent| {
        send(
            &events,
            Event::Query {
                query,
                event: QueryEvent::Exec(ev),
            },
        )
    };
    let result = execute_limited(&mut lease, &sql, ROW_LIMIT, timeout, &mut sink).await;

    let cancelled = running
        .lock()
        .unwrap()
        .remove(&query)
        .is_some_and(|r| r.cancel_requested);
    let in_tx = lease.session.in_transaction();
    let error = result.err().map(|e| describe(&e));
    trace.record(
        &sql,
        "ran",
        error.as_ref().map(|e| e.0.clone()),
        Some(started.elapsed()),
        None,
    );
    finish(&events, query, started.elapsed(), error, in_tx, cancelled);
    pool.release(Some(tab), lease);
    send_stats(&events, conn, &pool);
}

/// Runs `sql` on the session, stopping it at `limit`. The server stops a statement
/// at the time limit, too; this also covers one it doesn't (SQLite, MySQL for
/// anything but SELECT).
pub(crate) async fn execute_limited(
    lease: &mut Lease,
    sql: &str,
    row_limit: usize,
    limit: Option<Duration>,
    sink: tangul_drivers::Sink<'_>,
) -> anyhow::Result<()> {
    let cancel = lease.session.cancel_handle();
    let mut timed_out = false;
    let result = {
        let exec = lease.session.execute(sql, row_limit, sink);
        tokio::pin!(exec);
        match limit {
            None => exec.await,
            Some(limit) => tokio::select! {
                result = &mut exec => result,
                _ = tokio::time::sleep(limit) => {
                    timed_out = true;
                    if let Err(e) = cancel.cancel().await {
                        eprintln!("cancel after time limit failed: {e:#}");
                    }
                    exec.await
                }
            },
        }
    };
    match (timed_out, limit) {
        (true, Some(limit)) => Err(anyhow::anyhow!(
            "stopped after the time limit of {}s",
            limit.as_secs_f64()
        )),
        _ => result,
    }
}

/// A [`Command::Query`]: a spare session, the whole result collected. A session left
/// inside a transaction is closed rather than reused (the server rolls it back).
pub(crate) async fn run_query(
    pool: &Arc<Pool>,
    running: &Running,
    query: QueryId,
    sql: &str,
    run: Run,
    row_limit: usize,
) -> Result<QueryOutput, QueryError> {
    let error = |e: &anyhow::Error| {
        let (message, position) = describe(e);
        QueryError {
            position,
            ..QueryError::new(message)
        }
    };
    let started = Instant::now();
    let acquired = match run.mode {
        Mode::OwnSession => pool.acquire_own().await,
        Mode::AsSent | Mode::RolledBack => pool.acquire(None).await,
    };
    let mut lease = acquired.map_err(|e| error(&e))?;
    let mut synced = match sync_read_only(&mut lease, run.read_only).await {
        Ok(()) => sync_timeout(&mut lease, run.timeout).await,
        Err(e) => Err(e),
    };
    if synced.is_ok() && run.mode == Mode::RolledBack {
        synced = lease.session.begin_read_only().await;
    }
    if let Err(e) = synced {
        done_with(pool, lease, run.mode).await;
        return Err(error(&e));
    }
    running.lock().unwrap().insert(
        query,
        RunningQuery {
            cancel: lease.session.cancel_handle(),
            cancel_requested: false,
        },
    );
    let mut output = QueryOutput::default();
    let mut sink = |ev: ExecEvent| match ev {
        ExecEvent::ResultStart(columns) => output.results.push(ResultSet::new(columns)),
        ExecEvent::Rows(batch) => {
            if let Some(result) = output.results.last_mut() {
                result.append(batch);
            }
        }
        ExecEvent::StatementDone { rows } => output.rows = Some(rows),
        ExecEvent::Truncated => output.truncated = true,
    };
    let result = execute_limited(&mut lease, sql, row_limit, run.timeout, &mut sink).await;
    running.lock().unwrap().remove(&query);
    done_with(pool, lease, run.mode).await;
    result.map_err(|e| error(&e))?;
    output.elapsed = started.elapsed();
    Ok(output)
}

/// After a [`Command::Query`]: rolls back what [`Mode::RolledBack`] started, closes an
/// own session, and gives any other session back unless it is left in a
/// transaction (then it is closed, and the server rolls back).
pub(crate) async fn done_with(pool: &Pool, mut lease: Lease, mode: Mode) {
    if mode == Mode::RolledBack
        && lease.session.in_transaction()
        && let Err(e) = lease.session.execute("ROLLBACK", 0, &mut |_| {}).await
    {
        eprintln!("rollback after a read failed: {e:#}");
    }
    if mode == Mode::OwnSession || lease.session.in_transaction() {
        close_all(vec![lease], None);
    } else {
        pool.release(None, lease);
    }
}

/// Makes the server side match `want` before a statement runs. In read-only mode it is
/// applied before every statement, since a script could have switched it off; that
/// costs one round trip. An open transaction can't be changed, so read-only mode
/// refuses to run inside one that may write.
pub(crate) async fn sync_read_only(lease: &mut Lease, want: bool) -> anyhow::Result<()> {
    if lease.session.in_transaction() {
        if want && !lease.read_only {
            anyhow::bail!(
                "Read-only mode is on, but this tab has an open transaction that can write. \
                 Commit or roll it back first."
            );
        }
        return Ok(());
    }
    if want || lease.read_only {
        lease.session.set_read_only(want).await?;
        lease.read_only = want;
    }
    Ok(())
}

/// Tells the server the statement time limit when it changed for this session.
pub(crate) async fn sync_timeout(lease: &mut Lease, want: Option<Duration>) -> anyhow::Result<()> {
    if lease.timeout != want {
        lease.session.set_statement_timeout(want).await?;
        lease.timeout = want;
    }
    Ok(())
}

/// Longest wait for a spare session; validation is skipped when all are busy.
pub(crate) const VALIDATE_WAIT: Duration = Duration::from_secs(2);

/// `None` when no session was free in time (the caller keeps its last result).
pub(crate) async fn validate(pool: &Pool, statements: &[(usize, String)]) -> Option<Vec<Problem>> {
    let mut lease = tokio::time::timeout(VALIDATE_WAIT, pool.acquire(None))
        .await
        .ok()?
        .ok()?;
    let mut problems = Vec::new();
    for (i, (_, sql)) in statements.iter().enumerate() {
        if let Err(e) = lease.session.validate(sql).await {
            let (message, position) = describe(&e);
            problems.push(Problem {
                statement: i,
                position,
                message,
            });
        }
    }
    pool.release(None, lease);
    Some(problems)
}

/// Frees the pool slots immediately (reported via stats if `report` is set) and closes
/// the sessions in the background. A slow or hanging server disconnect must neither block
/// the pool nor delay the stats.
pub(crate) fn close_all(leases: Vec<Lease>, report: Option<(Events, ConnId, Arc<Pool>)>) {
    if leases.is_empty() {
        return;
    }
    let sessions: Vec<_> = leases.into_iter().map(Lease::into_session).collect();
    if let Some((events, conn, pool)) = report {
        send_stats(&events, conn, &pool);
    }
    tokio::spawn(async move {
        for session in sessions {
            let _ = tokio::time::timeout(CLOSE_TIMEOUT, session.close()).await;
        }
    });
}

/// Message and (for statement errors) position of an execution error.
pub(crate) fn describe(e: &anyhow::Error) -> (String, Option<usize>) {
    let position = e.downcast_ref::<SqlError>().and_then(|e| e.position);
    (format!("{e:#}"), position)
}

pub(crate) fn finish(
    events: &Events,
    query: QueryId,
    elapsed: Duration,
    error: Option<(String, Option<usize>)>,
    in_transaction: bool,
    cancelled: bool,
) {
    send(
        events,
        Event::Query {
            query,
            event: QueryEvent::Finished {
                elapsed,
                error_position: error.as_ref().and_then(|e| e.1),
                error: error.map(|e| e.0),
                in_transaction,
                cancelled,
            },
        },
    );
}

pub(crate) fn send_stats(events: &Events, conn: ConnId, pool: &Pool) {
    send(
        events,
        Event::PoolStats {
            conn,
            stats: pool.stats(),
        },
    );
}

pub(crate) fn send(events: &Events, event: Event) {
    // Unbounded: only fails when the UI is gone.
    let _ = events.try_send(event);
}
