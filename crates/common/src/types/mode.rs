use crate::iterators::EitherIter;
use crate::types::VersionOrRequirement;
use anyhow::{Context as _, bail};
use regex::Regex;
use schemars::JsonSchema;
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
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Mode {
    pub pipeline: ModePipeline,
    pub optimize_setting: ModeOptimizerSetting,
    pub version: Option<semver::VersionReq>,
}

impl Ord for Mode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.to_string().cmp(&other.to_string())
    }
}

impl PartialOrd for Mode {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.pipeline.fmt(f)?;
        f.write_str(" ")?;
        self.optimize_setting.fmt(f)?;

        if let Some(version) = &self.version {
            f.write_str(" ")?;
            version.fmt(f)?;
        }

        Ok(())
    }
}

impl FromStr for Mode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parsed_mode = ParsedMode::from_str(s)?;
        let mut iter = parsed_mode.to_modes();
        let (Some(mode), None) = (iter.next(), iter.next()) else {
            bail!("Failed to parse the mode")
        };
        Ok(mode)
    }
}

impl Mode {
    /// Return all of the available mode combinations that we'd like to test.
    pub fn all() -> impl Iterator<Item = &'static Mode> {
        static ALL_MODES: LazyLock<Vec<Mode>> = LazyLock::new(|| {
            ModePipeline::test_cases()
                .flat_map(|pipeline| {
                    ModeOptimizerSetting::test_cases().map(move |optimize_setting| Mode {
                        pipeline,
                        optimize_setting,
                        version: None,
                    })
                })
                .collect::<Vec<_>>()
        });
        ALL_MODES.iter()
    }

    /// Resolves the [`Mode`]'s solidity version requirement into a [`VersionOrRequirement`] if
    /// the requirement is present on the object. Otherwise, the passed default version is used.
    pub fn compiler_version_to_use(&self, default: Version) -> VersionOrRequirement {
        match self.version {
            Some(ref requirement) => requirement.clone().into(),
            None => default.into(),
        }
    }
}

/// What do we want the compiler to do?
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum ModePipeline {
    /// Compile Solidity code via Yul IR
    ViaYulIR,
    /// Compile Solidity direct to assembly
    ViaEVMAssembly,
}

impl FromStr for ModePipeline {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            // via Yul IR
            "Y" => Ok(ModePipeline::ViaYulIR),
            // Don't go via Yul IR
            "E" => Ok(ModePipeline::ViaEVMAssembly),
            // Anything else that we see isn't a mode at all
            _ => Err(anyhow::anyhow!(
                "Unsupported pipeline '{s}': expected 'Y' or 'E'"
            )),
        }
    }
}

impl Display for ModePipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModePipeline::ViaYulIR => f.write_str("Y"),
            ModePipeline::ViaEVMAssembly => f.write_str("E"),
        }
    }
}

impl ModePipeline {
    /// Should we go via Yul IR?
    pub fn via_yul_ir(&self) -> bool {
        matches!(self, ModePipeline::ViaYulIR)
    }

    /// An iterator over the available pipelines that we'd like to test,
    /// when an explicit pipeline was not specified.
    pub fn test_cases() -> impl Iterator<Item = ModePipeline> + Clone {
        [ModePipeline::ViaYulIR, ModePipeline::ViaEVMAssembly].into_iter()
    }
}

/// Optimizer configuration for compilation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ModeOptimizerSetting {
    /// Whether the solc optimizer is enabled.
    pub solc_optimizer_enabled: bool,
    /// The resolc optimization level (used for LLVM optimizations).
    pub level: ModeOptimizerLevel,
}

impl Default for ModeOptimizerSetting {
    fn default() -> Self {
        Self {
            solc_optimizer_enabled: true,
            level: ModeOptimizerLevel::Mz,
        }
    }
}

