use std::sync::LazyLock;

use regex::Regex;
use semver::{Prerelease, Version};
use thiserror::Error;

static RELEASE_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^v(\d+)\.(\d+)\.(\d+)$").unwrap());
static RC_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+\.\d+\.\d+-rc\.(\d+)$").unwrap());

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VersionError {
    #[error("tag {0:?} is not v followed by a semantic version")]
    NotSemver(String),
    #[error("version {version} would never be deployed: {current} is already published")]
    NotIncreasing { version: Version, current: Version },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allocation {
    pub version: Version,
    pub rc: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Publish {
    Deploys,
    AlreadyPublished,
    BelowCurrent { current: Version },
}

pub fn release_base<S: AsRef<str>>(git_tags: &[S]) -> Version {
    let highest = git_tags
        .iter()
        .filter_map(|tag| {
            let caps = RELEASE_TAG.captures(tag.as_ref())?;
            let part = |i: usize| caps[i].parse::<u64>().ok();
            Some(Version::new(part(1)?, part(2)?, part(3)?))
        })
        .max()
        .unwrap_or_else(|| Version::new(0, 0, 0));
    Version::new(highest.major, highest.minor, highest.patch + 1)
}

pub fn next_rc<S: AsRef<str>, P: AsRef<str>>(git_tags: &[S], published: &[P], high_water: u64) -> Allocation {
    let base = release_base(git_tags);
    let published_max =
        published.iter().filter_map(|tag| RC_TAG.captures(tag.as_ref())?[1].parse::<u64>().ok()).max().unwrap_or(0);
    let n = published_max.max(high_water) + 1;
    let mut version = base;
    version.pre = Prerelease::new(&format!("rc.{n}")).expect("rc prerelease is valid");
    Allocation { version, rc: Some(n) }
}

pub fn tag_version(tag: &str) -> Result<Allocation, VersionError> {
    let version = tag
        .strip_prefix('v')
        .and_then(|rest| Version::parse(rest).ok())
        .filter(|version| version.build.is_empty())
        .ok_or_else(|| VersionError::NotSemver(tag.to_owned()))?;
    Ok(Allocation { version, rc: None })
}

pub fn assess<P: AsRef<str>>(version: &Version, published: &[P]) -> Result<Publish, VersionError> {
    let versions: Vec<Version> = published.iter().filter_map(|tag| Version::parse(tag.as_ref()).ok()).collect();
    if versions.contains(version) {
        return Ok(Publish::AlreadyPublished);
    }
    match versions.into_iter().max() {
        Some(current) if current > *version => {
            if version.pre.is_empty() {
                Ok(Publish::BelowCurrent { current })
            } else {
                Err(VersionError::NotIncreasing { version: version.clone(), current })
            }
        }
        _ => Ok(Publish::Deploys),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: [&str; 0] = [];

    #[test]
    fn rc_numbers_continue_above_what_github_actions_published() {
        let continued = next_rc(&NONE, &["0.0.1-rc.54", "0.0.1-rc.55", "sha-abc"], 0);
        assert_eq!(continued.version.to_string(), "0.0.1-rc.56");
        let patched = next_rc(&["v0.1.0", "v0.1.1", "v0.1.1-beta.1", "release"], &["0.1.2-rc.21"], 0);
        assert_eq!(patched.version.to_string(), "0.1.2-rc.22");
        let after_db_loss = next_rc(&NONE, &["0.0.1-rc.9"], 12);
        assert_eq!(after_db_loss.rc, Some(13));
    }

    #[test]
    fn rc_numbers_order_numerically() {
        let nine = next_rc(&NONE, &["0.0.1-rc.8"], 0).version;
        let ten = next_rc(&NONE, &["0.0.1-rc.9"], 0).version;
        assert!(ten > nine);
    }

    #[test]
    fn versions_that_flux_would_ignore_are_refused() {
        let published = ["0.2.0", "0.1.2-rc.30"];
        let stale_rc = next_rc(&["v0.1.1"], &published, 0).version;
        assert!(matches!(assess(&stale_rc, &published), Err(VersionError::NotIncreasing { .. })));
        let hotfix = tag_version("v0.1.5").unwrap().version;
        assert!(matches!(assess(&hotfix, &published), Ok(Publish::BelowCurrent { .. })));
        let release = tag_version("v0.2.0").unwrap().version;
        assert_eq!(assess(&release, &published), Ok(Publish::AlreadyPublished));
        assert_eq!(assess(&tag_version("v0.3.0").unwrap().version, &published), Ok(Publish::Deploys));
    }

    #[test]
    fn tags_must_be_semver() {
        assert!(tag_version("v1.2").is_err());
        assert!(tag_version("1.2.3").is_err());
        assert!(tag_version("v1.2.3+build").is_err());
        assert_eq!(tag_version("v1.0.0-beta.1").unwrap().version.to_string(), "1.0.0-beta.1");
    }
}
