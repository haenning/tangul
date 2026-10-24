//! The latest release on GitHub: is it newer, and its DMG.

use std::path::{Path, PathBuf};

use futures::StreamExt as _;
use tokio::io::AsyncWriteExt as _;

use crate::Version;

/// The releases API's latest release (drafts and pre-releases never are).
pub const LATEST: &str = "https://api.github.com/repos/haenning/tangul/releases/latest";

/// A release newer than the running app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// The release notes (Markdown).
    pub notes: String,
    /// Its page on GitHub.
    pub page: String,
    /// The universal DMG.
    pub dmg: Download,
    /// The DMG's detached signature (`<dmg>.sig`, from v0.2.5 on); without one the
    /// DMG must be signed inside.
    pub signature: Option<Download>,
}

/// A file of a release: where it is downloaded from and its size in bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub url: String,
    pub size: u64,
}

/// The DMG a release ships, as `make app` and the release workflow name it.
pub fn dmg_name(version: Version) -> String {
    format!("tangul-{version}-universal.dmg")
}

/// The release in the API's answer when it is newer than `current`, published
/// (no draft, no pre-release) and has its DMG.
pub fn newer(release: &serde_json::Value, current: Version) -> Option<Release> {
    let flag = |key: &str| release[key].as_bool().unwrap_or(true);
    if flag("draft") || flag("prerelease") {
        return None;
    }
    let version = Version::parse(release["tag_name"].as_str()?)?;
    if version <= current {
        return None;
    }
    let assets = release["assets"].as_array()?;
    // Only this repository's download of exactly that file.
    let file = |name: &str| -> Option<Download> {
        let asset = assets.iter().find(|a| a["name"] == name)?;
        let url = asset["browser_download_url"].as_str()?;
        (url == format!("{DOWNLOADS}v{version}/{name}")).then(|| Download {
            url: url.to_string(),
            size: asset["size"].as_u64().unwrap_or_default(),
        })
    };
    let name = dmg_name(version);
    Some(Release {
        version,
        notes: release["body"].as_str().unwrap_or_default().to_string(),
        page: release["html_url"].as_str()?.to_string(),
        dmg: file(&name).filter(|d| d.size > 0)?,
        signature: file(&format!("{name}.sig")),
    })
}

/// The downloaded DMG and its detached signature, if the release has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    pub dmg: PathBuf,
    pub signature: Option<PathBuf>,
}

/// Where the releases' files are downloaded from.
const DOWNLOADS: &str = "https://github.com/haenning/tangul/releases/download/";

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        // GitHub's API refuses requests without one.
        .user_agent(concat!("tangul/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())
}

/// Asks GitHub for the latest release: one request, nothing sent but the request.
/// `None` when the running version is the latest.
pub async fn check(current: Version) -> Result<Option<Release>, String> {
    let answer = client()?
        .get(LATEST)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("Could not ask GitHub for updates: {e}"))?;
    let text = answer
        .text()
        .await
        .map_err(|e| format!("GitHub's answer was not readable: {e}"))?;
    let release: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("GitHub's answer was not readable: {e}"))?;
    Ok(newer(&release, current))
}

/// Downloads the release's DMG (and its detached signature) into `dir`; each has to
/// be the size GitHub named.
pub async fn download(release: &Release, dir: &Path) -> Result<Downloaded, String> {
    let dmg = dir.join(dmg_name(release.version));
    fetch(&release.dmg, &dmg).await?;
    let signature = match &release.signature {
        Some(sig) => {
            let path = dir.join(format!("{}.sig", dmg_name(release.version)));
            fetch(sig, &path).await?;
            Some(path)
        }
        None => None,
    };
    Ok(Downloaded { dmg, signature })
}