impl Display for ModeOptimizerSetting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.level.fmt(f)?;
        f.write_str(" ")?;
        f.write_str(if self.solc_optimizer_enabled {
            "S+"
        } else {
            "S-"
        })
    }
}

impl ModeOptimizerSetting {
    /// An iterator over the available optimizer settings that we'd like to test,
    /// when an explicit optimizer setting was not specified.
    pub fn test_cases() -> impl Iterator<Item = ModeOptimizerSetting> + Clone {
        Self::solc_optimizer_test_cases().flat_map(|solc_optimizer_enabled| {
            ModeOptimizerLevel::test_cases().map(move |level| ModeOptimizerSetting {
                solc_optimizer_enabled,
                level,
            })
        })
    }

    /// An iterator over the available solc optimizer settings that we'd like to test,
    /// when an explicit solc optimizer setting was not specified.
    pub fn solc_optimizer_test_cases() -> impl Iterator<Item = bool> + Clone {
        [true, false].into_iter()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub enum ModeOptimizerLevel {
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

impl FromStr for ModeOptimizerLevel {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "M0" => Ok(ModeOptimizerLevel::M0),
            "M1" => Ok(ModeOptimizerLevel::M1),
            "M2" => Ok(ModeOptimizerLevel::M2),
            "M3" => Ok(ModeOptimizerLevel::M3),
            "Ms" => Ok(ModeOptimizerLevel::Ms),
            "Mz" => Ok(ModeOptimizerLevel::Mz),
            _ => Err(anyhow::anyhow!(
                "Unsupported optimizer level '{s}': expected 'M0', 'M1', 'M2', 'M3', 'Ms' or 'Mz'"
            )),
        }
    }
}

impl Display for ModeOptimizerLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModeOptimizerLevel::M0 => f.write_str("M0"),
            ModeOptimizerLevel::M1 => f.write_str("M1"),
            ModeOptimizerLevel::M2 => f.write_str("M2"),
            ModeOptimizerLevel::M3 => f.write_str("M3"),
            ModeOptimizerLevel::Ms => f.write_str("Ms"),
            ModeOptimizerLevel::Mz => f.write_str("Mz"),
        }
    }
}

impl ModeOptimizerLevel {
    /// Returns the optimization level as the corresponding mode character for resolc.
    pub fn to_mode_char(&self) -> char {
        match self {
            ModeOptimizerLevel::M0 => '0',
            ModeOptimizerLevel::M1 => '1',
            ModeOptimizerLevel::M2 => '2',
            ModeOptimizerLevel::M3 => '3',
            ModeOptimizerLevel::Ms => 's',
            ModeOptimizerLevel::Mz => 'z',
        }
    }

    /// An iterator over the available optimizer levels that we'd like to test,
    /// when an explicit optimizer level was not specified.
    pub fn test_cases() -> impl Iterator<Item = ModeOptimizerLevel> + Clone {
        [
            // No optimizations:
            ModeOptimizerLevel::M0,
            // Aggressive performance optimizations:
            ModeOptimizerLevel::M3,
            // Aggressive size optimizations:
            ModeOptimizerLevel::Mz,
        ]
        .into_iter()
    }
}

