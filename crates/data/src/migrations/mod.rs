//! The steps between released layouts of the settings files (see `CLAUDE.md` here):
//! step `i` turns layout `i + 1` into `i + 2`. None yet: both files are at their
//! first released layout.

use serde_json::Value;

/// One step: a file's JSON from one layout to the next.
pub type Step = fn(&mut Value);

/// `settings.json`.
pub const SETTINGS: &[Step] = &[];

/// `databases/<id>.json`.
pub const DATABASE: &[Step] = &[];

/// The layout the steps lead to.
pub const fn current(steps: &[Step]) -> u64 {
    steps.len() as u64 + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upgrade::upgrade;

    /// Every released layout, as its release wrote it, reaches the current one and
    /// loads there.
    #[test]
    fn every_released_layout_upgrades_and_loads() {
        let settings: &[&str] = &[include_str!("fixtures/settings/v1.json")];
        assert_eq!(
            settings.len() as u64,
            current(SETTINGS),
            "a fixture per version"
        );
        for (i, text) in settings.iter().enumerate() {
            let up = crate::settings::upgrade(text).unwrap_or_else(|e| panic!("v{}: {e}", i + 1));
            crate::settings::Settings::parse(&up).unwrap_or_else(|e| panic!("v{}: {e}", i + 1));
            assert!(up.contains(&format!("\"version\": {}", current(SETTINGS))));
        }

        let database: &[&str] = &[include_str!("fixtures/database/v1.json")];
        assert_eq!(
            database.len() as u64,
            current(DATABASE),
            "a fixture per version"
        );
        for (i, text) in database.iter().enumerate() {
            let up = crate::database_settings::upgrade(text)
                .unwrap_or_else(|e| panic!("v{}: {e}", i + 1));
            crate::database_settings::parse(&up).unwrap_or_else(|e| panic!("v{}: {e}", i + 1));
        }
        // An upgraded file is up to date.
        let once = upgrade("{}", &crate::database_settings::defaults(), DATABASE).unwrap();
        assert_eq!(
            upgrade(&once, &crate::database_settings::defaults(), DATABASE),
            Ok(once)
        );
    }
}
