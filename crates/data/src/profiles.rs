//! Saved databases (each with its users) as TOML in the config dir
//! (`~/Library/Application Support/tangul/profiles.toml` on macOS, overridable with
//! `TANGUL_CONFIG_DIR`, used by `make run-dev`).
//! Passwords go to the Keychain (one entry per user), never into the file.

use std::{collections::HashMap, path::PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use tangul_core::{Database, DbUser, DriverKind, SshAuth, SshConfig, SshTarget, TlsMode};

use crate::config::{LEGACY_NAME, config_dir};

const KEYCHAIN_SERVICE: &str = "tangul";

#[derive(Default, Serialize, Deserialize)]
struct ProfilesFile {
    #[serde(default)]
    databases: Vec<Database>,
    /// Pre-0.2 format: one flat entry per server+user. Migrated on load.
    #[serde(default, skip_serializing)]
    profiles: Vec<LegacyProfile>,
}

#[derive(Deserialize)]
struct LegacyProfile {
    id: String,
    name: String,
    driver: DriverKind,
    host: String,
    port: u16,
    user: String,
    #[serde(default)]
    database: String,
    #[serde(default)]
    tls: TlsMode,
}

/// Groups legacy profiles by connection target. The profile id becomes the user id,
/// so passwords already in the Keychain keep working.
fn migrate(profiles: Vec<LegacyProfile>, databases: &mut Vec<Database>) {
    for p in profiles {
        let user = DbUser {
            id: p.id.clone(),
            user: p.user,
        };
        let existing = databases.iter_mut().find(|d| {
            d.driver == p.driver && d.host == p.host && d.port == p.port && d.database == p.database
        });
        match existing {
            Some(db) => {
                if db.name.is_empty() {
                    db.name = p.name;
                }
                db.users.push(user);
            }
            None => databases.push(Database {
                id: p.id,
                name: p.name,
                driver: p.driver,
                host: p.host,
                port: p.port,
                database: p.database,
                tls: p.tls,
                users: vec![user],
                ssh: None,
                assistant_user: None,
                ai_values: Default::default(),
                ai_access: Default::default(),
                ai_tools: true,
                editor_values: true,
            }),
        }
    }
}

pub struct Profiles {
    path: Option<PathBuf>,
    pub databases: Vec<Database>,
    /// Session cache so the Keychain is hit once per profile, and so passwords work
    /// on platforms where only the mock keyring store is available.
    passwords: HashMap<String, String>,
    /// [`Self::in_memory`]: never saved, and secrets stay in the session cache, so
    /// tests never read or overwrite the real Keychain.
    in_memory: bool,
}

impl Profiles {
    pub fn load() -> Self {
        let path = config_dir().map(|d| d.join("profiles.toml"));
        let file = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| match toml::from_str::<ProfilesFile>(&s) {
                Ok(f) => Some(f),
                Err(e) => {
                    eprintln!("ignoring invalid profiles file: {e}");
                    None
                }
            })
            .unwrap_or_default();
        let migrated = !file.profiles.is_empty();
        let mut databases = file.databases;
        migrate(file.profiles, &mut databases);
        let store = Self {
            path,
            databases,
            passwords: HashMap::new(),
            in_memory: false,
        };
        if migrated && let Err(e) = store.save() {
            eprintln!("could not write migrated profiles file: {e:#}");
        }
        store
    }

    pub fn database(&self, id: &str) -> Option<&Database> {
        self.databases.iter().find(|d| d.id == id)
    }

    /// Profiles that are never saved, with `dir` as their folder: the settings
    /// files next to them are read there (tests).
    pub fn in_memory_at(dir: &std::path::Path, databases: Vec<Database>) -> Self {
        Self {
            path: Some(dir.join("profiles.toml")),
            ..Self::in_memory(databases)
        }
    }

    /// Profiles that are never saved (tests, tools).
    pub fn in_memory(databases: Vec<Database>) -> Self {
        Self {
            path: None,
            databases,
            passwords: HashMap::new(),
            in_memory: true,
        }
    }

    /// A database user by id, with its database.
    pub fn user(&self, user_id: &str) -> Option<(&Database, &DbUser)> {
        self.databases
            .iter()
            .find_map(|db| Some((db, db.users.iter().find(|u| u.id == user_id)?)))
    }

    pub fn save(&self) -> Result<()> {
        if self.in_memory {
            return Ok(());
        }
        let path = self.path.as_ref().context("no config directory")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = ProfilesFile {
            databases: self.databases.clone(),
            profiles: Vec::new(),
        };
        std::fs::write(path, toml::to_string_pretty(&file)?)?;
        Ok(())
    }

    /// Inserts or replaces a database's connection settings (matched by id).
    /// The users of an existing database are kept.
    pub fn upsert_database(&mut self, mut db: Database) -> Result<()> {
        match self.databases.iter_mut().find(|d| d.id == db.id) {
            Some(existing) => {
                db.users = std::mem::take(&mut existing.users);
                *existing = db;
            }
            None => self.databases.push(db),
        }
        self.save()
    }

    /// Inserts or replaces a user of a database (matched by id) and stores its password.
    pub fn upsert_user(
        &mut self,
        db_id: &str,
        user: DbUser,
        password: Option<String>,
    ) -> Result<()> {
        if !self.databases.iter().any(|d| d.id == db_id) {
            anyhow::bail!("unknown database");
        }
        if let Some(pw) = password {
            self.store_secret(&user.id, pw);
        }
        let db = self
            .databases
            .iter_mut()
            .find(|d| d.id == db_id)
            .context("unknown database")?;
        match db.users.iter_mut().find(|u| u.id == user.id) {
            Some(existing) => *existing = user,
            None => db.users.push(user),
        }
        self.save()
    }

    /// Removes a database with its users and their secrets (the SSH one too, and
    /// copies left under the old name). Unknown ids are fine.
    pub fn remove_database(&mut self, id: &str) -> Result<()> {
        let Some(at) = self.databases.iter().position(|d| d.id == id) else {
            return Ok(());
        };
        let db = self.databases.remove(at);
        self.save()?;
        let accounts = db.users.iter().map(|u| u.id.clone());
        for account in accounts.chain([SshConfig::secret_account(id)]) {
            self.forget_secret(&account);
            if !self.in_memory
                && let Ok(entry) = keyring::Entry::new(LEGACY_NAME, &account)
            {
                let _ = entry.delete_credential();
            }
        }
        Ok(())
    }

    /// The config directory (where `profiles.toml` lives).
    pub fn dir(&self) -> Option<&std::path::Path> {
        self.path.as_deref().and_then(|p| p.parent())
    }

    /// Stores the SSH password or key passphrase of a database.
    pub fn set_ssh_secret(&mut self, db_id: &str, secret: String) {
        self.store_secret(&SshConfig::secret_account(db_id), secret);
    }

    /// The tunnel settings of a database with its secret, if it uses SSH.
    pub fn ssh_target(&mut self, db: &Database) -> Option<SshTarget> {
        let config = db.ssh.clone()?;
        let secret = match config.auth {
            SshAuth::Agent => None,
            SshAuth::Key | SshAuth::Password => self.password(&SshConfig::secret_account(&db.id)),
        };
        Some(SshTarget { config, secret })
    }

    /// Stores a secret of the app's own (e.g. the MCP server's token) by account.
    pub fn set_secret(&mut self, account: &str, secret: String) {
        self.store_secret(account, secret);
    }

    /// Removes a secret of the app's own (e.g. an AI provider's API key).
    pub fn forget_secret(&mut self, account: &str) {
        self.passwords.remove(account);
        if !self.in_memory
            && let Err(e) = keychain(account).and_then(|k| Ok(k.delete_credential()?))
        {
            eprintln!("keychain delete failed: {e:#}");
        }
    }

    /// A secret by Keychain account (user id, or [`SshConfig::secret_account`]).
    pub fn password(&mut self, account: &str) -> Option<String> {
        if let Some(pw) = self.passwords.get(account) {
            return Some(pw.clone());
        }
        if self.in_memory {
            return None;
        }
        let pw = match keychain(account).ok()?.get_password() {
            Ok(pw) => pw,
            // Saved under the old name: copy it over (the old entry stays).
            Err(_) => {
                let pw = keyring::Entry::new(LEGACY_NAME, account)
                    .ok()?
                    .get_password()
                    .ok()?;
                self.store_secret(account, pw.clone());
                pw
            }
        };
        self.passwords.insert(account.to_string(), pw.clone());
        Some(pw)
    }
}

