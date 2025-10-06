//! This crate provides compiler helpers for all supported Solidity targets:
//! - Ethereum solc compiler
//! - Polkadot revive resolc compiler
//! - Polkadot revive Wasm compiler

use std::{
    collections::HashMap,
    hash::Hash,
    path::{Path, PathBuf},
    pin::Pin,
};

use alloy::json_abi::JsonAbi;
use alloy::primitives::Address;
use anyhow::{Context as _, Result};
use semver::Version;
use serde::{Deserialize, Serialize};

use revive_common::EVMVersion;
use revive_dt_common::cached_fs::read_to_string;

// Re-export this as it's a part of the compiler interface.
pub use revive_dt_common::types::{Mode, ModeOptimizerSetting, ModePipeline};

pub mod revive_js;
pub mod revive_resolc;
pub mod solc;

/// A common interface for all supported Solidity compilers.
pub trait SolidityCompiler {
    /// Returns the version of the compiler.
    fn version(&self) -> &Version;

    /// Returns the path of the compiler executable.
    fn path(&self) -> &Path;

    /// The low-level compiler interface.
    fn build(
        &self,
        input: CompilerInput,
    ) -> Pin<Box<dyn Future<Output = Result<CompilerOutput>> + '_>>;

    /// Does the compiler support the provided mode and version settings.
    fn supports_mode(
        &self,
        optimizer_setting: ModeOptimizerSetting,
        pipeline: ModePipeline,
    ) -> bool;
}

/// The generic compilation input configuration.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CompilerInput {
    pub pipeline: Option<ModePipeline>,
    pub optimization: Option<ModeOptimizerSetting>,
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
    /// The compiled contracts. The bytecode of the contract is kept as a string in case linking is
    /// required and the compiled source has placeholders.
    pub contracts: HashMap<PathBuf, HashMap<String, (String, JsonAbi)>>,
}

/// A generic builder style interface for configuring the supported compiler options.
#[derive(Default)]
pub struct Compiler {
    input: CompilerInput,
}

impl Compiler {
    pub fn new() -> Self {
        Self {
            input: CompilerInput {
                pipeline: Default::default(),
                optimization: Default::default(),
                evm_version: Default::default(),
                allow_paths: Default::default(),
                base_path: Default::default(),
                sources: Default::default(),
                libraries: Default::default(),
                revert_string_handling: Default::default(),
            },
        }
    }

    pub fn with_optimization(mut self, value: impl Into<Option<ModeOptimizerSetting>>) -> Self {
        self.input.optimization = value.into();
        self
    }

    pub fn with_pipeline(mut self, value: impl Into<Option<ModePipeline>>) -> Self {
        self.input.pipeline = value.into();
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

    pub fn with_source(mut self, path: impl AsRef<Path>) -> Result<Self> {
        self.input.sources.insert(
            path.as_ref().to_path_buf(),
            read_to_string(path.as_ref()).context("Failed to read the contract source")?,
        );
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

    pub fn then(self, callback: impl FnOnce(Self) -> Self) -> Self {
        callback(self)
    }

    pub fn try_then<E>(self, callback: impl FnOnce(Self) -> Result<Self, E>) -> Result<Self, E> {
        callback(self)
    }

    pub async fn try_build(self, compiler: &dyn SolidityCompiler) -> Result<CompilerOutput> {
        compiler.build(self.input).await
    }

    pub fn input(&self) -> &CompilerInput {
        &self.input
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
