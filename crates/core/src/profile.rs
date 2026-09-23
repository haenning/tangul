use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DriverKind {
    Postgres,
    Mysql,
}

impl DriverKind {
    pub fn default_port(self) -> u16 {
        match self {
            DriverKind::Postgres => 5432,
            DriverKind::Mysql => 3306,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DriverKind::Postgres => "PostgreSQL",
            DriverKind::Mysql => "MySQL",
        }
    }
}

/// TLS mode with libpq semantics: `Prefer`/`Require` encrypt but do not verify the
/// server certificate (managed DBs often use private CAs). Verification is a later step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    Disable,
    #[default]
    Prefer,
    Require,
}

/// A saved database: where to connect. The credentials hang off it as [`DbUser`]s,
/// so one server/database can be used with several logins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Database {
    pub id: String,
    pub name: String,
    pub driver: DriverKind,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub tls: TlsMode,
    #[serde(default)]
    pub users: Vec<DbUser>,
    // TODO(ssh): optional SSH tunnel settings (host, port, user, auth: password|key|agent).
}

impl Database {
    /// The given name, or `host/database` when none was set.
    pub fn display_name(&self) -> String {
        if !self.name.is_empty() {
            return self.name.clone();
        }
        if self.database.is_empty() {
            self.host.clone()
        } else {
            format!("{}/{}", self.host, self.database)
        }
    }

    pub fn user(&self, id: &str) -> Option<&DbUser> {
        self.users.iter().find(|u| u.id == id)
    }
}

/// A login for a [`Database`]. The password is never stored here (see Keychain).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DbUser {
    /// Stable id, used as Keychain account name.
    pub id: String,
    pub user: String,
}

/// Everything a driver needs to open a session. `host`/`port` may point to a local
/// tunnel endpoint instead of the database's host later on.
#[derive(Debug, Clone)]
pub struct ConnectConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub database: String,
    pub tls: TlsMode,
}

impl ConnectConfig {
    pub fn new(db: &Database, user: &DbUser, password: Option<String>) -> Self {
        Self {
            host: db.host.clone(),
            port: db.port,
            user: user.user.clone(),
            password,
            database: db.database.clone(),
            tls: db.tls,
        }
    }
}
