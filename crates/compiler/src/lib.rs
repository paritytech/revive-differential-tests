//! This crate provides compiler helpers for all supported Solidity targets:
//! - Ethereum solc compiler
//! - Polkadot revive resolc compiler
//! - Polkadot revive Wasm compiler

use std::{fs::read_to_string, hash::Hash, path::Path};

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
    type Options: Default + PartialEq + Eq + Hash;

    /// The low-level compiler interface.
    fn build(
        &self,
        input: CompilerInput<Self::Options>,
    ) -> anyhow::Result<CompilerOutput<Self::Options>>;

    fn new(solc_version: &Version) -> Self;
}

/// The generic compilation input configuration.
#[derive(Debug)]
pub struct CompilerInput<T: PartialEq + Eq + Hash> {
    pub extra_options: T,
    pub input: SolcStandardJsonInput,
}

/// The generic compilation output configuration.
pub struct CompilerOutput<T: PartialEq + Eq + Hash> {
    pub input: CompilerInput<T>,
    pub output: SolcStandardJsonOutput,
}

impl<T> PartialEq for CompilerInput<T>
where
    T: PartialEq + Eq + Hash,
{
    fn eq(&self, other: &Self) -> bool {
        let self_input = serde_json::to_vec(&self.input).unwrap_or_default();
        let other_input = serde_json::to_vec(&self.input).unwrap_or_default();
        self.extra_options.eq(&other.extra_options) && self_input == other_input
    }
}

impl<T> Eq for CompilerInput<T> where T: PartialEq + Eq + Hash {}

impl<T> Hash for CompilerInput<T>
where
    T: PartialEq + Eq + Hash,
{
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.extra_options.hash(state);
        state.write(&serde_json::to_vec(&self.input).unwrap_or_default());
    }
}

/// A generic builder style interface for configuring all compiler options.
pub struct Compiler<T: SolidityCompiler> {
    input: SolcStandardJsonInput,
    extra_options: T::Options,
    allow_paths: Vec<String>,
    base_path: Option<String>,
}

impl Default for Compiler<solc::Solc> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Compiler<T>
where
    T: SolidityCompiler,
{
    pub fn new() -> Self {
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
            extra_options: Default::default(),
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

    pub fn extra_options(mut self, extra_options: T::Options) -> Self {
        self.extra_options = extra_options;
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

    pub fn try_build(self, solc_version: &Version) -> anyhow::Result<CompilerOutput<T::Options>> {
        T::new(solc_version).build(CompilerInput {
            extra_options: self.extra_options,
            input: self.input,
        })
    }
}