async fn fetch(download: &Download, path: &Path) -> Result<(), String> {
    let fail = |e: &dyn std::fmt::Display| format!("Could not download the update: {e}");
    let answer = client()?
        .get(&download.url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| fail(&e))?;
    let mut file = tokio::fs::File::create(path).await.map_err(|e| fail(&e))?;
    let mut bytes = answer.bytes_stream();
    let mut size = 0u64;
    while let Some(chunk) = bytes.next().await {
        let chunk = chunk.map_err(|e| fail(&e))?;
        size += chunk.len() as u64;
        file.write_all(&chunk).await.map_err(|e| fail(&e))?;
    }
    file.flush().await.map_err(|e| fail(&e))?;
    if size != download.size {
        return Err(fail(&format!(
            "got {size} bytes instead of {}",
            download.size
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release(tag: &str) -> serde_json::Value {
        json!({
            "tag_name": tag,
            "draft": false,
            "prerelease": false,
            "body": "## What's Changed\n* copy formats",
            "html_url": format!("https://github.com/haenning/tangul/releases/tag/{tag}"),
            "assets": [
                { "name": "notes.txt", "browser_download_url": "https://x/notes.txt", "size": 3 },
                {
                    "name": format!("tangul-{}-universal.dmg", tag.trim_start_matches('v')),
                    "browser_download_url": format!(
                        "https://github.com/haenning/tangul/releases/download/{tag}/tangul-{}-universal.dmg",
                        tag.trim_start_matches('v')
                    ),
                    "size": 34233856
                }
            ]
        })
    }

    /// Asks the real API (network): `cargo test -p tangul-update -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn github_names_a_newer_release_than_0_1_0() {
        let release = check(Version::parse("0.1.0").unwrap())
            .await
            .unwrap()
            .expect("a release newer than 0.1.0");
        assert_eq!(
            release.dmg.url,
            format!(
                "{DOWNLOADS}v{}/{}",
                release.version,
                dmg_name(release.version)
            )
        );
        assert!(release.dmg.size > 1_000_000, "{release:?}");
    }

    #[test]
    fn a_newer_release_with_its_dmg() {
        let current = Version::parse("0.2.3").unwrap();
        assert_eq!(
            newer(&release("v0.2.4"), current),
            Some(Release {
                version: Version::parse("0.2.4").unwrap(),
                notes: "## What's Changed\n* copy formats".into(),
                page: "https://github.com/haenning/tangul/releases/tag/v0.2.4".into(),
                dmg: Download {
                    url: "https://github.com/haenning/tangul/releases/download/v0.2.4/tangul-0.2.4-universal.dmg"
                        .into(),
                    size: 34233856,
                },
                signature: None,
            })
        );
        // With its detached signature (v0.2.5 on), from the same place.
        let mut with_sig = release("v0.2.5");
        with_sig["assets"].as_array_mut().unwrap().push(json!({
            "name": "tangul-0.2.5-universal.dmg.sig",
            "browser_download_url":
                "https://github.com/haenning/tangul/releases/download/v0.2.5/tangul-0.2.5-universal.dmg.sig",
            "size": 376
        }));
        assert_eq!(
            newer(&with_sig, current).unwrap().signature,
            Some(Download {
                url: "https://github.com/haenning/tangul/releases/download/v0.2.5/tangul-0.2.5-universal.dmg.sig"
                    .into(),
                size: 376,
            })
        );
        // A signature from elsewhere is none (and the DMG then needs one inside).
        with_sig["assets"][2]["browser_download_url"] = json!("https://evil.example/x.sig");
        assert_eq!(newer(&with_sig, current).unwrap().signature, None);
        // The same or an older one: nothing to do.
        assert_eq!(newer(&release("v0.2.3"), current), None);
        assert_eq!(newer(&release("v0.2.2"), current), None);
    }

    #[test]
    fn only_published_releases_with_the_dmg() {
        let current = Version::parse("0.2.3").unwrap();
        for (key, value) in [
            ("draft", json!(true)),
            ("prerelease", json!(true)),
            ("draft", json!(null)),
            ("tag_name", json!("v0.3.0-rc.1")),
            ("tag_name", json!(null)),
            ("assets", json!([])),
            ("html_url", json!(null)),
        ] {
            let mut r = release("v0.3.0");
            r[key] = value.clone();
            assert_eq!(newer(&r, current), None, "{key}: {value}");
        }
        // A DMG of another version doesn't count.
        let mut r = release("v0.3.0");
        r["assets"][1]["name"] = json!("tangul-0.2.9-universal.dmg");
        assert_eq!(newer(&r, current), None);
        // Only this repository's download of that very file.
        for url in [
            "https://x/tangul-0.3.0-universal.dmg",
            "http://github.com/haenning/tangul/releases/download/v0.3.0/tangul-0.3.0-universal.dmg",
            "https://github.com/someone/tangul/releases/download/v0.3.0/tangul-0.3.0-universal.dmg",
            "https://github.com/haenning/tangul/releases/download/v0.2.1/tangul-0.3.0-universal.dmg",
            "https://github.com/haenning/tangul/releases/download/v0.3.0/../x/tangul-0.3.0-universal.dmg",
            "https://github.com.evil.example/haenning/tangul/releases/download/v0.3.0/tangul-0.3.0-universal.dmg",
        ] {
            let mut r = release("v0.3.0");
            r["assets"][1]["browser_download_url"] = json!(url);
            assert_eq!(newer(&r, current), None, "{url}");
        }
        // No notes are fine.
        let mut r = release("v0.3.0");
        r["body"] = json!(null);
        assert_eq!(newer(&r, current).unwrap().notes, "");
    }
}
