//! The gate every statement passes before it reaches a database. The engine decides
//! here, from its own copy of `settings.json`, whether and how a statement runs for
//! the caller that sent it; callers can't pass read-only or time-limit flags of
//! their own. Behind it the server enforces the same: a read-only transaction and
//! the statement time limit.
//!
//! - **User** (a tab): read-only mode of the database user and the time limit. In
//!   write mode the user writes; an `UPDATE` / `DELETE` without `WHERE` needs the
//!   dialog's go-ahead ([`Approval::Confirmed`]). Read-only mode is lifted only in
//!   the settings, never by a go-ahead.
//! - **Schedule**: only statements on the read allow-list, in a read-only transaction
//!   that is rolled back afterwards.
//! - **Assistant**: reads on the allow-list run like a schedule's. Anything else is
//!   held: the engine issues a single-use [`Pass`] for exactly that SQL on that
//!   connection, and it runs only when sent again with the pass after a person
//!   approved it, on a new session of its own that is closed afterwards. The
//!   connection's pooled sessions never allow the assistant to write.

use std::collections::HashMap;
use std::hash::{BuildHasher, DefaultHasher, Hash, Hasher, RandomState};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use tangul_assist::guard;
use tangul_assist::suggest::one_line;
use tangul_core::ConnId;
use tangul_data::settings::Settings;

/// Who wants a statement run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    /// Typed or opened in a tab.
    User,
    /// A scheduled bookmark run.
    Schedule,
    /// The AI assistant (agent panel, MCP).
    Assistant,
}

impl Caller {
    fn name(self) -> &'static str {
        match self {
            Caller::User => "user",
            Caller::Schedule => "schedule",
            Caller::Assistant => "assistant",
        }
    }
}

/// How a statement runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    /// The server refuses writes.
    pub read_only: bool,
    /// Stopped after this long.
    pub timeout: Option<Duration>,
    pub mode: Mode,
}

/// Which session a statement runs on, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// As sent, on the tab's session (it may open a transaction and keep it).
    #[default]
    AsSent,
    /// Inside a read-only transaction that is rolled back afterwards: nothing it
    /// does stays, even if the server let a write through.
    RolledBack,
    /// On a new session of its own, closed afterwards (approved assistant writes).
    OwnSession,
}

/// Why a person is asked first; the statements are (keyword, one line of SQL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Guard {
    /// They change data, and `user` is in read-only mode.
    ReadOnly {
        user: String,
        statements: Vec<(String, String)>,
    },
    /// `UPDATE` / `DELETE` without a `WHERE`.
    Unfiltered { statements: Vec<(String, String)> },
    /// The assistant wants to run statements that aren't plain reads.
    Assistant { statements: Vec<(String, String)> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Run(Run),
    /// Ask first. Allowing [`Guard::Unfiltered`] or [`Guard::Assistant`] runs `then`;
    /// allowing [`Guard::ReadOnly`] only means lifting read-only mode.
    Confirm {
        guard: Guard,
        then: Run,
    },
    /// Never runs for this caller.
    Refuse(String),
}

/// A person's go-ahead sent along with a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Approval {
    #[default]
    None,
    /// The user confirmed the dialog for this run (tabs only).
    Confirmed,
    /// A person approved the statement the assistant sent (see [`Held`]).
    Pass(Pass),
}

/// A single-use approval for one statement on one connection, issued when the
/// engine held it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pass(u64);

/// A statement the engine didn't run because a person has to approve it first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub guard: Guard,
    /// Send the statement again with [`Approval::Pass`] once approved.
    pub pass: Pass,
}

/// Length at which a statement is cut for display.
const STATEMENT_LEN: usize = 160;
/// A pass not used within this time is void.
const PASS_LIFETIME: Duration = Duration::from_secs(10 * 60);

