//! This crate provides compiler helpers for all supported Solidity targets:
//! - Ethereum solc compiler
//! - Polkadot revive compiler

use std::{fs::read_to_string, path::Path};

use revive_common::EVMVersion;
use revive_solc_json_interface::{
    SolcStandardJsonInput, SolcStandardJsonInputLanguage, SolcStandardJsonInputSettings,
    SolcStandardJsonInputSettingsOptimizer, SolcStandardJsonInputSettingsSelection,
    SolcStandardJsonOutput,
};
use semver::Version;

pub mod revive_js;
pub mod revive_resolc;
pub mod solc;

/// A common interface for all supported Solidity compilers.
pub trait SolidityCompiler {
    /// Extra options specific to the compiler.
    type Options: Sized;

    fn build(
        &self,
        input: &SolcStandardJsonInput,
        extra_options: &Option<Self::Options>,
    ) -> anyhow::Result<SolcStandardJsonOutput>;

    fn new(solc_version: &Version) -> Self;
}

/// A generic builder style interface for configuring all compiler options.
pub struct Compiler<T: SolidityCompiler> {
    input: SolcStandardJsonInput,
    extra_options: Option<T::Options>,
    solc_version: Version,
    allow_paths: Vec<String>,
    base_path: Option<String>,
}

impl<T> Compiler<T>
where
    T: SolidityCompiler,
{
    pub fn new(solc_version: Version) -> Self {
        Self {
            input: SolcStandardJsonInput {
                language: SolcStandardJsonInputLanguage::Solidity,
                sources: Default::default(),
                settings: SolcStandardJsonInputSettings::new(
                    None,
                    Default::default(),
                    None,
                    SolcStandardJsonInputSettingsSelection::new_required(),
                    SolcStandardJsonInputSettingsOptimizer::new(
                        false,
                        None,
                        &Version::new(0, 0, 0),
                        false,
                    ),
                    None,
                ),
            },
            extra_options: None,
            solc_version,
            allow_paths: Default::default(),
            base_path: None,
        }
    }

    pub fn solc_optimizer(mut self, enabled: bool) -> Self {
        self.input.settings.optimizer.enabled = enabled;
        self
    }

    pub fn with_source(mut self, path: &Path) -> anyhow::Result<Self> {
        self.input
            .sources
            .insert(path.display().to_string(), read_to_string(path)?.into());
        Ok(self)
    }

    pub fn evm_version(mut self, evm_version: EVMVersion) -> Self {
        self.input.settings.evm_version = Some(evm_version);
        self
    }

    pub fn solc_version(mut self, solc_version: Version) -> Self {
        self.solc_version = solc_version;
        self
    }

    pub fn extra_options(mut self, extra_options: T::Options) -> Self {
        self.extra_options = Some(extra_options);
        self
    }

    pub fn allow_path(mut self, path: String) -> Self {
        self.allow_paths.push(path);
        self
    }

    pub fn base_path(mut self, base_path: String) -> Self {
        self.base_path = Some(base_path);
        self
    }

    pub fn try_build(&self) -> anyhow::Result<SolcStandardJsonOutput> {
        let compiler = T::new(&self.solc_version);
        compiler.build(&self.input, &self.extra_options)
    }
}
