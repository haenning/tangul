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
//!   that is rolled back afterwards. One statement at a time, checked with the
//!   database's lexical rules and sent without its comments, so the server runs
//!   exactly what was checked.
//! - **Assistant**: reads on the allow-list run like a schedule's. Anything else never
//!   runs for it: it comes back as a [`Proposal`], which the user may open in a tab
//!   and run as themselves (`Caller::User`, with the user's rules).

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use tangul_assist::guard;
use tangul_assist::lexer::Lexing;
use tangul_assist::suggest::one_line;
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
    /// The editor sampling a column's values to offer them (like schedules: reads
    /// only).
    Editor,
}

impl Caller {
    fn name(self) -> &'static str {
        match self {
            Caller::User => "user",
            Caller::Schedule => "schedule",
            Caller::Assistant => "assistant",
            Caller::Editor => "editor",
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
}

/// What the assistant sent that isn't a plain read: never run for it. The user may
/// open it in a tab and run it as themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    /// (keyword, one line) of each statement that isn't a read.
    pub statements: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Run(Run),
    /// Ask first. Allowing [`Guard::Unfiltered`] runs `then`; allowing
    /// [`Guard::ReadOnly`] only means lifting read-only mode.
    Confirm {
        guard: Guard,
        then: Run,
    },
    /// Never runs for this caller.
    Refuse(String),
    /// The assistant's statements that aren't reads: not run, proposed to the user.
    Propose(Proposal),
}

/// A person's go-ahead sent along with a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Approval {
    #[default]
    None,
    /// The user confirmed the dialog for this run (tabs only).
    Confirmed,
}

/// Length at which a statement is cut for display.
const STATEMENT_LEN: usize = 160;

/// How the statements in `sql` may run for `caller`, as `db_user` (the database user
/// name, which read-only mode goes by). Used by the engine, and by front ends to
/// show a dialog before sending.
pub fn decide(settings: &Settings, db_user: Option<&str>, sql: &str, caller: Caller) -> Decision {
    decide_with(settings, db_user, sql, Lexing::default(), caller)
}

