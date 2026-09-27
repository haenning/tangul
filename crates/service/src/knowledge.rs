//! What tangul knows about the databases, in one place for every reader: the
//! schema of each connected database user and the learned usage of each database.
//! The app's editor, schema tree and results read it, and so will the assistant's
//! tools; nothing keeps a copy of its own.
//!
//! Thread-safe and cheap to clone (a handle to the same pool), so readers off the
//! UI thread (completion, tools) can hold it.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant};

use tangul_assist::schema::SchemaIndex;
use tangul_assist::usage::Usage;
use tangul_core::{ColumnProfile, ConnId};

/// How long a value profile is used before it is sampled again.
pub const PROFILE_TTL: Duration = Duration::from_secs(60 * 60);

/// A column: (schema, table, column), as the catalog spells them.
pub type ColumnRef = (String, String, String);

/// Handle to the pool.
#[derive(Clone, Default)]
pub struct Knowledge(Arc<RwLock<Pool>>);

#[derive(Default)]
struct Pool {
    /// By database user id: stays the same when the connection is reopened.
    users: HashMap<String, UserKnowledge>,
    /// Learned usage by database id (shared by the database's users).
    usage: HashMap<String, Arc<Usage>>,
    /// Value profiles by database user and column, with when they were taken. Only
    /// filled where the user allowed the assistant to read values.
    profiles: HashMap<(String, ColumnRef), (ColumnProfile, Instant)>,
    /// By database id: how many tables the AI's database user may change or drop
    /// (as the server answered); unknown until checked.
    ai_writable: HashMap<String, u64>,
}

/// What is known for one database user.
#[derive(Clone)]
pub struct UserKnowledge {
    pub database: String,
    /// The connection it is open on now.
    pub conn: ConnId,
    pub schema: Arc<SchemaIndex>,
}

impl Knowledge {
    fn read<T>(&self, f: impl FnOnce(&Pool) -> T) -> T {
        f(&self.0.read().unwrap_or_else(PoisonError::into_inner))
    }

    fn write<T>(&self, f: impl FnOnce(&mut Pool) -> T) -> T {
        f(&mut self.0.write().unwrap_or_else(PoisonError::into_inner))
    }

    /// `user` of `database` is (being) opened as `conn` with `schema`: from the cache
    /// until the server answers, then the server's.
    pub fn set_schema(&self, user: &str, database: &str, conn: ConnId, schema: Arc<SchemaIndex>) {
        self.write(|p| {
            p.users.insert(
                user.to_string(),
                UserKnowledge {
                    database: database.to_string(),
                    conn,
                    schema,
                },
            )
        });
    }

    /// `user`'s connection was opened again as `conn`; what is known stays.
    pub fn set_conn(&self, user: &str, conn: ConnId) {
        self.write(|p| {
            if let Some(u) = p.users.get_mut(user) {
                u.conn = conn;
            }
        });
    }

    pub fn forget_user(&self, user: &str) {
        self.write(|p| {
            p.users.remove(user);
            p.profiles.retain(|(u, _), _| u != user);
        });
    }

    /// How many tables the AI's user of `database` may change or drop, if checked.
    pub fn ai_writable(&self, database: &str) -> Option<u64> {
        self.read(|p| p.ai_writable.get(database).copied())
    }

    pub fn set_ai_writable(&self, database: &str, tables: u64) {
        self.write(|p| p.ai_writable.insert(database.to_string(), tables));
    }

    /// `user`'s profile of `column`, unless it is older than [`PROFILE_TTL`].
    pub fn profile(&self, user: &str, column: &ColumnRef) -> Option<ColumnProfile> {
        self.read(|p| {
            let (profile, at) = p.profiles.get(&(user.to_string(), column.clone()))?;
            (at.elapsed() < PROFILE_TTL).then(|| profile.clone())
        })
    }

    pub fn set_profile(&self, user: &str, column: ColumnRef, profile: ColumnProfile) {
        self.write(|p| {
            p.profiles
                .insert((user.to_string(), column), (profile, Instant::now()))
        });
    }

    /// The most frequent values of `user`'s columns with a fresh profile.
    pub fn known_values(&self, user: &str) -> HashMap<ColumnRef, Vec<String>> {
        self.read(|p| {
            p.profiles
                .iter()
                .filter(|((u, _), (_, at))| u == user && at.elapsed() < PROFILE_TTL)
                .map(|((_, c), (profile, _))| {
                    (
                        c.clone(),
                        profile.top.iter().map(|(v, _)| v.clone()).collect(),
                    )
                })
                .collect()
        })
    }

    pub fn user(&self, user: &str) -> Option<UserKnowledge> {
        self.read(|p| p.users.get(user).cloned())
    }

