use revive_dt_common::types::VersionOrRequirement;
use semver::Version;
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::sync::LazyLock;
use std::str::FromStr;
use regex::Regex;

/// This represents a mode that a given test should be run with, if possible.
/// 
/// We obtain this by taking a [`ParsedMode`], which may be looser or more strict
/// in its requirements, and then expanding it out into a list of [`TestMode`]s.
/// 
/// Use [`ParsedMode::to_test_modes()`] to do this.
#[derive(Clone, Debug)]
pub struct TestMode {
    pub pipeline: ModePipeline,
    pub optimize_setting: ModeOptimizerSetting,
    pub version: Option<semver::VersionReq>,
}

impl TestMode {
    /// Return a set of [`TestMode`]s that correspond to the given [`ParsedMode`]. 
    pub fn from_parsed_mode(parsed: &ParsedMode) -> impl Iterator<Item = Self> {
        parsed.to_test_modes()
    }
}

/// This represents a mode that has been parsed from test metadata.
/// 
/// Mode strings can take the following form (in pseudo-regex):
/// 
/// ```text
/// [YEILV][+-]? (M[0123sz])? <semver>?
/// ```
/// 
/// We can parse valid mode strings into [`ParsedMode`] using [`ParsedMode::from_str`].
#[derive(Clone, Debug)]
pub struct ParsedMode {
    pub pipeline: Option<ModePipeline>,
    pub optimize_flag: Option<bool>,
    pub optimize_setting: Option<ModeOptimizerSetting>,
    pub version: Option<semver::VersionReq>,
}

impl FromStr for ParsedMode {
    type Err = ParseModeError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        static REGEX: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?x)
                ^
                (?:(?P<pipeline>[YEILV])(?P<optimize_flag>[+-])?)? # Pipeline to use eg Y, E+, E-
                \s*
                (?P<optimize_setting>M[a-zA-Z0-9])?                # Optimize setting eg M0, Ms, Mz
                \s*
                (?P<version>[>=<]*\d+(?:\.\d+)*)?                  # Optional semver version eg >=0.8.0, 0.7, <0.8
                $
            ").unwrap()
        });
        
        let Some(caps) = REGEX.captures(s) else {
            return Err(ParseModeError::CannotParse);
        };

        let pipeline = match caps.name("pipeline") {
            Some(m) => Some(ModePipeline::from_str(m.as_str())?),
            None => None,
        };

        let optimize_flag = match caps.name("optimize_flag") {
            Some(m) => Some(m.as_str() == "+"),
            None => None,
        };

        let optimize_setting = match caps.name("optimize_setting") {
            Some(m) => Some(ModeOptimizerSetting::from_str(m.as_str())?),
            None => None,
        };

        let version = match caps.name("version") {
            Some(m) => Some(semver::VersionReq::parse(m.as_str()).map_err(|e| ParseModeError::InvalidVersion(e.to_string()))?),
            None => None,
        };

        Ok(ParsedMode {
            pipeline,
            optimize_flag,
            optimize_setting,
            version,
        })
    }
}

impl Display for ParsedMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut has_written = false;

        if let Some(pipeline) = &self.pipeline {
            pipeline.fmt(f)?;
            if let Some(optimize_flag) = self.optimize_flag {
                f.write_str(if optimize_flag { "+" } else { "-" })?;
            }
            has_written = true;
        }

        if let Some(optimize_setting) = &self.optimize_setting {
            if has_written { f.write_str(" ")?; }
            optimize_setting.fmt(f)?;
            has_written = true;
        }

        if let Some(version) = &self.version {
            if has_written { f.write_str(" ")?; }
            version.fmt(f)?;
        }

        Ok(())
    }
}

impl ParsedMode {
    /// This takes a [`ParsedMode`] and expands it into a list of [`TestMode`]s that we should try.
    pub fn to_test_modes(&self) -> impl Iterator<Item = TestMode> {
        let pipeline_iter = self.pipeline.as_ref().map_or_else(
            || EitherIter::A(ModePipeline::test_cases()),
            |p| EitherIter::B(std::iter::once(*p)),
        );

        let optimize_flag_setting = self.optimize_flag.map(|b| {
            if b { ModeOptimizerSetting::M3 } else { ModeOptimizerSetting::M0 }
        });

        let optimize_flag_iter = match optimize_flag_setting {
            Some(setting) => EitherIter::A(std::iter::once(setting)),
            None => EitherIter::B(ModeOptimizerSetting::test_cases()),
        };

        let optimize_settings_iter = self.optimize_setting.as_ref().map_or_else(
            || EitherIter::A(optimize_flag_iter),
            |s| EitherIter::B(std::iter::once(*s)),
        );

        pipeline_iter.flat_map(move |pipeline| {
            optimize_settings_iter.clone().map(move |optimize_setting| {
                TestMode {
                    pipeline,
                    optimize_setting,
                    version: self.version.clone(),
                }
            })
        })
    }
}

#[derive(thiserror::Error, Debug, Clone)]
pub enum ParseModeError {
    #[error("Cannot parse compiler mode via regex: expecting something like 'Y', 'E+ <0.8', 'Y Mz =0.8'")]
    CannotParse,
    #[error("Unsupported compiler pipeline mode: {0}. We support Y and E modes only.")]
    UnsupportedPipeline(char),
    #[error("Unsupported optimizer setting: {0}. We support M0, M1, M2, M3, Ms, and Mz only.")]
    UnsupportedOptimizerSetting(String),
    #[error("Invalid semver specifier: {0}")]
    InvalidVersion(String),
}