/// [`decide`] with the database's lexical rules. For the schedule and the assistant,
/// `sql` is the text they send ([`guard::read_text`]).
fn decide_with(
    settings: &Settings,
    db_user: Option<&str>,
    sql: &str,
    lexing: Lexing,
    caller: Caller,
) -> Decision {
    let run = user_run(settings, db_user);
    match caller {
        Caller::Schedule | Caller::Editor | Caller::Assistant => {
            let not_reads: Vec<(String, String)> = guard::not_reads_with(sql, lexing)
                .into_iter()
                .map(|(keyword, statement)| (keyword, one_line(&statement, STATEMENT_LEN)))
                .collect();
            if not_reads.is_empty() {
                if let Some(reason) = guard::not_one_read(sql) {
                    return Decision::Refuse(reason.to_string());
                }
                return Decision::Run(Run {
                    read_only: true,
                    timeout: run.timeout,
                    mode: Mode::RolledBack,
                });
            }
            if caller != Caller::Assistant {
                let place = if caller == Caller::Schedule {
                    "on a schedule"
                } else {
                    "for the editor"
                };
                return Decision::Refuse(format!(
                    "only statements that read may run {place}; this one is {}",
                    not_reads[0].0
                ));
            }
            Decision::Propose(Proposal {
                statements: not_reads,
            })
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
    /// Run `sql`: the user's as sent, a reader's without comments.
    Run {
        run: Run,
        sql: String,
    },
    Proposed(Proposal),
    Refused(String),
}

pub(crate) struct Gate {
    source: SettingsSource,
    settings: Settings,
    /// Modification time of the file last read.
    seen: Option<Option<SystemTime>>,
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

    /// `lexing`: the database's lexical rules.
    pub(crate) fn enforce(
        &self,
        db_user: &str,
        sql: &str,
        lexing: Lexing,
        caller: Caller,
        approval: Approval,
    ) -> Enforced {
        let sql = match caller {
            Caller::User => sql.to_string(),
            Caller::Schedule | Caller::Editor | Caller::Assistant => guard::read_text(sql, lexing),
        };
        let run = |run| Enforced::Run {
            run,
            sql: sql.clone(),
        };
        match decide_with(&self.settings, Some(db_user), &sql, lexing, caller) {
            Decision::Run(r) => run(r),
            Decision::Refuse(reason) => Enforced::Refused(reason),
            Decision::Propose(proposal) => Enforced::Proposed(proposal),
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
                (Caller::User, Approval::Confirmed) => run(then),
                _ => Enforced::Refused(
                    "UPDATE or DELETE without WHERE needs to be confirmed first".into(),
                ),
            },
        }
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

    impl Gate {
        /// [`Gate::enforce`] with the default lexical rules.
        fn enforce_default(
            &self,
            db_user: &str,
            sql: &str,
            caller: Caller,
            approval: Approval,
        ) -> Enforced {
            self.enforce(db_user, sql, Lexing::default(), caller, approval)
        }
    }

    /// The text a reader's statement is sent as, or why it isn't.
    fn sent(sql: &str, lexing: Lexing) -> Result<String, String> {
        match gate("{}").enforce("rw", sql, lexing, Caller::Assistant, Approval::None) {
            Enforced::Run { sql, .. } => Ok(sql),
            Enforced::Refused(reason) => Err(reason),
            Enforced::Proposed(p) => Err(format!("proposed {}", p.statements[0].0)),
        }
    }

    fn lexing(kind: tangul_core::DriverKind) -> Lexing {
        tangul_assist::dialect::dialect_for(kind).lexing()
    }

    #[test]
    fn readers_send_what_was_checked() {
        use tangul_core::DriverKind::{Mysql, Postgres};
        // Comments are blanked out (same length) and a trailing `;` dropped.
        assert_eq!(
            sent("select 1 -- why\n/* note */ + 2;\n", lexing(Postgres)),
            Ok("select 1       \n           + 2".into())
        );
        // MySQL runs `/*! … */`: it never reaches the server.
        assert_eq!(
            sent("SELECT 1 /*! INTO OUTFILE '/tmp/x' */", lexing(Mysql)),
            Ok("SELECT 1".into())
        );
        assert_eq!(
            sent(
                "SELECT 1; /*! COMMIT */; /*! SET SESSION TRANSACTION READ WRITE */; \
                 /*! DELETE FROM t */",
                lexing(Mysql)
            ),
            Ok("SELECT 1".into())
        );
        // `#` is a comment on MySQL: what follows the line is checked as the server
        // reads it.
        assert_eq!(
            sent("SELECT 1 # '\n INTO OUTFILE '/tmp/x'", lexing(Mysql)),
            Err("proposed SELECT INTO".into())
        );
    }

    #[test]
    fn readers_send_one_statement() {
        use tangul_core::DriverKind::Postgres;
        for sql in [
            "select 1; select 2",
            // Postgres ends the string at `\'`; the guard's tokens may not.
            "SELECT '\\'; COMMIT; SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE; \
             DELETE FROM t; --'",
            "select ';'",
        ] {
            let reason = sent(sql, lexing(Postgres)).unwrap_err();
            assert!(reason.starts_with("only one statement"), "{sql}: {reason}");
        }
        for sql in [
            "SELECT U&\"d\\0062link_exec\"('x', 'drop table t')",
            "select u&'\\0041'",
        ] {
            let reason = sent(sql, lexing(Postgres)).unwrap_err();
            assert!(reason.starts_with("U& escapes"), "{sql}: {reason}");
        }
        // Schedules too.
        assert!(matches!(
            gate("{}").enforce_default(
                "rw",
                "select 1; select 2",
                Caller::Schedule,
                Approval::None
            ),
            Enforced::Refused(_)
        ));
        // The user's statements are sent as they are.
        let Enforced::Run { sql, .. } =
            gate("{}").enforce_default("rw", "select 1; -- x", Caller::User, Approval::None)
        else {
            panic!("runs");
        };
        assert_eq!(sql, "select 1; -- x");
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
        let g = gate(r#"{"safety": {"read_only_users": ["ro"]}}"#);
        assert!(matches!(
            g.enforce_default(
                "rw",
                "delete from t where id = 1",
                Caller::User,
                Approval::None
            ),
            Enforced::Run {
                run: Run {
                    read_only: false,
                    ..
                },
                ..
            }
        ));
        // Without WHERE: only after the dialog.
        assert!(matches!(
            g.enforce_default("rw", "delete from t", Caller::User, Approval::None),
            Enforced::Refused(_)
        ));
        assert!(matches!(
            g.enforce_default("rw", "delete from t", Caller::User, Approval::Confirmed),
            Enforced::Run { .. }
        ));
        // Read-only mode: a go-ahead doesn't lift it.
        assert!(matches!(
            g.enforce_default(
                "ro",
                "delete from t where id = 1",
                Caller::User,
                Approval::Confirmed
            ),
            Enforced::Refused(_)
        ));
    }

    #[test]
    fn schedules_and_the_editor_only_read() {
        let g = gate("{}");
        for caller in [Caller::Schedule, Caller::Editor] {
            assert!(matches!(
                g.enforce_default("rw", "select * from t", caller, Approval::None),
                Enforced::Run {
                    run: Run {
                        read_only: true,
                        mode: Mode::RolledBack,
                        ..
                    },
                    ..
                }
            ));
            for sql in [
                "delete from t where id = 1",
                "select nextval('s')",
                "set x = 1",
                "select 1; select 2",
            ] {
                assert!(
                    matches!(
                        g.enforce_default("rw", sql, caller, Approval::Confirmed),
                        Enforced::Refused(_)
                    ),
                    "{sql}"
                );
            }
        }
        assert!(matches!(
            g.enforce_default("rw", "delete from t where id = 1", Caller::Editor, Approval::None),
            Enforced::Refused(r) if r.contains("for the editor")
        ));
    }

    #[test]
    fn assistant_writes_are_proposals() {
        let g = gate(r#"{"safety": {"read_only_users": ["ro"]}}"#);
        assert!(matches!(
            g.enforce_default("rw", "select * from t", Caller::Assistant, Approval::None),
            Enforced::Run {
                run: Run {
                    read_only: true,
                    mode: Mode::RolledBack,
                    ..
                },
                ..
            }
        ));
        // Anything that isn't a plain read, however it is dressed up, and whatever
        // approval comes along.
        for sql in [
            "update t set a = 1 where id = 2",
            "with gone as (delete from t returning *) select * from gone",
            "select * from t for update",
            "select nextval('s')",
            "select 1; drop table t",
        ] {
            for user in ["rw", "ro"] {
                let Enforced::Proposed(p) =
                    g.enforce_default(user, sql, Caller::Assistant, Approval::Confirmed)
                else {
                    panic!("{sql} is proposed");
                };
                assert!(!p.statements.is_empty(), "{sql}");
            }
        }
        let Enforced::Proposed(p) = g.enforce_default(
            "rw",
            "select 1; delete from t where id = 1; insert into t values (1)",
            Caller::Assistant,
            Approval::None,
        ) else {
            panic!("proposed");
        };
        let keywords: Vec<&str> = p.statements.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keywords, ["DELETE", "INSERT"]);
    }

    #[test]
    fn reads_the_settings_file_itself() {
        let dir = std::env::temp_dir().join(format!("tangul-gate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let _ = std::fs::remove_file(&path);
        let mut g = Gate::new(SettingsSource::File(Some(path.clone())));
        // No file: defaults (write mode, 60s).
        assert!(matches!(
            g.enforce_default("u", "select 1", Caller::User, Approval::None),
            Enforced::Run {
                run: Run {
                    read_only: false,
                    ..
                },
                ..
            }
        ));
        std::fs::write(&path, r#"{"safety": {"read_only_users": ["u"]}}"#).unwrap();
        g.reload_now();
        assert!(matches!(
            g.enforce_default("u", "select 1", Caller::User, Approval::None),
            Enforced::Run {
                run: Run {
                    read_only: true,
                    ..
                },
                ..
            }
        ));
        // A broken file keeps what was in effect.
        std::fs::write(&path, "{broken").unwrap();
        g.reload_now();
        assert!(matches!(
            g.enforce_default("u", "select 1", Caller::User, Approval::None),
            Enforced::Run {
                run: Run {
                    read_only: true,
                    ..
                },
                ..
            }
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
