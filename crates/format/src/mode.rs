use revive_dt_common::types::VersionOrRequirement;
use semver::Version;
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};

/// Specifies the compilation mode of the test artifact.
#[derive(Hash, Debug, Clone, Eq, PartialEq)]
pub enum Mode {
    Solidity(SolcMode),
    Unknown(String),
}

/// Specify Solidity specific compiler options.
#[derive(Hash, Debug, Default, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct SolcMode {
    pub solc_version: Option<semver::VersionReq>,
    solc_optimize: Option<bool>,
    pub llvm_optimizer_settings: Vec<String>,
}

impl SolcMode {
    /// Try to parse a mode string into a solc mode.
    /// Returns `None` if the string wasn't a solc YUL mode string.
    ///
    /// The mode string is expected to start with the `Y` ID (YUL ID),
    /// optionally followed by `+` or `-` for the solc optimizer settings.
    ///
    /// Options can be separated by a whitespace contain the following
    /// - A solc `SemVer version requirement` string
    /// - One or more `-OX` where X is a supposed to be an LLVM opt mode
    pub fn parse_from_mode_string(mode_string: &str) -> Option<Self> {
        let mut result = Self::default();

        let mut parts = mode_string.trim().split(" ");

        match parts.next()? {
            "Y" => {}
            "Y+" => result.solc_optimize = Some(true),
            "Y-" => result.solc_optimize = Some(false),
            _ => return None,
        }

        for part in parts {
            if let Ok(solc_version) = semver::VersionReq::parse(part) {
                result.solc_version = Some(solc_version);
                continue;
            }
            if let Some(level) = part.strip_prefix("-O") {
                result.llvm_optimizer_settings.push(level.to_string());
                continue;
            }
            panic!("the YUL mode string {mode_string} failed to parse, invalid part: {part}")
        }

        Some(result)
    }

    /// Returns whether to enable the solc optimizer.
    pub fn solc_optimize(&self) -> bool {
        self.solc_optimize.unwrap_or(true)
    }

    /// Calculate the latest matching solc patch version. Returns:
    /// - `latest_supported` if no version request was specified.
    /// - A matching version with the same minor version as `latest_supported`, if any.
    /// - `None` if no minor version of the `latest_supported` version matches.
    pub fn last_patch_version(&self, latest_supported: &Version) -> Option<Version> {
        let Some(version_req) = self.solc_version.as_ref() else {
            return Some(latest_supported.to_owned());
        };

        // lgtm
        for patch in (0..latest_supported.patch + 1).rev() {
            let version = Version::new(0, latest_supported.minor, patch);
            if version_req.matches(&version) {
                return Some(version);
            }
        }

        None
    }

    /// Resolves the [`SolcMode`]'s solidity version requirement into a [`VersionOrRequirement`] if
    /// the requirement is present on the object. Otherwise, the passed default version is used.
    pub fn compiler_version_to_use(&self, default: Version) -> VersionOrRequirement {
        match self.solc_version {
            Some(ref requirement) => requirement.clone().into(),
            None => default.into(),
        }
    }
}

impl<'de> Deserialize<'de> for Mode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mode_string = String::deserialize(deserializer)?;

        if let Some(solc_mode) = SolcMode::parse_from_mode_string(&mode_string) {
            return Ok(Self::Solidity(solc_mode));
        }

        Ok(Self::Unknown(mode_string))
    }
}
