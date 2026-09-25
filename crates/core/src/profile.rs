use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DriverKind {
    Postgres,
    Mysql,
    /// Only the app's own data (bookmark metrics), read-only; not for profiles.
    Sqlite,
}

impl DriverKind {
    pub fn default_port(self) -> u16 {
        match self {
            DriverKind::Postgres => 5432,
            DriverKind::Mysql => 3306,
            DriverKind::Sqlite => 0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DriverKind::Postgres => "PostgreSQL",
            DriverKind::Mysql => "MySQL",
            DriverKind::Sqlite => "SQLite",
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
    /// Reach the database through an SSH tunnel (`host`/`port` are then as seen
    /// from the SSH server).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<SshConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SshAuth {
    /// Keys from the running ssh-agent (`SSH_AUTH_SOCK`).
    #[default]
    Agent,
    /// A private key file, optionally with a passphrase (Keychain).
    Key,
    /// Password (Keychain).
    Password,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SshConfig {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub auth: SshAuth,
    /// Private key for [`SshAuth::Key`]; `~` expands to the home directory.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key_path: String,
}

fn default_ssh_port() -> u16 {
    22
}

impl SshConfig {
    /// Keychain account of the SSH password or key passphrase.
    pub fn secret_account(db_id: &str) -> String {
        format!("ssh:{db_id}")
    }
}

/// Everything the engine needs to open a tunnel: SSH settings plus the secret.
#[derive(Clone)]
pub struct SshTarget {
    pub config: SshConfig,
    /// Password or key passphrase.
    pub secret: Option<String>,
}

impl std::fmt::Debug for SshTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshTarget")
            .field("config", &self.config)
            .field("secret", &self.secret.as_ref().map(|_| "***"))
            .finish()
    }
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

/// Everything a driver needs to open a session. With an SSH tunnel the engine points
/// `host`/`port` at the tunnel's local endpoint.
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
