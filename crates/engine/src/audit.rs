//! The audit log: what the assistant and schedules sent, and how it went.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tangul_data::local::{AuditEntry, Local};

use crate::gate::{self, Caller};

/// The audit log: what the assistant and schedules sent, and how it went.
#[derive(Clone, Default)]
pub(crate) struct Audit(pub(crate) Option<Arc<Mutex<Local>>>);

impl Audit {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record(
        &self,
        caller: Caller,
        target: &str,
        sql: &str,
        outcome: &str,
        error: Option<String>,
        duration: Option<Duration>,
        rows: Option<u64>,
    ) {
        let Some(local) = &self.0 else { return };
        if caller == Caller::User {
            return;
        }
        let entry = AuditEntry {
            at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or_default(),
            caller: gate::caller_name(caller).into(),
            target: target.into(),
            sql: sql.into(),
            outcome: outcome.into(),
            error,
            duration_ms: duration.map(|d| d.as_millis() as i64),
            rows: rows.map(|r| r as i64),
        };
        if let Err(e) = local.lock().unwrap().add_audit(&entry) {
            eprintln!("could not write the audit log: {e:#}");
        }
    }
}

/// Who ran a statement, for the audit log.
#[derive(Clone)]
pub(crate) struct Trace {
    pub(crate) audit: Audit,
    pub(crate) caller: Caller,
    pub(crate) target: String,
}

impl Trace {
    pub(crate) fn record(
        &self,
        sql: &str,
        outcome: &str,
        error: Option<String>,
        duration: Option<Duration>,
        rows: Option<u64>,
    ) {
        self.audit.record(
            self.caller,
            &self.target,
            sql,
            outcome,
            error,
            duration,
            rows,
        );
    }
}
