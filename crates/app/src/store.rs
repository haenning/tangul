//! Saved databases (each with its users) as TOML in the config dir
//! (`~/Library/Application Support/tangul/profiles.toml` on macOS, overridable with
//! `TANGUL_CONFIG_DIR`, used by `make run-dev`).
//! Passwords go to the Keychain (one entry per user), never into the file.

use std::{collections::HashMap, path::PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use tangul_core::{Database, DbUser, DriverKind, SshAuth, SshConfig, SshTarget, TlsMode};

const KEYCHAIN_SERVICE: &str = "tangul";
/// The app's name before it became tangul. Its config directory and Keychain
/// entries are taken over the first time they are needed.
const LEGACY_NAME: &str = "rusdb";

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
            }),
        }
    }
}

/// Where `profiles.toml` and `settings.json` live; `TANGUL_CONFIG_DIR` overrides it.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TANGUL_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    let base = dirs::config_dir()?;
    let dir = base.join("tangul");
    adopt_legacy_dir(&base.join(LEGACY_NAME), &dir);
    Some(dir)
}

/// Moves the config directory of the old name to `dir`, if there is one and `dir`
/// doesn't exist yet.
fn adopt_legacy_dir(legacy: &std::path::Path, dir: &std::path::Path) {
    if dir.exists() || !legacy.is_dir() {
        return;
    }
    match std::fs::rename(legacy, dir) {
        Ok(()) => eprintln!("moved {} to {}", legacy.display(), dir.display()),
        Err(e) => eprintln!("could not move {}: {e}", legacy.display()),
    }
}

pub struct Store {
    path: Option<PathBuf>,
    pub databases: Vec<Database>,
    /// Session cache so the Keychain is hit once per profile, and so passwords work
    /// on platforms where only the mock keyring store is available.
    passwords: HashMap<String, String>,
}

impl Store {
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
        };
        if migrated && let Err(e) = store.save() {
            eprintln!("could not write migrated profiles file: {e:#}");
        }
        store
    }

    pub fn database(&self, id: &str) -> Option<&Database> {
        self.databases.iter().find(|d| d.id == id)
    }

    pub fn save(&self) -> Result<()> {
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
        let db = self
            .databases
            .iter_mut()
            .find(|d| d.id == db_id)
            .context("unknown database")?;
        if let Some(pw) = password {
            set_keychain(&mut self.passwords, &user.id, pw);
        }
        match db.users.iter_mut().find(|u| u.id == user.id) {
            Some(existing) => *existing = user,
            None => db.users.push(user),
        }
        self.save()
    }

    /// The config directory (where `profiles.toml` lives).
    pub fn dir(&self) -> Option<&std::path::Path> {
        self.path.as_deref().and_then(|p| p.parent())
    }

    /// Stores the SSH password or key passphrase of a database.
    pub fn set_ssh_secret(&mut self, db_id: &str, secret: String) {
        set_keychain(
            &mut self.passwords,
            &SshConfig::secret_account(db_id),
            secret,
        );
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

    /// A secret by Keychain account (user id, or [`SshConfig::secret_account`]).
    pub fn password(&mut self, account: &str) -> Option<String> {
        if let Some(pw) = self.passwords.get(account) {
            return Some(pw.clone());
        }
        let pw = match keychain(account).ok()?.get_password() {
            Ok(pw) => pw,
            // Saved under the old name: copy it over (the old entry stays).
            Err(_) => {
                let pw = keyring::Entry::new(LEGACY_NAME, account)
                    .ok()?
                    .get_password()
                    .ok()?;
                set_keychain(&mut self.passwords, account, pw.clone());
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

fn set_keychain(cache: &mut HashMap<String, String>, account: &str, secret: String) {
    if let Err(e) = keychain(account).and_then(|k| Ok(k.set_password(&secret)?)) {
        eprintln!("keychain write failed: {e:#}");
    }
    cache.insert(account.to_string(), secret);
}

/// Unique enough id for a new database or user without pulling in a uuid crate.
pub fn new_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("p{nanos:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_over_the_legacy_config_dir() {
        let base = std::env::temp_dir().join(format!("tangul-dirs-{}", std::process::id()));
        let (legacy, dir) = (base.join("rusdb"), base.join("tangul"));
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("profiles.toml"), "x").unwrap();
        adopt_legacy_dir(&legacy, &dir);
        assert!(!legacy.exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("profiles.toml")).unwrap(),
            "x"
        );
        // An existing new directory is never replaced.
        std::fs::create_dir_all(&legacy).unwrap();
        adopt_legacy_dir(&legacy, &dir);
        assert!(legacy.exists());
        std::fs::remove_dir_all(&base).unwrap();
    }

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
