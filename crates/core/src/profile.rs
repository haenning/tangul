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

/// A saved connection. Multiple profiles for the same server (different users)
/// are simply multiple profiles. The password is never stored here (see Keychain).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// Stable id, used as Keychain account name.
    pub id: String,
    pub name: String,
    pub driver: DriverKind,
    pub host: String,
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub tls: TlsMode,
    // TODO(ssh): optional SSH tunnel settings (host, port, user, auth: password|key|agent).
}

/// Everything a driver needs to open a session. `host`/`port` may point to a local
/// tunnel endpoint instead of the profile's host later on.
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
    pub fn from_profile(profile: &Profile, password: Option<String>) -> Self {
        Self {
            host: profile.host.clone(),
            port: profile.port,
            user: profile.user.clone(),
            password,
            database: profile.database.clone(),
            tls: profile.tls,
        }
    }
}
