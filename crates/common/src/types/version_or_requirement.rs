use std::{fmt::Display, str::FromStr};

use anyhow::{Error, bail};
use semver::{Version, VersionReq};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum VersionOrRequirement {
    Version(Version),
    Requirement(VersionReq),
}

impl From<Version> for VersionOrRequirement {
    fn from(value: Version) -> Self {
        Self::Version(value)
    }
}

impl From<VersionReq> for VersionOrRequirement {
    fn from(value: VersionReq) -> Self {
        Self::Requirement(value)
    }
}

impl TryFrom<VersionOrRequirement> for Version {
    type Error = anyhow::Error;

    fn try_from(value: VersionOrRequirement) -> Result<Self, Self::Error> {
        let VersionOrRequirement::Version(version) = value else {
            anyhow::bail!("Version or requirement was not a version");
        };
        Ok(version)
    }
}

impl TryFrom<VersionOrRequirement> for VersionReq {
    type Error = anyhow::Error;

    fn try_from(value: VersionOrRequirement) -> Result<Self, Self::Error> {
        let VersionOrRequirement::Requirement(requirement) = value else {
            anyhow::bail!("Version or requirement was not a requirement");
        };
        Ok(requirement)
    }
}

impl FromStr for VersionOrRequirement {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(version) = Version::parse(s) {
            Ok(Self::Version(version))
        } else if let Ok(version_req) = VersionReq::parse(s) {
            Ok(Self::Requirement(version_req))
        } else {
            bail!("Not a valid version or version requirement")
        }
    }
}

impl Display for VersionOrRequirement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VersionOrRequirement::Version(version) => version.fmt(f),
            VersionOrRequirement::Requirement(version_req) => version_req.fmt(f),
        }
    }
}