/// This represents a mode that has been parsed from test metadata.
///
/// Mode strings can take the following form (in pseudo-regex):
///
/// ```text
/// [YEILV][+-]? (M[0123sz])? (S[+-])? <semver>?
/// ```
///
/// - `[YEILV]`: Pipeline — `Y` (via Yul IR) or `E` (via EVM Assembly), ILV (legacy aliases)
/// - `[+-]`: Optimization shorthand — `+` (optimized: M3, solc optimizer enabled) or `-` (unoptimized: M0, solc optimizer disabled)
/// - `M[0123sz]`: Resolc/LLVM optimization level — `M0`..`M3`, `Ms`, `Mz`
/// - `S[+-]`: Solc optimizer — `S+` (enabled) or `S-` (disabled)
/// - `<semver>`: Version requirement
///
/// Priority:
/// - Explicit `M`/`S` settings override the `+`/`-` shorthand.
/// - If omitted, expands to all combinations we'd like to test. E.g.:
///   - `Y M3` → `Y M3 S+` and `Y M3 S-`
///   - `Y S+` → `Y M0 S+`, `Y M3 S+`, and `Y Mz S+`
///
/// Examples: `Y+`, `Y M3`, `Y Mz S- >=0.8.0`
///
/// We can parse valid mode strings into [`ParsedMode`] using [`ParsedMode::from_str`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize, Serialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct ParsedMode {
    pub pipeline: Option<ModePipeline>,
    pub optimize_flag: Option<bool>,
    pub optimize_level: Option<ModeOptimizerLevel>,
    pub solc_optimizer_enabled: Option<bool>,
    pub version: Option<semver::VersionReq>,
}

impl FromStr for ParsedMode {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        static REGEX: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?x)
                ^
                (?:(?P<pipeline>[YEILV])(?P<optimize_flag>[+-])?)? # Pipeline to use e.g. Y, E+, E-
                \s*
                (?P<optimize_level>M[a-zA-Z0-9])?                  # Optimize level e.g. M0, Ms, Mz
                \s*
                (?:S(?P<solc_optimizer_enabled>[+-]))?             # Solc optimizer e.g. S+, S-
                \s*
                (?P<version>[>=<^]*\d+(?:\.\d+)*)?                 # Optional semver version e.g. >=0.8.0, 0.7, <0.8
                $
            ").unwrap()
        });

        let Some(caps) = REGEX.captures(s) else {
            anyhow::bail!("Cannot parse mode '{s}' from string");
        };

        let pipeline = match caps.name("pipeline") {
            Some(m) => Some(
                ModePipeline::from_str(m.as_str())
                    .context("Failed to parse mode pipeline from string")?,
            ),
            None => None,
        };

        let optimize_flag = caps.name("optimize_flag").map(|m| m.as_str() == "+");

        let optimize_level = match caps.name("optimize_level") {
            Some(m) => Some(
                ModeOptimizerLevel::from_str(m.as_str())
                    .context("Failed to parse optimizer level from string")?,
            ),
            None => None,
        };

        let solc_optimizer_enabled = caps
            .name("solc_optimizer_enabled")
            .map(|m| m.as_str() == "+");

        let version = match caps.name("version") {
            Some(m) => Some(
                semver::VersionReq::parse(m.as_str())
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "Cannot parse the version requirement '{}': {e}",
                            m.as_str()
                        )
                    })
                    .context("Failed to parse semver requirement from mode string")?,
            ),
            None => None,
        };

        Ok(ParsedMode {
            pipeline,
            optimize_flag,
            optimize_level,
            solc_optimizer_enabled,
            version,
        })
    }
}

impl Display for ParsedMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut has_written = false;

        if let Some(pipeline) = self.pipeline {
            pipeline.fmt(f)?;
            if let Some(optimize_flag) = self.optimize_flag {
                f.write_str(if optimize_flag { "+" } else { "-" })?;
            }
            has_written = true;
        }

        if let Some(optimize_level) = self.optimize_level {
            if has_written {
                f.write_str(" ")?;
            }
            optimize_level.fmt(f)?;
            has_written = true;
        }

        if let Some(solc_optimizer_enabled) = self.solc_optimizer_enabled {
            if has_written {
                f.write_str(" ")?;
            }
            f.write_str(if solc_optimizer_enabled { "S+" } else { "S-" })?;
            has_written = true;
        }

        if let Some(version) = &self.version {
            if has_written {
                f.write_str(" ")?;
            }
            version.fmt(f)?;
        }

        Ok(())
    }
}

impl From<ParsedMode> for String {
    fn from(parsed_mode: ParsedMode) -> Self {
        parsed_mode.to_string()
    }
}

