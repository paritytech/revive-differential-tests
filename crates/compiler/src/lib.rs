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
    pub use crate::{
        Compiler, CompilerInput, CompilerOutput, Mode, ModeOptimizerLevel, ModeOptimizerSetting,
        ModePipeline, RevertString, SolidityCompiler,
        revive_resolc::{Resolc, ResolcRuntimeTarget},
        solc::{Solc, SolcRuntimeTarget},
    };
}

pub(crate) mod internal_prelude {
    pub use crate::{
        is_same_major_minor_patch, prelude::*, resolve_output_source_path, sha256_file_hex,
    };
    pub use revive_dt_config::prelude::*;

    pub use std::{
        collections::{BTreeSet, HashMap},
        hash::Hash,
        path::{Path, PathBuf},
        process::Stdio,
        sync::{Arc, LazyLock},
    };

    pub use alloy::{json_abi::JsonAbi, primitives::Address};
    pub use anyhow::{Context as _, Result};
    pub use dashmap::DashMap;
    pub use foundry_compilers_artifacts::{
        output_selection::{
            BytecodeOutputSelection, ContractOutputSelection, EvmOutputSelection, OutputSelection,
        },
        solc::{
            BytecodeObject, CompilerOutput as SolcOutput, DebuggingSettings, Libraries, Optimizer,
            RevertStrings, Settings, SolcInput, SolcLanguage, Source, Sources,
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
    pub use sha2::{Digest, Sha256};
    pub use tokio::{io::AsyncWriteExt, process::Command as AsyncCommand, sync::OnceCell};
    pub use tracing::{Span, field::display, info};

    pub use revive_common::EVMVersion;
    pub use revive_dt_common::{
        cached_fs::read_to_string, fs::normalize_path, futures::StaticFuture,
        types::VersionOrRequirement,
    };
    pub use revive_dt_solc_binaries::download_solc;
}

pub mod revive_resolc;
pub mod solc;

/// A common interface for all supported Solidity compilers.
pub trait SolidityCompiler {
    /// Returns the version of the compiler.
    fn version(&self) -> &Version;

    /// Returns the Solidity frontend version used by the compiler.
    fn frontend_version(&self) -> &Version;

    /// Returns the path of the compiler executable.
    fn path(&self) -> &Path;

    /// A hex-encoded sha256 fingerprint of the compiler that uniquely identifies the binary and
    /// any compile-time settings baked into its output (e.g. PVM heap/stack sizes for resolc).
    /// Used as part of the compilation cache key so that swapping binaries or tweaking settings
    /// invalidates cached artifacts without relying on the version string.
    fn fingerprint(&self) -> &str;

    /// The low-level compiler interface.
    fn build(&self, input: CompilerInput) -> StaticFuture<Result<CompilerOutput>>;

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

/// Resolves an executable path. If `path` already includes a directory component (relative or
/// absolute), it is returned as-is. Otherwise, `PATH` is searched for an executable file matching
/// the bare name.
pub fn resolve_executable_path(path: &Path) -> Result<PathBuf> {
    if path.parent().is_some_and(|p| !p.as_os_str().is_empty()) {
        return Ok(path.to_path_buf());
    }
    let path_env = std::env::var_os("PATH").with_context(|| {
        format!(
            "Cannot resolve bare executable {}: PATH not set",
            path.display()
        )
    })?;
    std::env::split_paths(&path_env)
        .map(|dir| dir.join(path))
        .find(|candidate| candidate.is_file())
        .with_context(|| format!("Executable {} not found on PATH", path.display()))
}

/// Reads the file at `path` (resolving bare names against `PATH`) and returns the sha256 digest
/// of its contents as a lowercase hex string. Results are memoized per canonical path for the
/// lifetime of the process — `Resolc::new` is called once per (test × mode) tuple, so a naïve
/// implementation would rehash multi-MB binaries tens of thousands of times per run.
pub async fn sha256_file_hex(path: &Path) -> Result<String> {
    static FILE_HASH_CACHE: LazyLock<dashmap::DashMap<PathBuf, String>> =
        LazyLock::new(Default::default);

    let resolved = resolve_executable_path(path)?;
    let canonical = std::fs::canonicalize(&resolved)
        .with_context(|| format!("Failed to canonicalize {} for hashing", resolved.display()))?;

    if let Some(cached) = FILE_HASH_CACHE.get(&canonical) {
        return Ok(cached.clone());
    }

    let canonical_for_blocking = canonical.clone();
    let hash = tokio::task::spawn_blocking(move || -> Result<String> {
        let bytes = std::fs::read(&canonical_for_blocking).with_context(|| {
            format!(
                "Failed to read {} for hashing",
                canonical_for_blocking.display()
            )
        })?;
        Ok(hex::encode(Sha256::digest(&bytes)))
    })
    .await
    .context("sha256_file_hex blocking task panicked")??;

    FILE_HASH_CACHE.insert(canonical, hash.clone());
    Ok(hash)
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

/// Checks whether two versions have the same major, minor, and patch,
/// ignoring any existing build metadata and pre-release.
pub fn is_same_major_minor_patch(a: &Version, b: &Version) -> bool {
    // Note: Do not use `a == b` since that also compares build metadata.
    (a.major, a.minor, a.patch) == (b.major, b.minor, b.patch)
}
