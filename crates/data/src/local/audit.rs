//! What the assistant, AI tools and scheduled runs did: every statement they sent,
//! whether it ran, was proposed to the user or refused, and how it went (written by the
//! engine, which is where every statement passes); and every AI tool call (written
//! by whoever runs the tools, e.g. the MCP server).

use anyhow::Result;
use rusqlite::params;

use super::Local;

/// Most entries kept; older ones are dropped.
const KEEP: i64 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// Unix milliseconds.
    pub at: i64,
    /// `assistant`, `schedule` or `mcp`.
    pub caller: String,
    /// Where it went: `user@host/database`.
    pub target: String,
    pub sql: String,
    /// `ran`, `proposed` (not run; for the user to open) or `refused`; for tool calls
    /// also `failed`.
    pub outcome: String,
    pub error: Option<String>,
    pub duration_ms: Option<i64>,
    pub rows: Option<i64>,
}

impl Local {
    pub fn add_audit(&self, e: &AuditEntry) -> Result<()> {
        self.conn.execute(
            "INSERT INTO audit (at, caller, target, sql, outcome, error, duration_ms, rows)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                e.at,
                e.caller,
                e.target,
                e.sql,
                e.outcome,
                e.error,
                e.duration_ms,
                e.rows
            ],
        )?;
        self.conn.execute(
            "DELETE FROM audit WHERE id <= (SELECT max(id) FROM audit) - ?1",
            params![KEEP],
        )?;
        Ok(())
    }

    /// The latest entries, newest first.
    pub fn audit(&self, limit: usize) -> Vec<AuditEntry> {
        self.audit_query(
            "SELECT at, caller, target, sql, outcome, error, duration_ms, rows
             FROM audit ORDER BY at DESC, id DESC LIMIT ?1",
            params![limit as i64],
        )
    }

    /// The AI tool calls of `caller` (`assistant`, `mcp`), newest first, `limit`
    /// of them after skipping `offset`.
    pub fn tool_calls(&self, caller: &str, offset: usize, limit: usize) -> Vec<AuditEntry> {
        self.audit_query(
            "SELECT at, caller, target, sql, outcome, error, duration_ms, rows
             FROM audit WHERE caller = ?1 AND target LIKE 'tool:%'
             ORDER BY at DESC, id DESC LIMIT ?2 OFFSET ?3",
            params![caller, limit as i64, offset as i64],
        )
    }

    /// How many AI tool calls of `caller` are kept.
    pub fn tool_call_count(&self, caller: &str) -> usize {
        self.conn
            .query_row(
                "SELECT count(*) FROM audit WHERE caller = ?1 AND target LIKE 'tool:%'",
                params![caller],
                |r| r.get::<_, i64>(0),
            )
            .map_or(0, |n| n as usize)
    }

    fn audit_query(&self, sql: &str, args: impl rusqlite::Params) -> Vec<AuditEntry> {
        let result = self.conn.prepare_cached(sql).and_then(|mut stmt| {
            stmt.query_map(args, |r| {
                Ok(AuditEntry {
                    at: r.get(0)?,
                    caller: r.get(1)?,
                    target: r.get(2)?,
                    sql: r.get(3)?,
                    outcome: r.get(4)?,
                    error: r.get(5)?,
                    duration_ms: r.get(6)?,
                    rows: r.get(7)?,
                })
            })?
            .collect()
        });
        result.unwrap_or_else(|e| {
            eprintln!("audit query failed: {e}");
            Vec::new()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_newest_first() {
        let l = Local::in_memory();
        let entry = |at: i64, outcome: &str| AuditEntry {
            at,
            caller: "assistant".into(),
            target: "app@db/app".into(),
            sql: "select 1".into(),
            outcome: outcome.into(),
            error: None,
            duration_ms: Some(3),
            rows: Some(1),
        };
        l.add_audit(&entry(1, "ran")).unwrap();
        l.add_audit(&entry(2, "proposed")).unwrap();
        let all = l.audit(10);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], entry(2, "proposed"));
    }

    #[test]
    fn tool_calls_by_caller_in_pages() {
        let l = Local::in_memory();
        let call = |at: i64, caller: &str, target: &str| AuditEntry {
            at,
            caller: caller.into(),
            target: target.into(),
            sql: "{}".into(),
            outcome: "ran".into(),
            error: None,
            duration_ms: Some(1),
            rows: None,
        };
        for at in 0..30 {
            l.add_audit(&call(at, "mcp", "tool:run_readonly shop"))
                .unwrap();
        }
        l.add_audit(&call(100, "assistant", "tool:describe_table"))
            .unwrap();
        // A statement the assistant ran is no tool call.
        l.add_audit(&call(101, "assistant", "app@db/app")).unwrap();
        assert_eq!(l.tool_call_count("mcp"), 30);
        assert_eq!(l.tool_call_count("assistant"), 1);
        let page = l.tool_calls("mcp", 25, 25);
        assert_eq!(
            page.iter().map(|c| c.at).collect::<Vec<_>>(),
            [4, 3, 2, 1, 0]
        );
        assert_eq!(l.tool_calls("mcp", 0, 5)[0].at, 29);
        assert_eq!(l.tool_calls("assistant", 0, 5).len(), 1);
    }
}
