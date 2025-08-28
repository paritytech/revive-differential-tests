use crate::types::VersionOrRequirement;
use semver::Version;
use serde::{Deserialize, Serialize};
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

impl Mode {
    /// Return all of the available mode combinations.
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
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "M0" => Ok(ModeOptimizerSetting::M0),
            "M1" => Ok(ModeOptimizerSetting::M1),
            "M2" => Ok(ModeOptimizerSetting::M2),
            "M3" => Ok(ModeOptimizerSetting::M3),
            "Ms" => Ok(ModeOptimizerSetting::Ms),
            "Mz" => Ok(ModeOptimizerSetting::Mz),
            _ => Err(anyhow::anyhow!(
                "Unsupported optimizer setting '{s}': expected 'M0', 'M1', 'M2', 'M3', 'Ms' or 'Mz'"
            )),
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