fn keychain(account: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(KEYCHAIN_SERVICE, account)?)
}

impl Profiles {
    fn store_secret(&mut self, account: &str, secret: String) {
        if !self.in_memory
            && let Err(e) = keychain(account).and_then(|k| Ok(k.set_password(&secret)?))
        {
            eprintln!("keychain write failed: {e:#}");
        }
        self.passwords.insert(account.to_string(), secret);
    }
}

/// Unique enough id for a new database or user without pulling in a uuid crate.
pub fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("p{nanos:x}")
}

/// A random password of 24 letters and digits (about 143 bits), safe in any SQL
/// string literal or connection string.
pub fn new_password() -> String {
    const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).expect("system randomness");
    // 256 is not a multiple of the alphabet's size; the bias is negligible here.
    bytes
        .iter()
        .map(|b| CHARS[usize::from(*b) % CHARS.len()] as char)
        .collect()
}

/// Six random lowercase letters: a board's revision.
pub fn new_revision() -> String {
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes).expect("system randomness");
    bytes.iter().map(|b| char::from(b'a' + b % 26)).collect()
}

#[cfg(test)]
mod tests {

    #[test]
    fn secrets_of_the_app_are_kept_and_forgotten() {
        let mut p = Profiles::in_memory(Vec::new());
        p.set_secret("ai-key:anthropic", "sk".into());
        assert_eq!(p.password("ai-key:anthropic").as_deref(), Some("sk"));
        p.forget_secret("ai-key:anthropic");
        assert_eq!(p.password("ai-key:anthropic"), None);
    }

