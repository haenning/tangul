//! Installing a downloaded release over the running app (macOS): only an app
//! signed like the running one, with the version expected, ever replaces it.
//!
//! 1. [`installable`]: the running app is a bundle that may be replaced (not run
//!    from a translocated copy, its folder writable) and has a stable signature;
//!    its designated requirement (bundle id and certificate) is what the new app
//!    must meet. Ad-hoc signed builds have none: they only get the notice.
//! 2. [`install`]: mounts the DMG read-only, checks the app in it against that
//!    requirement (`codesign --verify --deep --strict -R`) and its version, copies
//!    it next to the running one and swaps the two by renaming. Any failure leaves
//!    the running app as it was.
//! 3. The replaced app stays as `.<name>.old` beside it until the new one starts
//!    and calls [`remove_old`].

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Version;

/// Why the running app can't update itself; it shows the notice with a link to
/// the release instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocked {
    /// Not running from an app bundle (`cargo run`).
    NotAnApp,
    /// Started from the DMG or the Downloads folder: macOS runs a copy elsewhere.
    Translocated,
    /// The folder the app is in can't be written to.
    ReadOnly(PathBuf),
    /// No stable signature to check the update against (a local build).
    AdHoc,
}

impl fmt::Display for Blocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Blocked::NotAnApp => write!(f, "tangul isn't running from an app"),
            Blocked::Translocated => write!(
                f,
                "tangul runs from where it was downloaded; move it to Applications first"
            ),
            Blocked::ReadOnly(dir) => write!(f, "{} can't be written to", dir.display()),
            Blocked::AdHoc => write!(f, "this build isn't signed for updates"),
        }
    }
}

/// What a new app's signature has to meet: the running app's designated
/// requirement, e.g. `identifier "io.github.haenning.tangul" and certificate leaf
/// = H"…"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement(String);

/// The running app bundle.
pub fn running_app() -> Option<PathBuf> {
    app_of(&std::env::current_exe().ok()?)
}

/// `…/tangul.app` for `…/tangul.app/Contents/MacOS/tangul`.
fn app_of(exe: &Path) -> Option<PathBuf> {
    let app = exe.parent()?.parent()?.parent()?;
    let bundle = app.extension().is_some_and(|e| e == "app")
        && exe.parent()?.file_name()? == "MacOS"
        && exe.parent()?.parent()?.file_name()? == "Contents";
    bundle.then(|| app.to_path_buf())
}

/// Whether `app` may update itself, and the requirement an update has to meet.
pub fn installable(app: &Path) -> Result<Requirement, Blocked> {
    if app
        .components()
        .any(|c| c.as_os_str() == "AppTranslocation")
    {
        return Err(Blocked::Translocated);
    }
    let dir = app.parent().ok_or(Blocked::NotAnApp)?;
    if !writable(dir) {
        return Err(Blocked::ReadOnly(dir.to_path_buf()));
    }
    let shown = Command::new("codesign")
        .args(["--display", "--requirements", "-"])
        .arg(app)
        .output()
        .map_err(|_| Blocked::AdHoc)?;
    let text = String::from_utf8_lossy(&shown.stdout) + String::from_utf8_lossy(&shown.stderr);
    designated(&text).ok_or(Blocked::AdHoc)
}

/// The designated requirement in `codesign --display --requirements -`'s output;
/// `None` for an ad-hoc signature (its requirement is only the code's hash, which
/// no other build meets).
fn designated(text: &str) -> Option<Requirement> {
    let requirement = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("designated => "))?
        .trim();
    (!requirement.is_empty() && !requirement.starts_with("cdhash"))
        .then(|| Requirement(requirement.to_string()))
}

/// Whether a file can be created in `dir` (and removed again).
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".tangul-update-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Installs the app in `dmg` over `app`: only when it meets `requirement` and is
/// `version`. `app` is untouched when anything fails.
pub fn install(
    dmg: &Path,
    app: &Path,
    requirement: &Requirement,
    version: Version,
) -> Result<(), String> {
    let name = app.file_name().ok_or("the app has no name")?;
    let mount = Mount::attach(dmg)?;
    let new = mount.dir.join(name);
    verify(&new, requirement)?;
    let found = bundle_version(&new)?;
    if found != version {
        return Err(format!(
            "The update is version {found}, not {version}: not installed"
        ));
    }
    let staged = sibling(app, "new")?;
    remove(&staged)?;
    run(
        Command::new("ditto").arg(&new).arg(&staged),
        "copy the update",
    )?;
    drop(mount);
    replace(app, &staged)
}

