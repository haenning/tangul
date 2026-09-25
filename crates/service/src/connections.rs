//! Every connection the app opened and what it is for, and connecting a saved
//! database user (profile, Keychain secret, SSH tunnel) or a local file.

use std::collections::HashMap;

use tangul_core::{ConnId, ConnectConfig, DriverKind, TlsMode};
use tangul_data::profiles::Profiles;
use tangul_engine::{Command, EngineHandle};

/// What a connection is for. Each database user has at most one per purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Purpose {
    /// Tabs and the schema sidebar; the one the UI shows.
    Tab,
    /// Scheduled bookmark runs; hidden.
    Background,
    /// The AI assistant; hidden, read-only.
    Assistant,
    /// A local SQLite file of the app's own (metrics.db), read-only.
    LocalFile,
}

/// Open connections by id, and by purpose and key (database user id, or the path of
/// a local file).
#[derive(Default)]
pub struct Connections {
    last_id: u64,
    open: HashMap<ConnId, (Purpose, String)>,
    by_key: HashMap<(Purpose, String), ConnId>,
}

impl Connections {
    /// The connection for `purpose` and `key`, if one was opened.
    pub fn get(&self, purpose: Purpose, key: &str) -> Option<ConnId> {
        self.by_key.get(&(purpose, key.to_string())).copied()
    }

    pub fn purpose(&self, conn: ConnId) -> Option<Purpose> {
        self.open.get(&conn).map(|(purpose, _)| *purpose)
    }

    /// The database user id (or file path) `conn` belongs to.
    pub fn key(&self, conn: ConnId) -> Option<&str> {
        self.open.get(&conn).map(|(_, key)| key.as_str())
    }

    /// The database user id of a [`Purpose::Tab`] connection.
    pub fn tab_user(&self, conn: ConnId) -> Option<&str> {
        match self.open.get(&conn)? {
            (Purpose::Tab, user) => Some(user),
            _ => None,
        }
    }

    /// The id for `purpose` and `key`: the one it has, or a new one.
    fn id_for(&mut self, purpose: Purpose, key: &str) -> ConnId {
        if let Some(id) = self.get(purpose, key) {
            return id;
        }
        self.last_id += 1;
        let id = ConnId(self.last_id);
        self.open.insert(id, (purpose, key.to_string()));
        self.by_key.insert((purpose, key.to_string()), id);
        id
    }

    /// Connects `user_id` of a saved database for `purpose` (again, keeping its id, if
    /// it had one). `None` when the user isn't saved.
    pub fn connect_user(
        &mut self,
        engine: &EngineHandle,
        profiles: &mut Profiles,
        user_id: &str,
        purpose: Purpose,
    ) -> Option<ConnId> {
        let (db, user) = profiles
            .user(user_id)
            .map(|(db, user)| (db.clone(), user.clone()))?;
        let password = profiles.password(user_id);
        let ssh = profiles.ssh_target(&db);
        let id = self.id_for(purpose, user_id);
        engine.send(Command::Connect {
            conn: id,
            driver: db.driver,
            cfg: ConnectConfig::new(&db, &user, password),
            ssh,
        });
        Some(id)
    }

    /// Connects the AI assistant for `user_id`'s database: as the database's assistant
    /// user if one is set (and still saved), else as `user_id`.
    pub fn connect_assistant(
        &mut self,
        engine: &EngineHandle,
        profiles: &mut Profiles,
        user_id: &str,
    ) -> Option<ConnId> {
        let as_user = assistant_user(profiles, user_id)?;
        if let Some(conn) = self.get(Purpose::Assistant, &as_user) {
            return Some(conn);
        }
        self.connect_user(engine, profiles, &as_user, Purpose::Assistant)
    }

    /// The connection to a local SQLite file, opened read-only if it isn't yet.
    /// Returns its id and whether it was opened just now.
    pub fn open_file(&mut self, engine: &EngineHandle, path: &str) -> (ConnId, bool) {
        if let Some(id) = self.get(Purpose::LocalFile, path) {
            return (id, false);
        }
        let id = self.id_for(Purpose::LocalFile, path);
        engine.send(Command::Connect {
            conn: id,
            driver: DriverKind::Sqlite,
            cfg: ConnectConfig {
                host: String::new(),
                port: 0,
                user: String::new(),
                password: None,
                database: path.to_string(),
                tls: TlsMode::Disable,
            },
            ssh: None,
        });
        (id, true)
    }

    /// Forgets a connection (after it failed); the next use connects again.
    pub fn forget(&mut self, conn: ConnId) {
        if let Some(key) = self.open.remove(&conn) {
            self.by_key.remove(&key);
        }
    }
}