/// What do we want the compiler to do?
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ModePipeline {
    /// Compile Solidity code via Yul IR
    Y,
    /// Compile Solidity direct to assembly
    E,
}

impl FromStr for ModePipeline {
    type Err = ParseModeError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            // via Yul IR
            "Y" => Ok(ModePipeline::Y),
            // Don't go via Yul IR
            "E" => Ok(ModePipeline::E),
            // Anything else that we see isn't a mode at all
            _ => Err(ParseModeError::UnsupportedPipeline(s.chars().next().unwrap_or('?')))
        }
    }
}

impl Display for ModePipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModePipeline::Y => f.write_str("Y"),
            ModePipeline::E => f.write_str("E"),
        }
    }
}

impl ModePipeline {
    /// An iterator over the available pipelines that we'd like to test,
    /// when an explicit pipeline was not specified.
    pub fn test_cases() -> impl Iterator<Item = ModePipeline> + Clone {
        [ModePipeline::Y, ModePipeline::E].into_iter()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum ModeOptimizerSetting {
    /// 0 / -: Don't apply any optimizations
    M0,
    /// 1: Apply less than default optimizations
    M1,
    /// 2: Apply the default optimizations
    M2,
    /// 3 / +: Apply aggressive optimizations
    M3,
    /// s: Optimize for size
    Ms,
    /// z: Aggressively optimize for size
    Mz,
}

impl FromStr for ModeOptimizerSetting {
    type Err = ParseModeError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "M0" => Ok(ModeOptimizerSetting::M0),
            "M1" => Ok(ModeOptimizerSetting::M1),
            "M2" => Ok(ModeOptimizerSetting::M2),
            "M3" => Ok(ModeOptimizerSetting::M3),
            "Ms" => Ok(ModeOptimizerSetting::Ms),
            "Mz" => Ok(ModeOptimizerSetting::Mz),
            _ => Err(ParseModeError::UnsupportedOptimizerSetting(s.to_owned()))
        }
    }
}

impl Display for ModeOptimizerSetting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModeOptimizerSetting::M0 => f.write_str("M0"),
            ModeOptimizerSetting::M1 => f.write_str("M1"),
            ModeOptimizerSetting::M2 => f.write_str("M2"),
            ModeOptimizerSetting::M3 => f.write_str("M3"),
            ModeOptimizerSetting::Ms => f.write_str("Ms"),
            ModeOptimizerSetting::Mz => f.write_str("Mz"),
        }
    }
}

impl ModeOptimizerSetting {
    /// An iterator over the available optimizer settings that we'd like to test,
    /// when an explicit optimizer setting was not specified.
    pub fn test_cases() -> impl Iterator<Item = ModeOptimizerSetting> + Clone {
        [
            // No optimizations:
            ModeOptimizerSetting::M0,
            // Aggressive optimizations:
            ModeOptimizerSetting::M3,
        ]
        .into_iter()
    }
}

/// An iterator that could be either of two iterators.
#[derive(Clone, Debug)]
enum EitherIter<A, B> {
    A(A),
    B(B)
}

impl<A, B> Iterator for EitherIter<A, B>
where
    A: Iterator,
    B: Iterator<Item = A::Item>,
{
    type Item = A::Item;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            EitherIter::A(iter) => iter.next(),
            EitherIter::B(iter) => iter.next(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parsed_mode_from_str() {
        let strings = vec![
            ("Mz", "Mz"),
            ("Y", "Y"),
            ("Y+", "Y+"),
            ("Y-", "Y-"),
            ("E", "E"),
            ("E+", "E+"),
            ("E-", "E-"),
            ("Y M0", "Y M0"),
            ("Y M1", "Y M1"),
            ("Y M2", "Y M2"),
            ("Y M3", "Y M3"),
            ("Y Ms", "Y Ms"),
            ("Y Mz", "Y Mz"),
            ("E M0", "E M0"),
            ("E M1", "E M1"),
            ("E M2", "E M2"),
            ("E M3", "E M3"),
            ("E Ms", "E Ms"),
            ("E Mz", "E Mz"),
            // When stringifying semver again, 0.8.0 becomes ^0.8.0 (same meaning)
            ("Y 0.8.0", "Y ^0.8.0"),
            ("E+ 0.8.0", "E+ ^0.8.0"),
            ("Y M3 >=0.8.0", "Y M3 >=0.8.0"),
            ("E Mz <0.7.0", "E Mz <0.7.0"),
            // We can parse +- _and_ M1/M2 but the latter takes priority.
            ("Y+ M1 0.8.0", "Y+ M1 ^0.8.0"),
            ("E- M2 0.7.0", "E- M2 ^0.7.0"),
        ];

        for (actual, expected) in strings {
            let parsed = ParsedMode::from_str(actual).expect(format!("Failed to parse mode string '{actual}'").as_str());
            assert_eq!(expected, parsed.to_string(), "Mode string '{actual}' did not parse to '{expected}': got '{parsed}'");
        }
    }

}







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
