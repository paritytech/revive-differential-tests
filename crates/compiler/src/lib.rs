//! This crate provides compiler helpers for all supported Solidity targets:
//! - Ethereum solc compiler
//! - Polkadot revive resolc compiler
//! - Polkadot revive Wasm compiler

use std::{
    collections::HashMap,
    fs::read_to_string,
    hash::Hash,
    path::{Path, PathBuf},
};

use alloy::json_abi::JsonAbi;
use alloy_primitives::Address;
use semver::Version;
use serde::{Deserialize, Serialize};

use revive_common::EVMVersion;
use revive_dt_common::types::VersionOrRequirement;
use revive_dt_config::Arguments;

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
        input: CompilerInput,
        additional_options: Self::Options,
    ) -> impl Future<Output = anyhow::Result<CompilerOutput>>;

    fn new(solc_executable: PathBuf) -> Self;

    fn get_compiler_executable(
        config: &Arguments,
        version: impl Into<VersionOrRequirement>,
    ) -> impl Future<Output = anyhow::Result<PathBuf>>;

    fn version(&self) -> anyhow::Result<Version>;
}

/// The generic compilation input configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompilerInput {
    pub enable_optimization: Option<bool>,
    pub via_ir: Option<bool>,
    pub evm_version: Option<EVMVersion>,
    pub allow_paths: Vec<PathBuf>,
    pub base_path: Option<PathBuf>,
    pub sources: HashMap<PathBuf, String>,
    pub libraries: HashMap<PathBuf, HashMap<String, Address>>,
    pub revert_string_handling: Option<RevertString>,
}

/// The generic compilation output configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompilerOutput {
    /// The compiled contracts. The bytecode of the contract is kept as a string incase linking is
    /// required and the compiled source has placeholders.
    pub contracts: HashMap<PathBuf, HashMap<String, (String, JsonAbi)>>,
}

/// A generic builder style interface for configuring the supported compiler options.
pub struct Compiler<T: SolidityCompiler> {
    input: CompilerInput,
    additional_options: T::Options,
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
            input: CompilerInput {
                enable_optimization: Default::default(),
                via_ir: Default::default(),
                evm_version: Default::default(),
                allow_paths: Default::default(),
                base_path: Default::default(),
                sources: Default::default(),
                libraries: Default::default(),
                revert_string_handling: Default::default(),
            },
            additional_options: T::Options::default(),
        }
    }

    pub fn with_optimization(mut self, value: impl Into<Option<bool>>) -> Self {
        self.input.enable_optimization = value.into();
        self
    }

    pub fn with_via_ir(mut self, value: impl Into<Option<bool>>) -> Self {
        self.input.via_ir = value.into();
        self
    }

    pub fn with_evm_version(mut self, version: impl Into<Option<EVMVersion>>) -> Self {
        self.input.evm_version = version.into();
        self
    }

    pub fn with_allow_path(mut self, path: impl AsRef<Path>) -> Self {
        self.input.allow_paths.push(path.as_ref().into());
        self
    }

    pub fn with_base_path(mut self, path: impl Into<Option<PathBuf>>) -> Self {
        self.input.base_path = path.into();
        self
    }

    pub fn with_source(mut self, path: impl AsRef<Path>) -> anyhow::Result<Self> {
        self.input
            .sources
            .insert(path.as_ref().to_path_buf(), read_to_string(path.as_ref())?);
        Ok(self)
    }

    pub fn with_library(
        mut self,
        path: impl AsRef<Path>,
        name: impl AsRef<str>,
        address: Address,
    ) -> Self {
        self.input
            .libraries
            .entry(path.as_ref().to_path_buf())
            .or_default()
            .insert(name.as_ref().into(), address);
        self
    }

    pub fn with_revert_string_handling(
        mut self,
        revert_string_handling: impl Into<Option<RevertString>>,
    ) -> Self {
        self.input.revert_string_handling = revert_string_handling.into();
        self
    }

    pub fn with_additional_options(mut self, options: impl Into<T::Options>) -> Self {
        self.additional_options = options.into();
        self
    }

    pub async fn try_build(
        self,
        compiler_path: impl AsRef<Path>,
    ) -> anyhow::Result<CompilerOutput> {
        T::new(compiler_path.as_ref().to_path_buf())
            .build(self.input, self.additional_options)
            .await
    }

    pub fn input(&self) -> CompilerInput {
        self.input.clone()
    }
}

/// Defines how the compiler should handle revert strings.
#[derive(
    Clone, Debug, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub enum RevertString {
    #[default]
    Default,
    Debug,
    Strip,
    VerboseDebug,
}