/// The new app must meet the running one's requirement: same bundle id, same
/// certificate, sealed resources intact.
fn verify(new: &Path, requirement: &Requirement) -> Result<(), String> {
    run(
        Command::new("codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(format!("-R={}", requirement.0))
            .arg(new),
        "verify the update's signature",
    )
    .map_err(|e| format!("{e}: not installed"))
}

fn bundle_version(app: &Path) -> Result<Version, String> {
    let plist = app.join("Contents/Info.plist");
    let out = Command::new("plutil")
        .args(["-extract", "CFBundleShortVersionString", "raw", "-o", "-"])
        .arg(&plist)
        .output()
        .map_err(|e| format!("Could not read the update's version: {e}"))?;
    Version::parse(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| "The update has no readable version".to_string())
}

/// `.<name>.<suffix>` next to `app`.
fn sibling(app: &Path, suffix: &str) -> Result<PathBuf, String> {
    let dir = app.parent().ok_or("the app has no folder")?;
    let name = app.file_name().ok_or("the app has no name")?;
    Ok(dir.join(format!(".{}.{suffix}", name.to_string_lossy())))
}

fn remove(path: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("Could not remove {}: {e}", path.display()))
        }
        _ => Ok(()),
    }
}

/// Puts `staged` where `app` is, keeping `app` as `.<name>.old`; puts `app` back
/// when the second rename fails.
fn replace(app: &Path, staged: &Path) -> Result<(), String> {
    let old = sibling(app, "old")?;
    remove(&old)?;
    std::fs::rename(app, &old).map_err(|e| format!("Could not move the running app aside: {e}"))?;
    if let Err(e) = std::fs::rename(staged, app) {
        let _ = std::fs::rename(&old, app);
        let _ = remove(staged);
        return Err(format!("Could not put the update in place: {e}"));
    }
    Ok(())
}

/// Removes what the last update left beside `app` (the replaced app, an
/// unfinished copy). Called once the new version runs.
pub fn remove_old(app: &Path) {
    for suffix in ["old", "new"] {
        if let Ok(path) = sibling(app, suffix) {
            let _ = remove(&path);
        }
    }
}

/// Starts `app` as a new instance; the running one quits afterwards.
pub fn relaunch(app: &Path) -> Result<(), String> {
    Command::new("open")
        .arg("-n")
        .arg(app)
        .spawn()
        .map(drop)
        .map_err(|e| format!("Could not start the new version: {e}"))
}

