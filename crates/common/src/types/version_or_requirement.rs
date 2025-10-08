use semver::{Version, VersionReq};

#[derive(Clone, Debug)]
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
