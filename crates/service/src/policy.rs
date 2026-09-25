//! The one place that decides whether and how a statement runs, for every caller:
//! read-only mode, the `UPDATE` / `DELETE` without `WHERE` check and the time limit
//! from `settings.json`.

use std::time::Duration;

use tangul_assist::guard;
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
}

/// How a statement runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    /// The server refuses writes.
    pub read_only: bool,
    /// Stopped after this long.
    pub timeout: Option<Duration>,
}

/// Why the user is asked first; the statements are (keyword, one line of SQL).
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Run(Run),
    /// Only for [`Caller::User`]: ask first. Allowing a [`Guard::Unfiltered`] runs
    /// `then`; allowing [`Guard::ReadOnly`] only lifts read-only mode.
    Confirm {
        guard: Guard,
        then: Run,
    },
    /// Never runs for this caller.
    Refuse(String),
}

/// Length at which a statement is cut for display.
const STATEMENT_LEN: usize = 160;

/// How the statements in `sql` may run for `caller`, as `db_user` (the database user
/// name, which read-only mode goes by).
pub fn decide(settings: &Settings, db_user: Option<&str>, sql: &str, caller: Caller) -> Decision {
    let checks = guard::check(sql);
    let described = |keep: &dyn Fn(&guard::Check) -> bool| -> Vec<(String, String)> {
        checks
            .iter()
            .filter(|c| keep(c))
            .map(|c| (c.keyword.clone(), one_line(&c.statement, STATEMENT_LEN)))
            .collect()
    };
    let timeout = settings.safety.max_query_time;
    match caller {
        Caller::Schedule | Caller::Assistant => {
            let writes = described(&|c| c.writes);
            match writes.first() {
                Some((keyword, _)) => Decision::Refuse(format!(
                    "only statements that read may run here; this one is {keyword}"
                )),
                None => Decision::Run(Run {
                    read_only: true,
                    timeout,
                }),
            }
        }
        Caller::User => {
            let run = user_run(settings, db_user);
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(json: &str) -> Settings {
        Settings::parse(json).unwrap()
    }

    #[test]
    fn users_run_with_their_mode_and_the_time_limit() {
        let s = settings(r#"{"safety": {"read_only_users": ["ro"], "max_query_time": "5s"}}"#);
        let limit = Some(Duration::from_secs(5));
        assert_eq!(
            decide(&s, Some("rw"), "select 1", Caller::User),
            Decision::Run(Run {
                read_only: false,
                timeout: limit
            })
        );
        assert_eq!(
            decide(&s, Some("ro"), "select 1", Caller::User),
            Decision::Run(Run {
                read_only: true,
                timeout: limit
            })
        );
        // Writes in read-only mode: ask first.
        match decide(&s, Some("ro"), "delete from t where id = 1", Caller::User) {
            Decision::Confirm {
                guard: Guard::ReadOnly { user, statements },
                then,
            } => {
                assert_eq!(user, "ro");
                assert_eq!(statements[0].0, "DELETE");
                assert!(then.read_only);
            }
            other => panic!("{other:?}"),
        }
        // Without WHERE: ask first; with it: run.
        assert!(matches!(
            decide(&s, Some("rw"), "delete from t", Caller::User),
            Decision::Confirm {
                guard: Guard::Unfiltered { .. },
                ..
            }
        ));
        let off = settings(r#"{"safety": {"confirm_unfiltered_writes": false}}"#);
        assert!(matches!(
            decide(&off, Some("rw"), "delete from t", Caller::User),
            Decision::Run(_)
        ));
    }

    #[test]
    fn schedules_and_the_assistant_only_read() {
        let s = settings("{}");
        for caller in [Caller::Schedule, Caller::Assistant] {
            assert_eq!(
                decide(&s, Some("rw"), "select * from t", caller),
                Decision::Run(Run {
                    read_only: true,
                    timeout: Some(Duration::from_secs(60))
                })
            );
            for write in [
                "update t set a = 1 where id = 2",
                "drop table t",
                "select 1; insert into t values (1)",
            ] {
                assert!(
                    matches!(decide(&s, Some("rw"), write, caller), Decision::Refuse(_)),
                    "{write}"
                );
            }
        }
    }
}
