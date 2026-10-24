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

impl Requirement {
    /// What the release's DMG is signed like: the same certificate, under the app's
    /// bundle id with `.dmg` (as `scripts/bundle-macos.sh` signs it). `None` when the
    /// requirement doesn't start with the bundle id.
    fn for_dmg(&self) -> Option<Requirement> {
        let rest = self.0.strip_prefix("identifier \"")?;
        let (id, rest) = rest.split_once('"')?;
        Some(Requirement(format!("identifier \"{id}.dmg\"{rest}")))
    }
}

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

/// The designated requirement in `codesign --display --requirements -`'s output,
/// when it pins a certificate (`certificate leaf = H"…"`, `anchor apple generic …`);
/// `None` for an ad-hoc signature (its requirement is only the code's hash) and for
/// one that names only the bundle id, which anyone can sign an app with.
fn designated(text: &str) -> Option<Requirement> {
    let requirement = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("designated => "))?
        .trim();
    let pinned = requirement.contains("certificate ") || requirement.contains("anchor ");
    (pinned && !requirement.starts_with("cdhash")).then(|| Requirement(requirement.to_string()))
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
    // Nothing reads the image before its own signature is checked.
    let signed_like = requirement
        .for_dmg()
        .ok_or("The app's signature names no bundle id: not installed")?;
    run(
        Command::new("codesign")
            .args(["--verify", "--strict"])
            .arg(format!("-R={}", signed_like.0))
            .arg(dmg),
        "verify the download's signature",
    )
    .map_err(|e| format!("{e}: not installed"))?;
    let mount = Mount::attach(dmg)?;
    let new = mount.dir.join(name);
    // The app itself, not a link to one somewhere else.
    let real = std::fs::symlink_metadata(&new).is_ok_and(|m| m.is_dir() && !m.is_symlink());
    if !real {
        return Err("The update holds no app: not installed".into());
    }
    accept(&new, requirement, version)?;
    let staged = sibling(app, "new")?;
    remove(&staged)?;
    run(
        Command::new("ditto").arg(&new).arg(&staged),
        "copy the update",
    )?;
    drop(mount);
    // What gets installed is the copy: checked again.
    if let Err(e) = accept(&staged, requirement, version) {
        let _ = remove(&staged);
        return Err(e);
    }
    replace(app, &staged)
}

/// `new` meets the requirement and is `version`.
fn accept(new: &Path, requirement: &Requirement, version: Version) -> Result<(), String> {
    verify(new, requirement)?;
    let found = bundle_version(new)?;
    if found != version {
        return Err(format!(
            "The update is version {found}, not {version}: not installed"
        ));
    }
    Ok(())
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

/// Updates that must never be installed (macOS: real signatures, real DMGs).
#[cfg(all(test, target_os = "macos"))]
mod attacks;

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tangul-update-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(super) fn app(dir: &Path, version: &str) -> PathBuf {
        let app = dir.join("tangul.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::write(app.join("Contents/version"), version).unwrap();
        app
    }

    pub(super) fn version_of(app: &Path) -> String {
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
        // A bundle id alone pins nothing: anyone can sign an app with it.
        assert_eq!(
            designated("designated => identifier \"io.github.haenning.tangul\""),
            None
        );
        assert!(
            designated(
                "designated => anchor apple generic and certificate leaf[subject.OU] = \"TEAM\""
            )
            .is_some()
        );
        // The DMG: the same certificate, the bundle id with `.dmg`.
        assert_eq!(
            designated(signed).unwrap().for_dmg(),
            Some(Requirement(
                "identifier \"io.github.haenning.tangul.dmg\" and certificate leaf = H\"ab12\""
                    .into()
            ))
        );
        assert_eq!(
            Requirement("anchor apple generic and certificate leaf = H\"ab12\"".into()).for_dmg(),
            None
        );
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