    /// The schema of `user`, if it was opened.
    pub fn schema(&self, user: &str) -> Option<Arc<SchemaIndex>> {
        self.read(|p| p.users.get(user).map(|u| u.schema.clone()))
    }

    /// The schema and usage behind connection `conn`.
    pub fn for_conn(&self, conn: ConnId) -> Option<(Arc<SchemaIndex>, Arc<Usage>)> {
        self.read(|p| {
            let u = p.users.values().find(|u| u.conn == conn)?;
            let usage = p.usage.get(&u.database).cloned().unwrap_or_default();
            Some((u.schema.clone(), usage))
        })
    }

    pub fn usage(&self, database: &str) -> Option<Arc<Usage>> {
        self.read(|p| p.usage.get(database).cloned())
    }

    pub fn set_usage(&self, database: &str, usage: Arc<Usage>) {
        self.write(|p| p.usage.insert(database.to_string(), usage));
    }

    /// Every opened database user: (user id, what is known).
    pub fn users(&self) -> Vec<(String, UserKnowledge)> {
        let mut users: Vec<_> = self.read(|p| {
            p.users
                .iter()
                .map(|(id, u)| (id.clone(), u.clone()))
                .collect()
        });
        users.sort_by(|a, b| a.0.cmp(&b.0));
        users
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tangul_assist::dialect::dialect_for;
    use tangul_core::{DriverKind, SchemaInfo, TableInfo, TableKind};

    fn schema(table: &str) -> Arc<SchemaIndex> {
        let info = SchemaInfo {
            tables: vec![TableInfo {
                schema: "public".into(),
                name: table.into(),
                kind: TableKind::Table,
                ..Default::default()
            }],
            ..Default::default()
        };
        Arc::new(SchemaIndex::new(
            info,
            dialect_for(DriverKind::Postgres),
            None,
        ))
    }

    fn tables(s: &SchemaIndex) -> Vec<String> {
        s.tables().iter().map(|t| t.name.clone()).collect()
    }

    #[test]
    fn one_pool_for_every_handle() {
        let k = Knowledge::default();
        let reader = k.clone();
        k.set_schema("u1", "db1", ConnId(1), schema("users"));
        assert_eq!(tables(&reader.schema("u1").unwrap()), ["users"]);
        // The server's schema replaces the cached one.
        k.set_schema("u1", "db1", ConnId(1), schema("orders"));
        assert_eq!(tables(&reader.schema("u1").unwrap()), ["orders"]);
        assert!(reader.schema("u2").is_none());
    }

    #[test]
    fn a_reopened_connection_keeps_what_is_known() {
        let k = Knowledge::default();
        k.set_schema("u1", "db1", ConnId(1), schema("users"));
        k.set_usage("db1", Arc::new(Usage::default()));
        k.set_conn("u1", ConnId(7));
        assert!(k.for_conn(ConnId(1)).is_none());
        let (s, _) = k.for_conn(ConnId(7)).unwrap();
        assert_eq!(tables(&s), ["users"]);
        assert_eq!(k.user("u1").unwrap().database, "db1");
        // Unknown users are ignored.
        k.set_conn("nobody", ConnId(9));
        assert_eq!(k.users().len(), 1);
        k.forget_user("u1");
        assert!(k.users().is_empty());
    }

    #[test]
    fn profiles_per_user_until_they_expire() {
        let k = Knowledge::default();
        let col = (
            "app".to_string(),
            "orders".to_string(),
            "status".to_string(),
        );
        let profile = ColumnProfile {
            rows: 4,
            top: vec![("paid".into(), 2), ("open".into(), 1)],
            ..Default::default()
        };
        k.set_profile("u1", col.clone(), profile.clone());
        assert_eq!(k.profile("u1", &col), Some(profile));
        assert_eq!(k.profile("u2", &col), None);
        assert_eq!(k.known_values("u1")[&col], ["paid", "open"]);
        assert!(k.known_values("u2").is_empty());
        k.forget_user("u1");
        assert_eq!(k.profile("u1", &col), None);
    }

    #[test]
    fn usage_is_per_database() {
        let k = Knowledge::default();
        k.set_schema("u1", "db1", ConnId(1), schema("users"));
        k.set_schema("u2", "db1", ConnId(2), schema("users"));
        assert!(k.usage("db1").is_none());
        let usage = Arc::new(Usage::default());
        k.set_usage("db1", usage.clone());
        // Both users of the database see the same usage.
        for conn in [ConnId(1), ConnId(2)] {
            assert!(Arc::ptr_eq(&k.for_conn(conn).unwrap().1, &usage));
        }
        assert_eq!(
            k.users()
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            ["u1", "u2"]
        );
    }
}