impl TryFrom<String> for ParsedMode {
    type Error = anyhow::Error;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        ParsedMode::from_str(&value)
    }
}

impl ParsedMode {
    /// This takes a [`ParsedMode`] and expands it into a list of [`Mode`]s that we should try.
    pub fn to_modes(&self) -> impl Iterator<Item = Mode> {
        let pipeline_iter = self.pipeline.as_ref().map_or_else(
            || EitherIter::A(ModePipeline::test_cases()),
            |p| EitherIter::B(std::iter::once(*p)),
        );

        let optimize_flag_setting = self.optimize_flag.map(|flag| {
            if flag {
                ModeOptimizerSetting {
                    solc_optimizer_enabled: true,
                    level: ModeOptimizerLevel::M3,
                }
            } else {
                ModeOptimizerSetting {
                    solc_optimizer_enabled: false,
                    level: ModeOptimizerLevel::M0,
                }
            }
        });

        let optimize_flag_iter = match optimize_flag_setting {
            Some(setting) => EitherIter::A(std::iter::once(setting)),
            None => EitherIter::B(ModeOptimizerSetting::test_cases()),
        };

        let optimize_settings: Vec<ModeOptimizerSetting> =
            match (self.solc_optimizer_enabled, self.optimize_level) {
                (Some(solc_optimizer_enabled), Some(level)) => {
                    vec![ModeOptimizerSetting {
                        solc_optimizer_enabled,
                        level,
                    }]
                }
                (None, Some(level)) => ModeOptimizerSetting::solc_optimizer_test_cases()
                    .map(|solc_optimizer_enabled| ModeOptimizerSetting {
                        solc_optimizer_enabled,
                        level,
                    })
                    .collect(),
                (Some(solc_optimizer_enabled), None) => ModeOptimizerLevel::test_cases()
                    .map(|level| ModeOptimizerSetting {
                        solc_optimizer_enabled,
                        level,
                    })
                    .collect(),
                (None, None) => optimize_flag_iter.collect(),
            };

        pipeline_iter.flat_map(move |pipeline| {
            optimize_settings
                .clone()
                .into_iter()
                .map(move |optimize_setting| Mode {
                    pipeline,
                    optimize_setting,
                    version: self.version.clone(),
                })
        })
    }

