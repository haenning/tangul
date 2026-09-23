//! Connection profiles as TOML in the config dir
//! (`~/Library/Application Support/rusdb/profiles.toml` on macOS).
//! Passwords go to the Keychain, never into the file.

use std::{collections::HashMap, path::PathBuf};

use anyhow::{Context as _, Result};
use rusdb_core::Profile;
use serde::{Deserialize, Serialize};

const KEYCHAIN_SERVICE: &str = "rusdb";

#[derive(Default, Serialize, Deserialize)]
struct ProfilesFile {
    #[serde(default)]
    profiles: Vec<Profile>,
}

pub struct Store {
    path: Option<PathBuf>,
    pub profiles: Vec<Profile>,
    /// Session cache so the Keychain is hit once per profile, and so passwords work
    /// on platforms where only the mock keyring store is available.
    passwords: HashMap<String, String>,
}

impl Store {
    pub fn load() -> Self {
        let path = dirs::config_dir().map(|d| d.join("rusdb").join("profiles.toml"));
        let profiles = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| match toml::from_str::<ProfilesFile>(&s) {
                Ok(f) => Some(f.profiles),
                Err(e) => {
                    eprintln!("ignoring invalid profiles file: {e}");
                    None
                }
            })
            .unwrap_or_default();
        Self {
            path,
            profiles,
            passwords: HashMap::new(),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = self.path.as_ref().context("no config directory")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = ProfilesFile {
            profiles: self.profiles.clone(),
        };
        std::fs::write(path, toml::to_string_pretty(&file)?)?;
        Ok(())
    }

    /// Inserts or replaces a profile (matched by id) and stores its password.
    pub fn upsert(&mut self, profile: Profile, password: Option<String>) -> Result<()> {
        if let Some(pw) = password {
            if let Err(e) = keychain(&profile.id).and_then(|k| Ok(k.set_password(&pw)?)) {
                eprintln!("keychain write failed: {e:#}");
            }
            self.passwords.insert(profile.id.clone(), pw);
        }
        match self.profiles.iter_mut().find(|p| p.id == profile.id) {
            Some(existing) => *existing = profile,
            None => self.profiles.push(profile),
        }
        self.save()
    }

    pub fn password(&mut self, profile_id: &str) -> Option<String> {
        if let Some(pw) = self.passwords.get(profile_id) {
            return Some(pw.clone());
        }
        let pw = keychain(profile_id).ok()?.get_password().ok()?;
        self.passwords.insert(profile_id.to_string(), pw.clone());
        Some(pw)
    }
}

fn keychain(profile_id: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(KEYCHAIN_SERVICE, profile_id)?)
}

/// Unique enough id for a new profile without pulling in a uuid crate.
pub fn new_profile_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("p{nanos:x}")
}
