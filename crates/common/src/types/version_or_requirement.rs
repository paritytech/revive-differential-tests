use semver::{Version, VersionReq};

#[derive(Clone, Debug)]
pub enum VersionOrRequirement {
    Version(Version),
    Requirement(VersionReq),
}

impl VersionOrRequirement {
    /// A helper function to convert a [`semver::Version`] into a [`semver::VersionReq`].
    pub fn version_to_requirement(version: &Version) -> VersionReq {
        // Ignoring "build" metadata in the version, we can turn
        // it into a requirement which is an exact match for the
        // given version and nothing else:
        VersionReq {
            comparators: vec![semver::Comparator {
                op: semver::Op::Exact,
                major: version.major,
                minor: Some(version.minor),
                patch: Some(version.patch),
                pre: version.pre.clone(),
            }],
        }
    }
}

impl serde::Serialize for VersionOrRequirement {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            VersionOrRequirement::Version(v) => serializer.serialize_str(&v.to_string()),
            VersionOrRequirement::Requirement(r) => serializer.serialize_str(&r.to_string()),
        }
    }
}

impl Default for VersionOrRequirement {
    fn default() -> Self {
        VersionOrRequirement::Requirement(VersionReq::STAR)
    }
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

impl From<VersionOrRequirement> for VersionReq {
    fn from(value: VersionOrRequirement) -> Self {
        match value {
            VersionOrRequirement::Version(version) => {
                VersionOrRequirement::version_to_requirement(&version)
            }
            VersionOrRequirement::Requirement(version_req) => version_req,
        }
    }
}

impl TryFrom<VersionOrRequirement> for Version {
    type Error = anyhow::Error;

    fn try_from(value: VersionOrRequirement) -> Result<Self, Self::Error> {
        match value {
            VersionOrRequirement::Version(version) => Ok(version),
            VersionOrRequirement::Requirement(mut version_req) => {
                if version_req.comparators.len() != 1 {
                    anyhow::bail!(
                        "The version requirement in VersionOrRequirement is not a single exact version"
                    );
                }

                let c = version_req.comparators.pop().unwrap();
                let (semver::Op::Exact, Some(minor), Some(patch)) = (c.op, c.minor, c.patch) else {
                    anyhow::bail!(
                        "The version requirement in VersionOrRequirement is not an exact version"
                    );
                };

                Ok(Version {
                    major: c.major,
                    minor,
                    patch,
                    pre: c.pre,
                    build: Default::default(),
                })
            }
        }
    }
}