fn run(command: &mut Command, what: &str) -> Result<(), String> {
    let out = command
        .output()
        .map_err(|e| format!("Could not {what}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let reason = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(format!("Could not {what}: {reason}"))
}

/// A DMG mounted read-only in its own folder, detached when dropped.
struct Mount {
    dir: PathBuf,
}

impl Mount {
    /// `diskutil image` (macOS 26 on, where `hdiutil attach` is deprecated), else
    /// `hdiutil`.
    fn attach(dmg: &Path) -> Result<Self, String> {
        let dir = dmg.with_extension("mount");
        std::fs::create_dir_all(&dir).map_err(|e| format!("Could not mount the update: {e}"))?;
        let diskutil = run(
            Command::new("diskutil")
                .args([
                    "image",
                    "attach",
                    "--mountOptions",
                    "nobrowse",
                    "--readOnly",
                ])
                .arg("--mountPoint")
                .arg(&dir)
                .arg(dmg),
            "mount the update",
        );
        if diskutil.is_err() {
            run(
                Command::new("hdiutil")
                    .args([
                        "attach",
                        "-nobrowse",
                        "-readonly",
                        "-noautoopen",
                        "-mountpoint",
                    ])
                    .arg(&dir)
                    .arg(dmg),
                "mount the update",
            )?;
        }
        Ok(Mount { dir })
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        let ejected = Command::new("diskutil")
            .arg("eject")
            .arg(&self.dir)
            .output();
        if !ejected.is_ok_and(|out| out.status.success()) {
            let _ = Command::new("hdiutil")
                .args(["detach", "-quiet", "-force"])
                .arg(&self.dir)
                .status();
        }
        let _ = std::fs::remove_dir(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tangul-update-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn app(dir: &Path, version: &str) -> PathBuf {
        let app = dir.join("tangul.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/version"), version).unwrap();
        app
    }

    fn version_of(app: &Path) -> String {
        std::fs::read_to_string(app.join("Contents/version")).unwrap()
    }

    #[test]
    fn finds_the_bundle_of_the_executable() {
        assert_eq!(
            app_of(Path::new("/Applications/tangul.app/Contents/MacOS/tangul")),
            Some(PathBuf::from("/Applications/tangul.app"))
        );
        assert_eq!(app_of(Path::new("/repo/target/debug/tangul")), None);
        assert_eq!(
            app_of(Path::new("/x/tangul.app/Contents/Resources/tangul")),
            None
        );
        assert_eq!(app_of(Path::new("tangul")), None);
    }

    #[test]
    fn the_designated_requirement_or_none_for_ad_hoc() {
        let signed = "Executable=/Applications/tangul.app/Contents/MacOS/tangul\n\
                      designated => identifier \"io.github.haenning.tangul\" and certificate leaf = H\"ab12\"\n";
        assert_eq!(
            designated(signed),
            Some(Requirement(
                "identifier \"io.github.haenning.tangul\" and certificate leaf = H\"ab12\"".into()
            ))
        );
        let ad_hoc = "Executable=/x\n# designated => cdhash H\"0f1e\"\n";
        assert_eq!(designated(ad_hoc), None);
        assert_eq!(designated("code object is not signed at all"), None);
        assert_eq!(designated("designated => "), None);
    }

    #[test]
    fn translocated_or_read_only_apps_stay_as_they_are() {
        assert_eq!(
            installable(Path::new(
                "/private/var/folders/x/AppTranslocation/1234/d/tangul.app"
            )),
            Err(Blocked::Translocated)
        );
        let missing = Path::new("/no/such/folder/tangul.app");
        assert_eq!(
            installable(missing),
            Err(Blocked::ReadOnly("/no/such/folder".into()))
        );
        assert_eq!(
            Blocked::Translocated.to_string(),
            "tangul runs from where it was downloaded; move it to Applications first"
        );
    }

    #[test]
    fn replaces_the_app_and_keeps_the_old_one_until_removed() {
        let dir = temp("replace");
        let running = app(&dir, "0.2.3");
        let staged = dir.join(".tangul.app.new");
        app(&dir.join("download"), "0.2.4");
        std::fs::rename(dir.join("download/tangul.app"), &staged).unwrap();

        replace(&running, &staged).unwrap();
        assert_eq!(version_of(&running), "0.2.4");
        assert_eq!(version_of(&dir.join(".tangul.app.old")), "0.2.3");
        assert!(!staged.exists());

        remove_old(&running);
        assert!(!dir.join(".tangul.app.old").exists());
        assert_eq!(version_of(&running), "0.2.4");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A signed app bundle of `version` with bundle id `id`, in a DMG of its own.
    #[cfg(target_os = "macos")]
    fn signed_dmg(dir: &Path, id: &str, version: &str) -> PathBuf {
        let staging = dir.join(format!("dmg-{version}"));
        let app = staging.join("tangul.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::copy("/usr/bin/true", app.join("Contents/MacOS/tangul")).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
                 <key>CFBundleIdentifier</key><string>{id}</string>\
                 <key>CFBundleExecutable</key><string>tangul</string>\
                 <key>CFBundleShortVersionString</key><string>{version}</string>\
                 </dict></plist>\n"
            ),
        )
        .unwrap();
        // Ad hoc, but with the bundle id: enough for an `identifier` requirement.
        run(
            Command::new("codesign")
                .args(["--force", "--sign", "-", "--identifier", id])
                .arg(&app),
            "sign",
        )
        .unwrap();
        let dmg = dir.join(format!("tangul-{version}.dmg"));
        run(
            Command::new("hdiutil")
                .args(["create", "-quiet", "-format", "UDZO", "-srcfolder"])
                .arg(&staging)
                .arg(&dmg),
            "create the dmg",
        )
        .unwrap();
        dmg
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn installs_only_an_app_signed_as_required_and_of_the_version() {
        let dir = temp("install");
        let running = app(&dir.join("Applications"), "0.2.3");
        let dmg = signed_dmg(&dir, "io.test.tangul", "0.2.4");
        let ours = Requirement("identifier \"io.test.tangul\"".into());
        let v = |text| Version::parse(text).unwrap();

        // Another signature or another version than expected: nothing changes.
        let other = Requirement("identifier \"io.test.someone-else\"".into());
        let err = install(&dmg, &running, &other, v("0.2.4")).unwrap_err();
        assert!(err.contains("verify the update's signature"), "{err}");
        let err = install(&dmg, &running, &ours, v("0.2.5")).unwrap_err();
        assert!(err.contains("version 0.2.4, not 0.2.5"), "{err}");
        assert_eq!(version_of(&running), "0.2.3");

        install(&dmg, &running, &ours, v("0.2.4")).unwrap();
        assert_eq!(bundle_version(&running).unwrap(), v("0.2.4"));
        verify(&running, &ours).unwrap();
        // The replaced app is kept until the new one runs; the DMG is detached.
        assert_eq!(
            version_of(&dir.join("Applications/.tangul.app.old")),
            "0.2.3"
        );
        assert!(!dmg.with_extension("mount").exists());
        remove_old(&running);
        assert!(!dir.join("Applications/.tangul.app.old").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_swap_puts_the_running_app_back() {
        let dir = temp("rollback");
        let running = app(&dir, "0.2.3");
        // Nothing staged: the second rename fails.
        let err = replace(&running, &dir.join(".tangul.app.new")).unwrap_err();
        assert!(
            err.starts_with("Could not put the update in place"),
            "{err}"
        );
        assert_eq!(version_of(&running), "0.2.3");
        assert!(!dir.join(".tangul.app.old").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
