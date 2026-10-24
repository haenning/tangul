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
    /// The universal DMG's download address and size in bytes.
    pub dmg: String,
    pub dmg_size: u64,
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
    let name = dmg_name(version);
    let asset = (release["assets"].as_array()?.iter()).find(|a| a["name"] == name.as_str())?;
    Some(Release {
        version,
        notes: release["body"].as_str().unwrap_or_default().to_string(),
        page: release["html_url"].as_str()?.to_string(),
        dmg: asset["browser_download_url"].as_str()?.to_string(),
        dmg_size: asset["size"].as_u64()?,
    })
}

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

/// Downloads the release's DMG into `dir`; its size has to be the one GitHub
/// named.
pub async fn download(release: &Release, dir: &Path) -> Result<PathBuf, String> {
    let fail = |e: &dyn std::fmt::Display| format!("Could not download the update: {e}");
    let path = dir.join(dmg_name(release.version));
    let answer = client()?
        .get(&release.dmg)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| fail(&e))?;
    let mut file = tokio::fs::File::create(&path).await.map_err(|e| fail(&e))?;
    let mut bytes = answer.bytes_stream();
    let mut size = 0u64;
    while let Some(chunk) = bytes.next().await {
        let chunk = chunk.map_err(|e| fail(&e))?;
        size += chunk.len() as u64;
        file.write_all(&chunk).await.map_err(|e| fail(&e))?;
    }
    file.flush().await.map_err(|e| fail(&e))?;
    if size != release.dmg_size {
        return Err(fail(&format!(
            "got {size} bytes instead of {}",
            release.dmg_size
        )));
    }
    Ok(path)
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
                    "browser_download_url": "https://x/tangul.dmg",
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
        assert!(release.dmg.ends_with(&dmg_name(release.version)), "{release:?}");
        assert!(release.dmg_size > 1_000_000, "{release:?}");
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
                dmg: "https://x/tangul.dmg".into(),
                dmg_size: 34233856,
            })
        );
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
        // No notes are fine.
        let mut r = release("v0.3.0");
        r["body"] = json!(null);
        assert_eq!(newer(&r, current).unwrap().notes, "");
    }
}
