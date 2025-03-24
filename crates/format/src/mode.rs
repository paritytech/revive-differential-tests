use serde::Deserialize;
use serde::de::Deserializer;

/// Specifies the compilation mode of the test artifact.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Mode {
    Solidity(SolcMode),
    Unknown(String),
}

/// Specify Solidity specific compiler options.
#[derive(Debug, Default, Clone, Eq, PartialEq)]
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