/// The user (id) the assistant connects as for `user_id`'s database.
pub fn assistant_user(profiles: &Profiles, user_id: &str) -> Option<String> {
    let (db, _) = profiles.user(user_id)?;
    let chosen = db
        .assistant_user
        .as_deref()
        .filter(|id| db.users.iter().any(|u| u.id == *id));
    Some(chosen.unwrap_or(user_id).to_string())
}

#[cfg(test)]
mod tests {
    use tangul_core::{Database, DbUser, QueryId};
    use tangul_engine::{Caller, QueryOptions};

    use super::*;

    /// A fresh local.db (a SQLite file with a `bookmarks` table).
    fn local_db() -> String {
        let dir = std::env::temp_dir().join(format!("tangul-conns-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        tangul_data::local::Local::open(Some(&dir));
        dir.join("local.db").to_string_lossy().into_owned()
    }

    #[test]
    fn the_assistant_uses_the_chosen_user() {
        let user = |id: &str| DbUser {
            id: id.into(),
            user: id.into(),
        };
        let db = |assistant: Option<&str>| Database {
            id: "db".into(),
            name: String::new(),
            driver: DriverKind::Postgres,
            host: String::new(),
            port: 5432,
            database: String::new(),
            tls: TlsMode::Disable,
            users: vec![user("admin"), user("reader")],
            ssh: None,
            assistant_user: assistant.map(Into::into),
        };
        let profiles = Profiles::in_memory(vec![db(Some("reader"))]);
        assert_eq!(
            assistant_user(&profiles, "admin").as_deref(),
            Some("reader")
        );
        // Not set, or a user that no longer exists: the selected user.
        let profiles = Profiles::in_memory(vec![db(None)]);
        assert_eq!(assistant_user(&profiles, "admin").as_deref(), Some("admin"));
        let profiles = Profiles::in_memory(vec![db(Some("gone"))]);
        assert_eq!(assistant_user(&profiles, "admin").as_deref(), Some("admin"));
        assert_eq!(assistant_user(&profiles, "nobody"), None);
    }

    #[test]
    fn ids_by_purpose_and_key() {
        let mut c = Connections::default();
        let a = c.id_for(Purpose::Tab, "u1");
        let b = c.id_for(Purpose::Background, "u1");
        assert_ne!(a, b);
        assert_eq!(c.id_for(Purpose::Tab, "u1"), a);
        assert_eq!(c.purpose(b), Some(Purpose::Background));
        assert_eq!(c.tab_user(a), Some("u1"));
        assert_eq!(c.tab_user(b), None);
        c.forget(b);
        assert_eq!(c.get(Purpose::Background, "u1"), None);
        assert_ne!(c.id_for(Purpose::Background, "u1"), b);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connects_a_saved_user_and_a_local_file() {
        let path = local_db();
        let (engine, _events) = tangul_engine::start_with(tangul_engine::EngineConfig::isolated());
        let mut profiles = Profiles::in_memory(vec![Database {
            id: "db".into(),
            name: "local".into(),
            driver: DriverKind::Sqlite,
            host: String::new(),
            port: 0,
            database: path.clone(),
            tls: TlsMode::Disable,
            users: vec![DbUser {
                id: "u".into(),
                user: String::new(),
            }],
            ssh: None,
            assistant_user: None,
        }]);
        let mut c = Connections::default();
        assert_eq!(
            c.connect_user(&engine, &mut profiles, "nobody", Purpose::Tab),
            None
        );
        let conn = c.connect_assistant(&engine, &mut profiles, "u").unwrap();
        assert_eq!(c.get(Purpose::Assistant, "u"), Some(conn));
        assert_eq!(c.connect_assistant(&engine, &mut profiles, "u"), Some(conn));
        let options = QueryOptions::new(Caller::Assistant);
        let out = engine
            .query(
                QueryId(1),
                conn,
                "SELECT count(*) FROM bookmarks",
                options.clone(),
            )
            .await
            .unwrap();
        assert_eq!(out.results[0].cell(0, 0), Some("0"));

        let (file, new) = c.open_file(&engine, &path);
        assert!(new);
        assert_eq!(c.open_file(&engine, &path), (file, false));
        assert_eq!(c.purpose(file), Some(Purpose::LocalFile));
        let out = engine
            .query(QueryId(2), file, "SELECT 1", options)
            .await
            .unwrap();
        assert_eq!(out.results[0].cell(0, 0), Some("1"));
    }
}