    /// Return a set of [`Mode`]s that correspond to the given [`ParsedMode`]s.
    /// This avoids any duplicate entries.
    pub fn many_to_modes<'a>(
        parsed: impl Iterator<Item = &'a ParsedMode>,
    ) -> impl Iterator<Item = Mode> {
        let modes: HashSet<_> = parsed.flat_map(|p| p.to_modes()).collect();
        modes.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parsed_mode_from_str() {
        let strings = vec![
            ("Mz", "Mz"),
            ("S+", "S+"),
            ("S-", "S-"),
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
            ("Y M0 S+", "Y M0 S+"),
            ("Y M0 S-", "Y M0 S-"),
            ("Y M1 S+", "Y M1 S+"),
            ("Y M1 S-", "Y M1 S-"),
            ("Y M2 S+", "Y M2 S+"),
            ("Y M2 S-", "Y M2 S-"),
            ("Y M3 S+", "Y M3 S+"),
            ("Y M3 S-", "Y M3 S-"),
            ("Y Ms S+", "Y Ms S+"),
            ("Y Ms S-", "Y Ms S-"),
            ("Y Mz S+", "Y Mz S+"),
            ("Y Mz S-", "Y Mz S-"),
            ("E M0 S+", "E M0 S+"),
            ("E M0 S-", "E M0 S-"),
            ("E M1 S+", "E M1 S+"),
            ("E M1 S-", "E M1 S-"),
            ("E M2 S+", "E M2 S+"),
            ("E M2 S-", "E M2 S-"),
            ("E M3 S+", "E M3 S+"),
            ("E M3 S-", "E M3 S-"),
            ("E Ms S+", "E Ms S+"),
            ("E Ms S-", "E Ms S-"),
            ("E Mz S+", "E Mz S+"),
            ("E Mz S-", "E Mz S-"),
            // When stringifying semver again, 0.8.0 becomes ^0.8.0 (same meaning)
            ("Y 0.8.0", "Y ^0.8.0"),
            ("E+ 0.8.0", "E+ ^0.8.0"),
            ("Y M3 S+ >=0.8.0", "Y M3 S+ >=0.8.0"),
            ("E Mz S- <0.7.0", "E Mz S- <0.7.0"),
            // We can parse +- _and_ M1/M2 and S+/S- but the latter ones take priority.
            ("Y+ M1 S+ 0.8.0", "Y+ M1 S+ ^0.8.0"),
            ("E- M2 S- 0.7.0", "E- M2 S- ^0.7.0"),
            // We don't see this in the wild but it is parsed.
            ("<=0.8", "<=0.8"),
        ];

        for (actual, expected) in strings {
            let parsed = ParsedMode::from_str(actual)
                .unwrap_or_else(|_| panic!("Failed to parse mode string '{actual}'"));
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
            ("Mz", vec!["Y Mz S+", "Y Mz S-", "E Mz S+", "E Mz S-"]),
            (
                "S+",
                vec![
                    "Y M0 S+", "Y M3 S+", "Y Mz S+", "E M0 S+", "E M3 S+", "E Mz S+",
                ],
            ),
            (
                "Y",
                vec![
                    "Y M0 S+", "Y M0 S-", "Y M3 S+", "Y M3 S-", "Y Mz S+", "Y Mz S-",
                ],
            ),
            (
                "E",
                vec![
                    "E M0 S+", "E M0 S-", "E M3 S+", "E M3 S-", "E Mz S+", "E Mz S-",
                ],
            ),
            ("Y+", vec!["Y M3 S+"]),
            ("Y-", vec!["Y M0 S-"]),
            (
                "Y <=0.8",
                vec![
                    "Y M0 S+ <=0.8",
                    "Y M0 S- <=0.8",
                    "Y M3 S+ <=0.8",
                    "Y M3 S- <=0.8",
                    "Y Mz S+ <=0.8",
                    "Y Mz S- <=0.8",
                ],
            ),
            (
                "<=0.8",
                vec![
                    "Y M0 S+ <=0.8",
                    "Y M0 S- <=0.8",
                    "Y M3 S+ <=0.8",
                    "Y M3 S- <=0.8",
                    "Y Mz S+ <=0.8",
                    "Y Mz S- <=0.8",
                    "E M0 S+ <=0.8",
                    "E M0 S- <=0.8",
                    "E M3 S+ <=0.8",
                    "E M3 S- <=0.8",
                    "E Mz S+ <=0.8",
                    "E Mz S- <=0.8",
                ],
            ),
            ("Y M3", vec!["Y M3 S+", "Y M3 S-"]),
            ("E M0", vec!["E M0 S+", "E M0 S-"]),
            ("Y M3 S+", vec!["Y M3 S+"]),
            ("E M0 S-", vec!["E M0 S-"]),
            ("Y+ M3 S+", vec!["Y M3 S+"]),
            ("E- M0 S-", vec!["E M0 S-"]),
        ];

        for (actual, expected) in strings {
            let parsed = ParsedMode::from_str(actual)
                .unwrap_or_else(|_| panic!("Failed to parse mode string '{actual}'"));
            let expected_set: HashSet<_> = expected.into_iter().map(|s| s.to_owned()).collect();
            let actual_set: HashSet<_> = parsed.to_modes().map(|m| m.to_string()).collect();

            assert_eq!(
                expected_set, actual_set,
                "Mode string '{actual}' did not expand to '{expected_set:?}': got '{actual_set:?}'"
            );
        }
    }
}