    #[test]
    fn passwords_are_new_each_time() {
        let (a, b) = (new_password(), new_password());
        assert_eq!(a.len(), 24);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn what_the_ai_may_read_loads_old_and_new_files() {
        use tangul_core::{Database, ValueAccess};
        let db = |extra: &str| -> Database {
            toml::from_str(&format!(
                "id = \"d\"\nname = \"n\"\ndriver = \"postgres\"\nhost = \"h\"\nport = 5432\n{extra}"
            ))
            .unwrap()
        };
        assert_eq!(db("").ai_values, ValueAccess::None);
        // Saved before there were levels.
        assert_eq!(db("ai_values = true").ai_values, ValueAccess::Values);
        assert_eq!(db("ai_values = false").ai_values, ValueAccess::None);
        assert_eq!(
            db("ai_values = \"statistics\"").ai_values,
            ValueAccess::Statistics
        );
        assert_eq!(db("ai_values = \"values\"").ai_values, ValueAccess::Values);
        // Only written when something is allowed.
        let mut d = db("");
        assert!(!toml::to_string(&d).unwrap().contains("ai_values"));
        d.ai_values = ValueAccess::Statistics;
        assert!(
            toml::to_string(&d)
                .unwrap()
                .contains("ai_values = \"statistics\"")
        );
    }

    #[test]
    fn schema_only_is_saved_only_when_set() {
        use tangul_core::{AiAccess, Database};
        let mut d: Database = toml::from_str(
            "id = \"d\"\nname = \"n\"\ndriver = \"postgres\"\nhost = \"h\"\nport = 5432\n",
        )
        .unwrap();
        assert_eq!(d.ai_access, AiAccess::Data);
        assert!(!toml::to_string(&d).unwrap().contains("ai_access"));
        d.ai_access = AiAccess::Schema;
        let saved = toml::to_string(&d).unwrap();
        assert!(saved.contains("ai_access = \"schema\""));
        assert_eq!(toml::from_str::<Database>(&saved).unwrap(), d);
        // The stricter of a database's level and a caller's ceiling.
        assert_eq!(AiAccess::Data.min(AiAccess::Schema), AiAccess::Schema);
        assert_eq!(AiAccess::Schema.min(AiAccess::Data), AiAccess::Schema);
        assert_eq!(AiAccess::Data.min(AiAccess::Data), AiAccess::Data);
    }

    #[test]
    fn ai_tools_are_on_unless_turned_off() {
        use tangul_core::Database;
        let db = |extra: &str| -> Database {
            toml::from_str(&format!(
                "id = \"d\"\nname = \"n\"\ndriver = \"postgres\"\nhost = \"h\"\nport = 5432\n{extra}"
            ))
            .unwrap()
        };
        // Saved before the setting existed: on.
        assert!(db("").ai_tools);
        assert!(!db("ai_tools = false").ai_tools);
        // Only written when off.
        let mut d = db("");
        assert!(!toml::to_string(&d).unwrap().contains("ai_tools"));
        d.ai_tools = false;
        assert!(toml::to_string(&d).unwrap().contains("ai_tools = false"));
    }
    use super::*;

    #[test]
    fn migrates_legacy_profiles_by_target() {
        let file: ProfilesFile = toml::from_str(
            r#"
            [[profiles]]
            id = "a"
            name = "prod"
            driver = "postgres"
            host = "db"
            port = 5432
            user = "admin"
            database = "app"

            [[profiles]]
            id = "b"
            name = ""
            driver = "postgres"
            host = "db"
            port = 5432
            user = "readonly"
            database = "app"

            [[profiles]]
            id = "c"
            name = "other"
            driver = "mysql"
            host = "db"
            port = 3306
            user = "root"
            "#,
        )
        .unwrap();
        let mut dbs = file.databases;
        migrate(file.profiles, &mut dbs);
        assert_eq!(dbs.len(), 2);
        assert_eq!(dbs[0].name, "prod");
        let users: Vec<_> = dbs[0].users.iter().map(|u| (&*u.id, &*u.user)).collect();
        assert_eq!(users, [("a", "admin"), ("b", "readonly")]);
        assert_eq!(dbs[1].users[0].id, "c");
    }
}
