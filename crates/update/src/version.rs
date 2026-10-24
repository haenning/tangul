//! Release versions as tangul's tags write them: `v0.2.3` (or `0.2.3`).

use std::fmt;

/// `major.minor.patch`; ordered as numbers, so `0.10.0` is after `0.9.9`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// `0.2.3` or `v0.2.3`; `None` for anything else, a pre-release (`0.3.0-rc.1`)
    /// included.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
        let mut next = || -> Option<u64> {
            let part = parts.next()?;
            // Digits only: no signs, no build or pre-release suffixes.
            (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                .then(|| part.parse().ok())?
        };
        let version = Version {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        parts.next().is_none().then_some(version)
    }

    /// The running app's version.
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).expect("the workspace version is x.y.z")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn parses_tags_and_versions() {
        assert_eq!(Version::parse("v0.2.3"), Some(v(0, 2, 3)));
        assert_eq!(Version::parse(" 1.10.0 "), Some(v(1, 10, 0)));
        assert_eq!(v(0, 2, 3).to_string(), "0.2.3");
        for bad in [
            "",
            "v",
            "0.2",
            "0.2.3.4",
            "0.3.0-rc.1",
            "v0.2.x",
            "+1.2.3",
            "0..1",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn orders_as_numbers() {
        assert!(v(0, 10, 0) > v(0, 9, 9));
        assert!(v(1, 0, 0) > v(0, 99, 99));
        assert!(v(0, 2, 4) > v(0, 2, 3));
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }
}