/// How the statements in `sql` may run for `caller`, as `db_user` (the database user
/// name, which read-only mode goes by). Used by the engine, and by front ends to
/// show a dialog before sending.
pub fn decide(settings: &Settings, db_user: Option<&str>, sql: &str, caller: Caller) -> Decision {
    let run = user_run(settings, db_user);
    match caller {
        Caller::Schedule | Caller::Assistant => {
            let not_reads: Vec<(String, String)> = guard::not_reads(sql)
                .into_iter()
                .map(|(keyword, statement)| (keyword, one_line(&statement, STATEMENT_LEN)))
                .collect();
            if not_reads.is_empty() {
                return Decision::Run(Run {
                    read_only: true,
                    timeout: run.timeout,
                    mode: Mode::RolledBack,
                });
            }
            if caller == Caller::Schedule {
                return Decision::Refuse(format!(
                    "only statements that read may run on a schedule; this one is {}",
                    not_reads[0].0
                ));
            }
            if let Some(user) = db_user.filter(|_| run.read_only) {
                return Decision::Refuse(format!(
                    "read-only mode is on for {user}, so the assistant can't change data"
                ));
            }
            Decision::Confirm {
                guard: Guard::Assistant {
                    statements: not_reads,
                },
                then: Run {
                    mode: Mode::OwnSession,
                    ..run
                },
            }
        }
        Caller::User => {
            let checks = guard::check(sql);
            let described = |keep: &dyn Fn(&guard::Check) -> bool| -> Vec<(String, String)> {
                checks
                    .iter()
                    .filter(|c| keep(c))
                    .map(|c| (c.keyword.clone(), one_line(&c.statement, STATEMENT_LEN)))
                    .collect()
            };
            if let Some(user) = db_user.filter(|_| run.read_only) {
                let statements = described(&|c| c.writes);
                if !statements.is_empty() {
                    return Decision::Confirm {
                        guard: Guard::ReadOnly {
                            user: user.to_string(),
                            statements,
                        },
                        then: run,
                    };
                }
            }
            if settings.safety.confirm_unfiltered_writes {
                let statements = described(&|c| c.unfiltered);
                if !statements.is_empty() {
                    return Decision::Confirm {
                        guard: Guard::Unfiltered { statements },
                        then: run,
                    };
                }
            }
            Decision::Run(run)
        }
    }
}

/// How the user's statements run without any check: read-only mode of `db_user` and
/// the time limit. For queries derived from a result (filter, sort, page).
pub fn user_run(settings: &Settings, db_user: Option<&str>) -> Run {
    Run {
        read_only: db_user.is_some_and(|u| settings.is_read_only(u)),
        timeout: settings.safety.max_query_time,
        mode: Mode::AsSent,
    }
}

/// Where the engine's settings come from.
#[derive(Debug, Clone)]
pub enum SettingsSource {
    /// `settings.json`, read by the engine and read again when it changes. `None`
    /// (no config directory) means the defaults.
    File(Option<PathBuf>),
    /// Fixed settings (tests).
    Fixed(Box<Settings>),
}

/// What the gate decided for one statement.
#[derive(Debug)]
pub(crate) enum Enforced {
    Run(Run),
    Held(Held),
    Refused(String),
}

struct Issued {
    conn: ConnId,
    sql: u64,
    at: Instant,
}

pub(crate) struct Gate {
    source: SettingsSource,
    settings: Settings,
    /// Modification time of the file last read.
    seen: Option<Option<SystemTime>>,
    passes: HashMap<Pass, Issued>,
}

fn fingerprint(sql: &str) -> u64 {
    let mut h = DefaultHasher::new();
    sql.trim().hash(&mut h);
    h.finish()
}

impl Gate {
    pub(crate) fn new(source: SettingsSource) -> Self {
        let settings = match &source {
            SettingsSource::Fixed(settings) => (**settings).clone(),
            SettingsSource::File(_) => Settings::default(),
        };
        let mut gate = Self {
            source,
            settings,
            seen: None,
            passes: HashMap::new(),
        };
        gate.reload();
        gate
    }

    /// Reads the settings file again if it changed. A file that can't be parsed keeps
    /// the settings in effect; no file means the defaults.
    pub(crate) fn reload(&mut self) {
        let SettingsSource::File(Some(path)) = &self.source else {
            return;
        };
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if self.seen == Some(modified) {
            return;
        }
        self.seen = Some(modified);
        let text = std::fs::read_to_string(path).unwrap_or_default();
        match Settings::parse(&text) {
            Ok(settings) => self.settings = settings,
            Err(e) => eprintln!("engine: keeping settings, invalid {}: {e}", path.display()),
        }
    }

    /// Reads the file again now (after the app changed it).
    pub(crate) fn reload_now(&mut self) {
        self.seen = None;
        self.reload();
    }

