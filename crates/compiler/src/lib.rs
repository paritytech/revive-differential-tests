//! This crate provides compiler helpers for all supported Solidity targets:
//! - Ethereum solc compiler
//! - Polkadot revive resolc compiler
//! - Polkadot revive Wasm compiler

use crate::internal_prelude::*;

// Re-export this as it's a part of the compiler interface.
pub use revive_dt_common::types::{
    Mode, ModeOptimizerLevel, ModeOptimizerSetting, ModePipeline, ParsedMode,
};

pub mod prelude {
    pub use crate::revive_resolc::{Resolc, ResolcKind};
    pub use crate::solc::{Solc, SolcKind};
    pub use crate::{Compiler, CompilerInput, CompilerOutput, RevertString, SolidityCompiler};
    pub use crate::{Mode, ModeOptimizerSetting, ModePipeline};
}

pub(crate) mod internal_prelude {
    pub use crate::prelude::*;
    pub use crate::resolve_output_source_path;
    pub use revive_dt_config::prelude::*;

    pub use std::collections::{BTreeSet, HashMap};
    pub use std::hash::Hash;
    pub use std::path::{Path, PathBuf};
    pub use std::process::Stdio;
    pub use std::sync::{Arc, LazyLock};

    pub use alloy::json_abi::JsonAbi;
    pub use alloy::primitives::Address;
    pub use anyhow::Context as _;
    pub use anyhow::Result;
    pub use dashmap::DashMap;
    pub use foundry_compilers_artifacts::{
        output_selection::{
            BytecodeOutputSelection, ContractOutputSelection, EvmOutputSelection, OutputSelection,
        },
        solc::CompilerOutput as SolcOutput,
        solc::{
            BytecodeObject, DebuggingSettings, Libraries, Optimizer, RevertStrings, Settings,
            SolcInput, SolcLanguage, Source, Sources,
        },
    };
    pub use revive_solc_json_interface::{
        PolkaVMDefaultHeapMemorySize, PolkaVMDefaultStackMemorySize, SolcStandardJsonInput,
        SolcStandardJsonInputLanguage, SolcStandardJsonInputSettings,
        SolcStandardJsonInputSettingsLibraries, SolcStandardJsonInputSettingsMetadata,
        SolcStandardJsonInputSettingsOptimizer, SolcStandardJsonInputSettingsPolkaVM,
        SolcStandardJsonInputSettingsPolkaVMMemory, SolcStandardJsonInputSettingsSelection,
        SolcStandardJsonOutput,
        standard_json::input::settings::optimizer::details::Details as SolcOptimizerDetails,
    };
    pub use semver::Version;
    pub use serde::{Deserialize, Serialize};
    pub use tokio::{io::AsyncWriteExt, process::Command as AsyncCommand};
    pub use tracing::{Span, field::display, info};

    pub use revive_common::EVMVersion;
    pub use revive_dt_common::cached_fs::read_to_string;
    pub use revive_dt_common::fs::normalize_path;
    pub use revive_dt_common::futures::FrameworkFuture;
    pub use revive_dt_common::types::VersionOrRequirement;
    pub use revive_dt_solc_binaries::download_solc;
}

pub mod revive_resolc;
pub mod solc;

/// A common interface for all supported Solidity compilers.
pub trait SolidityCompiler {
    /// Returns the version of the compiler.
    fn version(&self) -> &Version;

    /// Returns the path of the compiler executable.
    fn path(&self) -> &Path;

    /// The low-level compiler interface.
    fn build(&self, input: CompilerInput) -> FrameworkFuture<Result<CompilerOutput>>;

    /// Does the compiler support the provided mode and version settings.
    fn supports_mode(&self, mode: &Mode) -> bool;
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

/// Resolves a compiler output source path to an absolute, canonicalized file system path.
///
/// If `base_path` is `Some` and `path` is relative, joins it with the base before canonicalizing.
/// This is needed if the input source paths have been normalized to relative paths.
pub fn resolve_output_source_path(path: &Path, base_path: Option<&Path>) -> Result<PathBuf> {
    let path_buf = match base_path {
        Some(base) if !path.is_absolute() => base.join(path),
        Some(_) | None => path.to_path_buf(),
    };
    path_buf
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize path {}", path_buf.display()))
}
