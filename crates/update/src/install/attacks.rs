//! Updates that must never be installed, each in a real DMG, against a real
//! certificate-pinned requirement: two self-signed certificates in a keychain of
//! the test's own (deleted afterwards), like the releases' certificate.
//! Whatever is refused leaves the running app as it was, with nothing beside it.

use super::tests::{app, temp, version_of};
use super::*;

const ID: &str = "io.test.tangul";

/// A keychain file with code signing identities. On the user's search list while it
/// exists (codesign finds identities only there on some Macs, CI's among them), the
/// list put back as it was afterwards.
struct Keychain {
    path: PathBuf,
    password: String,
    /// The search list before.
    searched: Vec<String>,
}

/// The user's keychain search list.
fn search_list() -> Vec<String> {
    let out = Command::new("security")
        .args(["list-keychains", "-d", "user"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| line.trim().trim_matches('"').to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn set_search_list(keychains: &[String]) {
    let _ = Command::new("security")
        .args(["list-keychains", "-d", "user", "-s"])
        .args(keychains)
        .status();
}

impl Keychain {
    fn new(dir: &Path) -> Self {
        let keychain = Keychain {
            path: dir.join("test.keychain-db"),
            password: format!("tangul-test-{}", std::process::id()),
            searched: search_list(),
        };
        let security = |args: &[&str]| {
            let mut command = Command::new("security");
            command.args(args).arg(&keychain.path);
            run(&mut command, "set up the test keychain").unwrap();
        };
        security(&["create-keychain", "-p", &keychain.password]);
        security(&["unlock-keychain", "-p", &keychain.password]);
        let mut searched = vec![keychain.path.to_string_lossy().into_owned()];
        searched.extend(keychain.searched.iter().cloned());
        set_search_list(&searched);
        keychain
    }

    /// A self-signed code signing certificate `name`, as `release-signing-cert.sh`
    /// makes the releases' one.
    fn add_identity(&self, dir: &Path, name: &str) {
        let config = dir.join(format!("{name}.cnf"));
        std::fs::write(
            &config,
            format!(
                "[req]\ndistinguished_name = dn\nprompt = no\nx509_extensions = ext\n\
                 [dn]\nCN = {name}\n[ext]\nbasicConstraints = critical, CA:false\n\
                 keyUsage = critical, digitalSignature\n\
                 extendedKeyUsage = critical, codeSigning\n"
            ),
        )
        .unwrap();
        let (key, cert, p12) = (
            dir.join(format!("{name}.key")),
            dir.join(format!("{name}.pem")),
            dir.join(format!("{name}.p12")),
        );
        run(
            Command::new("/usr/bin/openssl")
                .args([
                    "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                ])
                .arg("-config")
                .arg(&config)
                .arg("-keyout")
                .arg(&key)
                .arg("-out")
                .arg(&cert),
            "make a certificate",
        )
        .unwrap();
        run(
            Command::new("/usr/bin/openssl")
                .args(["pkcs12", "-export", "-passout", "pass:test", "-name", name])
                .arg("-inkey")
                .arg(&key)
                .arg("-in")
                .arg(&cert)
                .arg("-out")
                .arg(&p12),
            "export the certificate",
        )
        .unwrap();
        run(
            Command::new("security")
                .arg("import")
                .arg(&p12)
                .arg("-k")
                .arg(&self.path)
                .args(["-P", "test", "-f", "pkcs12", "-T", "/usr/bin/codesign"]),
            "import the certificate",
        )
        .unwrap();
        run(
            Command::new("security")
                .args(["set-key-partition-list", "-S", "apple-tool:,apple:", "-s"])
                .args(["-k", &self.password])
                .arg(&self.path),
            "allow codesign",
        )
        .unwrap();
    }

    /// Signs `path` (an app or a DMG) as `id` with the identity `name` from this
    /// keychain.
    fn sign(&self, path: &Path, name: &str, id: &str) {
        run(
            Command::new("codesign")
                .args(["--force", "--timestamp=none", "--identifier", id])
                .arg("--keychain")
                .arg(&self.path)
                .args(["--sign", name])
                .arg(path),
            "sign",
        )
        .unwrap();
    }
}

impl Drop for Keychain {
    fn drop(&mut self) {
        set_search_list(&self.searched);
        let _ = Command::new("security")
            .arg("delete-keychain")
            .arg(&self.path)
            .status();
    }
}

/// An unsigned app bundle `tangul.app` of `version` in `staging`.
fn bundle(staging: &Path, id: &str, version: &str) -> PathBuf {
    let app = staging.join("tangul.app");
    std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
    std::fs::copy("/usr/bin/true", app.join("Contents/MacOS/tangul")).unwrap();
    std::fs::write(app.join("Contents/Resources/data.txt"), "sealed").unwrap();
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
    app
}

/// `staging` as a DMG.
fn dmg(dir: &Path, staging: &Path) -> PathBuf {
    let dmg = dir.join(format!(
        "{}.dmg",
        staging.file_name().unwrap().to_string_lossy()
    ));
    run(
        Command::new("hdiutil")
            .args(["create", "-quiet", "-format", "UDZO", "-srcfolder"])
            .arg(staging)
            .arg(&dmg),
        "create the dmg",
    )
    .unwrap();
    dmg
}

#[test]
fn only_an_app_signed_with_the_same_certificate_as_the_same_app_is_installed() {
    let dir = temp("attacks");
    let keychain = Keychain::new(&dir);
    keychain.add_identity(&dir, "tangul test A");
    keychain.add_identity(&dir, "tangul test B");
    let v = |text| Version::parse(text).unwrap();
    let running = app(&dir.join("Applications"), "0.2.3");

    // What a release signed with certificate A requires of its updates, read from
    // a real signature as `installable` does: pinned to A's certificate.
    let reference = bundle(&dir.join("reference"), ID, "0.2.3");
    keychain.sign(&reference, "tangul test A", ID);
    let shown = Command::new("codesign")
        .args(["--display", "--requirements", "-"])
        .arg(&reference)
        .output()
        .unwrap();
    let requirement = designated(
        &(String::from_utf8_lossy(&shown.stdout) + String::from_utf8_lossy(&shown.stderr)),
    )
    .expect("a certificate-pinned requirement");
    assert!(
        requirement.0.contains(ID) && requirement.0.contains("certificate leaf"),
        "{requirement:?}"
    );

    // Each attack as the DMG it would arrive in, the DMG signed like a release's
    // (certificate A, the bundle id with `.dmg`) unless the DMG is what's attacked.
    let dmg_id = format!("{ID}.dmg");
    let image = |name: &str, make: &dyn Fn(&Path), signer: Option<(&str, &str)>| -> PathBuf {
        let staging = dir.join(name);
        make(&staging);
        let dmg = dmg(&dir, &staging);
        if let Some((identity, id)) = signer {
            keychain.sign(&dmg, identity, id);
        }
        dmg
    };
    let attack = |name: &str, make: &dyn Fn(&Path)| -> PathBuf {
        image(name, make, Some(("tangul test A", &dmg_id)))
    };
    let signed_a = |staging: &Path| {
        let app = bundle(staging, ID, "0.2.4");
        keychain.sign(&app, "tangul test A", ID);
        app
    };
    let good_app = |s: &Path| {
        signed_a(s);
    };
    const DOWNLOAD: &str = "Could not verify the download's signature";
    const APP: &str = "Could not verify the update's signature";
    let changed_after_signing = {
        let dmg = attack("changed-dmg", &good_app);
        let mut bytes = std::fs::read(&dmg).unwrap();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0xff;
        std::fs::write(&dmg, bytes).unwrap();
        dmg
    };
    let refused = [
        // The DMG itself: checked before anything reads it.
        (
            "an unsigned DMG",
            image("dmg-unsigned", &good_app, None),
            v("0.2.4"),
            DOWNLOAD,
        ),
        (
            "a DMG signed with another certificate",
            image(
                "dmg-other-cert",
                &good_app,
                Some(("tangul test B", &dmg_id)),
            ),
            v("0.2.4"),
            DOWNLOAD,
        ),
        (
            "a DMG signed as something else",
            image("dmg-other-id", &good_app, Some(("tangul test A", ID))),
            v("0.2.4"),
            DOWNLOAD,
        ),
        (
            "a DMG changed after signing",
            changed_after_signing,
            v("0.2.4"),
            DOWNLOAD,
        ),
        // The app in a DMG signed like a release's.
        (
            "unsigned",
            attack("unsigned", &|s| {
                bundle(s, ID, "0.2.4");
            }),
            v("0.2.4"),
            APP,
        ),
        (
            "ad-hoc signed as the app",
            attack("adhoc", &|s| {
                let app = bundle(s, ID, "0.2.4");
                run(
                    Command::new("codesign")
                        .args(["--force", "--sign", "-", "--identifier", ID])
                        .arg(&app),
                    "sign",
                )
                .unwrap();
            }),
            v("0.2.4"),
            APP,
        ),
        (
            "another certificate, the same bundle id",
            attack("other-cert", &|s| {
                let app = bundle(s, ID, "0.2.4");
                keychain.sign(&app, "tangul test B", ID);
            }),
            v("0.2.4"),
            APP,
        ),
        (
            "the certificate, another bundle id",
            attack("other-id", &|s| {
                let app = bundle(s, "io.test.other", "0.2.4");
                keychain.sign(&app, "tangul test A", "io.test.other");
            }),
            v("0.2.4"),
            APP,
        ),
        (
            "a resource changed after signing",
            attack("changed-resource", &|s| {
                let app = signed_a(s);
                std::fs::write(app.join("Contents/Resources/data.txt"), "evil").unwrap();
            }),
            v("0.2.4"),
            APP,
        ),
        (
            "a file added after signing",
            attack("added-file", &|s| {
                let app = signed_a(s);
                std::fs::copy("/bin/sh", app.join("Contents/MacOS/helper")).unwrap();
            }),
            v("0.2.4"),
            APP,
        ),
        (
            "the executable changed after signing",
            attack("changed-executable", &|s| {
                let app = signed_a(s);
                std::fs::copy("/bin/sh", app.join("Contents/MacOS/tangul")).unwrap();
            }),
            v("0.2.4"),
            APP,
        ),
        (
            "a link to a valid app elsewhere",
            attack("symlink", &|s| {
                let target = signed_a(&dir.join("elsewhere"));
                std::fs::create_dir_all(s).unwrap();
                std::os::unix::fs::symlink(&target, s.join("tangul.app")).unwrap();
            }),
            v("0.2.4"),
            "The update holds no app",
        ),
        (
            "a valid app of another version than announced",
            attack("other-version", &|s| {
                signed_a(s);
            }),
            v("9.0.0"),
            "The update is version 0.2.4, not 9.0.0",
        ),
    ];
    for (what, dmg, version, reason) in &refused {
        let err = install(dmg, &running, &requirement, *version)
            .err()
            .unwrap_or_else(|| panic!("{what}: installed"));
        // Refused for the reason that matters, not by accident (a failed mount).
        assert!(err.starts_with(reason), "{what}: {err}");
        assert_eq!(version_of(&running), "0.2.3", "{what}");
        for left in [".tangul.app.new", ".tangul.app.old"] {
            assert!(
                !dir.join("Applications").join(left).exists(),
                "{what}: {left}"
            );
        }
        assert!(
            !dmg.with_extension("mount").exists(),
            "{what}: still mounted"
        );
    }

    // The real one: certificate A, the app's bundle id, the version announced.
    let good = attack("good", &|s| {
        signed_a(s);
    });
    install(&good, &running, &requirement, v("0.2.4")).unwrap();
    assert_eq!(bundle_version(&running).unwrap(), v("0.2.4"));
    verify(&running, &requirement).unwrap();
    assert_eq!(
        version_of(&dir.join("Applications/.tangul.app.old")),
        "0.2.3"
    );
    remove_old(&running);
    assert!(!dir.join("Applications/.tangul.app.old").exists());

    drop(keychain);
    std::fs::remove_dir_all(&dir).unwrap();
}