    pub(crate) fn enforce(
        &mut self,
        conn: ConnId,
        db_user: &str,
        sql: &str,
        caller: Caller,
        approval: Approval,
    ) -> Enforced {
        self.passes.retain(|_, p| p.at.elapsed() < PASS_LIFETIME);
        match decide(&self.settings, Some(db_user), sql, caller) {
            Decision::Run(run) => Enforced::Run(run),
            Decision::Refuse(reason) => Enforced::Refused(reason),
            Decision::Confirm {
                guard: Guard::ReadOnly { user, .. },
                ..
            } => Enforced::Refused(format!(
                "read-only mode is on for {user}; turn it off to change data"
            )),
            Decision::Confirm {
                guard: Guard::Unfiltered { .. },
                then,
            } => match (caller, approval) {
                (Caller::User, Approval::Confirmed) => Enforced::Run(then),
                _ => Enforced::Refused(
                    "UPDATE or DELETE without WHERE needs to be confirmed first".into(),
                ),
            },
            Decision::Confirm {
                guard: guard @ Guard::Assistant { .. },
                then,
            } => {
                if let Approval::Pass(pass) = approval
                    && self.redeem(pass, conn, sql)
                {
                    return Enforced::Run(then);
                }
                Enforced::Held(Held {
                    guard,
                    pass: self.issue(conn, sql),
                })
            }
        }
    }

    fn issue(&mut self, conn: ConnId, sql: &str) -> Pass {
        // Random (SipHash under keys std draws from the OS), so a pass can't be
        // guessed from one seen before.
        let pass = loop {
            let pass = Pass(RandomState::new().build_hasher().finish());
            if !self.passes.contains_key(&pass) {
                break pass;
            }
        };
        self.passes.insert(
            pass,
            Issued {
                conn,
                sql: fingerprint(sql),
                at: Instant::now(),
            },
        );
        pass
    }

    /// Uses up `pass` if it was issued for exactly this statement on this connection.
    fn redeem(&mut self, pass: Pass, conn: ConnId, sql: &str) -> bool {
        let valid = self
            .passes
            .get(&pass)
            .is_some_and(|p| p.conn == conn && p.sql == fingerprint(sql));
        if valid {
            self.passes.remove(&pass);
        }
        valid
    }
}

