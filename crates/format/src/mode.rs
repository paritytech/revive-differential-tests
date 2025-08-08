use regex::Regex;
use revive_dt_common::types::VersionOrRequirement;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt::Display;
use std::str::FromStr;
use std::sync::LazyLock;

/// This represents a mode that a given test should be run with, if possible.
///
/// We obtain this by taking a [`ParsedMode`], which may be looser or more strict
/// in its requirements, and then expanding it out into a list of [`Mode`]s.
///
/// Use [`ParsedMode::to_test_modes()`] to do this.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct Mode {
    pub pipeline: ModePipeline,
    pub optimize_setting: ModeOptimizerSetting,
    pub version: Option<semver::VersionReq>,
}

impl Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt_mode_parts(
            Some(&self.pipeline),
            None,
            Some(&self.optimize_setting),
            self.version.as_ref(),
            f,
        )
    }
}

impl Mode {
    /// Return a set of [`TestMode`]s that correspond to the given [`ParsedMode`]s.
    /// This avoids any duplicate entries.
    pub fn from_parsed_modes<'a>(
        parsed: impl Iterator<Item = &'a ParsedMode>,
    ) -> impl Iterator<Item = Self> {
        let modes: HashSet<_> = parsed.flat_map(|p| p.to_test_modes()).collect();
        modes.into_iter()
    }

    /// Return all of the test modes that we want to run when no specific [`ParsedMode`] is specified.
    pub fn all() -> impl Iterator<Item = Self> {
        ParsedMode {
            pipeline: None,
            optimize_flag: None,
            optimize_setting: None,
            version: None,
        }
        .to_test_modes()
    }

    /// Resolves the [`Mode`]'s solidity version requirement into a [`VersionOrRequirement`] if
    /// the requirement is present on the object. Otherwise, the passed default version is used.
    pub fn compiler_version_to_use(&self, default: Version) -> VersionOrRequirement {
        match self.version {
            Some(ref requirement) => requirement.clone().into(),
            None => default.into(),
        }
    }

    /// Should we go via Yul IR?
    pub fn via_yul_ir(&self) -> bool {
        self.pipeline == ModePipeline::Y
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
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
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

        let optimize_flag = caps.name("optimize_flag").map(|m| m.as_str() == "+");

        let optimize_setting = match caps.name("optimize_setting") {
            Some(m) => Some(ModeOptimizerSetting::from_str(m.as_str())?),
            None => None,
        };

        let version = match caps.name("version") {
            Some(m) => Some(
                semver::VersionReq::parse(m.as_str())
                    .map_err(|e| ParseModeError::InvalidVersion(e.to_string()))?,
            ),
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
        fmt_mode_parts(
            self.pipeline.as_ref(),
            self.optimize_flag,
            self.optimize_setting.as_ref(),
            self.version.as_ref(),
            f,
        )
    }
}

impl From<ParsedMode> for String {
    fn from(parsed_mode: ParsedMode) -> Self {
        parsed_mode.to_string()
    }
}

impl TryFrom<String> for ParsedMode {
    type Error = ParseModeError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        ParsedMode::from_str(&value)
    }
}

fn fmt_mode_parts(
    pipeline: Option<&ModePipeline>,
    optimize_flag: Option<bool>,
    optimize_setting: Option<&ModeOptimizerSetting>,
    version: Option<&semver::VersionReq>,
    f: &mut std::fmt::Formatter<'_>,
) -> std::fmt::Result {
    let mut has_written = false;

    if let Some(pipeline) = pipeline {
        pipeline.fmt(f)?;
        if let Some(optimize_flag) = optimize_flag {
            f.write_str(if optimize_flag { "+" } else { "-" })?;
        }
        has_written = true;
    }

    if let Some(optimize_setting) = optimize_setting {
        if has_written {
            f.write_str(" ")?;
        }
        optimize_setting.fmt(f)?;
        has_written = true;
    }

    if let Some(version) = version {
        if has_written {
            f.write_str(" ")?;
        }
        version.fmt(f)?;
    }

    Ok(())
}

impl ParsedMode {
    /// This takes a [`ParsedMode`] and expands it into a list of [`TestMode`]s that we should try.
    pub fn to_test_modes(&self) -> impl Iterator<Item = Mode> {
        let pipeline_iter = self.pipeline.as_ref().map_or_else(
            || EitherIter::A(ModePipeline::test_cases()),
            |p| EitherIter::B(std::iter::once(*p)),
        );

        let optimize_flag_setting = self.optimize_flag.map(|flag| {
            if flag {
                ModeOptimizerSetting::M3
            } else {
                ModeOptimizerSetting::M0
            }
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
            optimize_settings_iter
                .clone()
                .map(move |optimize_setting| Mode {
                    pipeline,
                    optimize_setting,
                    version: self.version.clone(),
                })
        })
    }
}

#[derive(thiserror::Error, Debug, Clone)]
pub enum ParseModeError {
    #[error(
        "Cannot parse compiler mode via regex: expecting something like 'Y', 'E+ <0.8', 'Y Mz =0.8'"
    )]
    CannotParse,
    #[error("Unsupported compiler pipeline mode: {0}. We support Y and E modes only.")]
    UnsupportedPipeline(char),
    #[error("Unsupported optimizer setting: {0}. We support M0, M1, M2, M3, Ms, and Mz only.")]
    UnsupportedOptimizerSetting(String),
    #[error("Invalid semver specifier: {0}")]
    InvalidVersion(String),
}

/// What do we want the compiler to do?
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
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
            _ => Err(ParseModeError::UnsupportedPipeline(
                s.chars().next().unwrap_or('?'),
            )),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
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
            _ => Err(ParseModeError::UnsupportedOptimizerSetting(s.to_owned())),
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

    /// Are any optimizations enabled?
    pub fn optimizations_enabled(&self) -> bool {
        !matches!(self, ModeOptimizerSetting::M0)
    }
}

/// An iterator that could be either of two iterators.
#[derive(Clone, Debug)]
enum EitherIter<A, B> {
    A(A),
    B(B),
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
            // We don't see this in the wild but it is parsed.
            ("<=0.8", "<=0.8"),
        ];

        for (actual, expected) in strings {
            let parsed = ParsedMode::from_str(actual)
                .expect(format!("Failed to parse mode string '{actual}'").as_str());
            assert_eq!(
                expected,
                parsed.to_string(),
                "Mode string '{actual}' did not parse to '{expected}': got '{parsed}'"
            );
        }
    }

    #[test]
    fn test_parsed_mode_to_test_modes() {
        let strings = vec![
            ("Mz", vec!["Y Mz", "E Mz"]),
            ("Y", vec!["Y M0", "Y M3"]),
            ("E", vec!["E M0", "E M3"]),
            ("Y+", vec!["Y M3"]),
            ("Y-", vec!["Y M0"]),
            ("Y <=0.8", vec!["Y M0 <=0.8", "Y M3 <=0.8"]),
            (
                "<=0.8",
                vec!["Y M0 <=0.8", "Y M3 <=0.8", "E M0 <=0.8", "E M3 <=0.8"],
            ),
        ];

        for (actual, expected) in strings {
            let parsed = ParsedMode::from_str(actual)
                .expect(format!("Failed to parse mode string '{actual}'").as_str());
            let expected_set: HashSet<_> = expected.into_iter().map(|s| s.to_owned()).collect();
            let actual_set: HashSet<_> = parsed.to_test_modes().map(|m| m.to_string()).collect();

            assert_eq!(
                expected_set, actual_set,
                "Mode string '{actual}' did not expand to '{expected_set:?}': got '{actual_set:?}'"
            );
        }
    }
}
