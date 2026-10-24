//! tangul updating itself from its GitHub releases, without UI: whether a newer
//! release exists ([`check`]), its DMG ([`download`]), and replacing the running
//! app with it, only when the new app is signed like the running one
//! ([`install`]). The app asks first and shows what happens.

mod install;
mod release;
mod version;

pub use install::{Blocked, Requirement, install, installable, relaunch, remove_old, running_app};
pub use release::{Download, Downloaded, LATEST, Release, check, dmg_name, download, newer};
pub use version::Version;