/// The caller's name as the audit log writes it.
pub(crate) fn caller_name(caller: Caller) -> &'static str {
    caller.name()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(json: &str) -> Settings {
        Settings::parse(json).unwrap()
    }

    fn gate(json: &str) -> Gate {
        Gate::new(SettingsSource::Fixed(Box::new(settings(json))))
    }

    #[test]
    fn users_write_in_write_mode() {
        let s = settings(r#"{"safety": {"read_only_users": ["ro"], "max_query_time": "5s"}}"#);
        let limit = Some(Duration::from_secs(5));
        assert_eq!(
            decide(
                &s,
                Some("rw"),
                "update t set a = 1 where id = 2",
                Caller::User
            ),
            Decision::Run(Run {
                read_only: false,
                timeout: limit,
                mode: Mode::AsSent,
            })
        );
        assert_eq!(
            decide(&s, Some("ro"), "select 1", Caller::User),
            Decision::Run(Run {
                read_only: true,
                timeout: limit,
                mode: Mode::AsSent,
            })
        );
        assert!(matches!(
            decide(&s, Some("ro"), "delete from t where id = 1", Caller::User),
            Decision::Confirm {
                guard: Guard::ReadOnly { .. },
                ..
            }
        ));
        assert!(matches!(
            decide(&s, Some("rw"), "delete from t", Caller::User),
            Decision::Confirm {
                guard: Guard::Unfiltered { .. },
                ..
            }
        ));
    }

    #[test]
    fn the_gate_enforces_for_users() {
        let mut g = gate(r#"{"safety": {"read_only_users": ["ro"]}}"#);
        let c = ConnId(1);
        assert!(matches!(
            g.enforce(
                c,
                "rw",
                "delete from t where id = 1",
                Caller::User,
                Approval::None
            ),
            Enforced::Run(Run {
                read_only: false,
                ..
            })
        ));
        // Without WHERE: only after the dialog.
        assert!(matches!(
            g.enforce(c, "rw", "delete from t", Caller::User, Approval::None),
            Enforced::Refused(_)
        ));
        assert!(matches!(
            g.enforce(c, "rw", "delete from t", Caller::User, Approval::Confirmed),
            Enforced::Run(_)
        ));
        // Read-only mode: a go-ahead doesn't lift it.
        assert!(matches!(
            g.enforce(
                c,
                "ro",
                "delete from t where id = 1",
                Caller::User,
                Approval::Confirmed
            ),
            Enforced::Refused(_)
        ));
    }

    #[test]
    fn schedules_only_read() {
        let mut g = gate("{}");
        let c = ConnId(1);
        assert!(matches!(
            g.enforce(c, "rw", "select * from t", Caller::Schedule, Approval::None),
            Enforced::Run(Run {
                read_only: true,
                mode: Mode::RolledBack,
                ..
            })
        ));
        for sql in [
            "delete from t where id = 1",
            "select nextval('s')",
            "set x = 1",
        ] {
            assert!(
                matches!(
                    g.enforce(c, "rw", sql, Caller::Schedule, Approval::Confirmed),
                    Enforced::Refused(_)
                ),
                "{sql}"
            );
        }
    }

    #[test]
    fn assistant_writes_need_a_single_use_pass() {
        let mut g = gate(r#"{"safety": {"read_only_users": ["ro"]}}"#);
        let (c, other) = (ConnId(1), ConnId(2));
        let write = "update t set a = 1 where id = 2";
        assert!(matches!(
            g.enforce(
                c,
                "rw",
                "select * from t",
                Caller::Assistant,
                Approval::None
            ),
            Enforced::Run(Run {
                read_only: true,
                mode: Mode::RolledBack,
                ..
            })
        ));
        // A confirmation meant for tabs doesn't count.
        let Enforced::Held(held) =
            g.enforce(c, "rw", write, Caller::Assistant, Approval::Confirmed)
        else {
            panic!("held");
        };
        assert!(matches!(held.guard, Guard::Assistant { .. }));
        let pass = Approval::Pass(held.pass);
        // Not for other SQL, not on another connection.
        assert!(matches!(
            g.enforce(
                c,
                "rw",
                "delete from t where id = 9",
                Caller::Assistant,
                pass
            ),
            Enforced::Held(_)
        ));
        assert!(matches!(
            g.enforce(other, "rw", write, Caller::Assistant, pass),
            Enforced::Held(_)
        ));
        // Exactly this statement: runs, with write access on a session of its own,
        // once.
        assert!(matches!(
            g.enforce(c, "rw", write, Caller::Assistant, pass),
            Enforced::Run(Run {
                read_only: false,
                mode: Mode::OwnSession,
                ..
            })
        ));
        assert!(matches!(
            g.enforce(c, "rw", write, Caller::Assistant, pass),
            Enforced::Held(_)
        ));
        // In read-only mode the assistant can't change data at all.
        assert!(matches!(
            g.enforce(c, "ro", write, Caller::Assistant, Approval::None),
            Enforced::Refused(_)
        ));
        // A made-up pass doesn't work.
        assert!(matches!(
            g.enforce(c, "rw", write, Caller::Assistant, Approval::Pass(Pass(42))),
            Enforced::Held(_)
        ));
    }

    #[test]
    fn reads_the_settings_file_itself() {
        let dir = std::env::temp_dir().join(format!("tangul-gate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let _ = std::fs::remove_file(&path);
        let mut g = Gate::new(SettingsSource::File(Some(path.clone())));
        let c = ConnId(1);
        // No file: defaults (write mode, 60s).
        assert!(matches!(
            g.enforce(c, "u", "select 1", Caller::User, Approval::None),
            Enforced::Run(Run {
                read_only: false,
                ..
            })
        ));
        std::fs::write(&path, r#"{"safety": {"read_only_users": ["u"]}}"#).unwrap();
        g.reload_now();
        assert!(matches!(
            g.enforce(c, "u", "select 1", Caller::User, Approval::None),
            Enforced::Run(Run {
                read_only: true,
                ..
            })
        ));
        // A broken file keeps what was in effect.
        std::fs::write(&path, "{broken").unwrap();
        g.reload_now();
        assert!(matches!(
            g.enforce(c, "u", "select 1", Caller::User, Approval::None),
            Enforced::Run(Run {
                read_only: true,
                ..
            })
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
