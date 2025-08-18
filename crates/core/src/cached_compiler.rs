//! A wrapper around the compiler which allows for caching of compilation artifacts so that they can
//! be reused between runs.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use futures::FutureExt;
use revive_dt_common::iterators::FilesWithExtensionIterator;
use revive_dt_compiler::{Compiler, CompilerOutput, Mode, SolidityCompiler};
use revive_dt_config::Arguments;
use revive_dt_format::metadata::{ContractIdent, ContractInstance, Metadata};

use alloy::{hex::ToHexExt, json_abi::JsonAbi, primitives::Address};
use anyhow::{Error, Result};
use once_cell::sync::Lazy;
use semver::Version;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tracing::{Instrument, debug, debug_span, instrument};

use crate::Platform;

pub struct CachedCompiler(ArtifactsCache);

impl CachedCompiler {
    pub async fn new(path: impl AsRef<Path>, invalidate_cache: bool) -> Result<Self> {
        let mut cache = ArtifactsCache::new(path);
        if invalidate_cache {
            cache = cache.with_invalidated_cache().await?;
        }
        Ok(Self(cache))
    }

    /// Compiles or gets the compilation artifacts from the cache.
    #[instrument(
        level = "debug",
        skip_all,
        fields(
            metadata_file_path = %metadata_file_path.as_ref().display(),
            %mode,
            platform = P::config_id().to_string()
        ),
        err
    )]
    pub async fn compile_contracts<P: Platform>(
        &self,
        metadata: &Metadata,
        metadata_file_path: impl AsRef<Path>,
        mode: &Mode,
        config: &Arguments,
        deployed_libraries: Option<&HashMap<ContractInstance, (ContractIdent, Address, JsonAbi)>>,
    ) -> Result<(CompilerOutput, Version)> {
        static CACHE_KEY_LOCK: Lazy<RwLock<HashMap<CacheKey, Arc<Mutex<()>>>>> =
            Lazy::new(Default::default);

        let compiler_version_or_requirement = mode.compiler_version_to_use(config.solc.clone());
        let compiler_path = <P::Compiler as SolidityCompiler>::get_compiler_executable(
            config,
            compiler_version_or_requirement,
        )
        .await?;
        let compiler_version = <P::Compiler as SolidityCompiler>::new(compiler_path.clone())
            .version()
            .await?;

        let cache_key = CacheKey {
            platform_key: P::config_id().to_string(),
            compiler_version: compiler_version.clone(),
            metadata_file_path: metadata_file_path.as_ref().to_path_buf(),
            solc_mode: mode.clone(),
        };

        let compilation_callback = || {
            async move {
                compile_contracts::<P>(
                    metadata.directory()?,
                    compiler_path,
                    metadata.files_to_compile()?,
                    mode,
                    deployed_libraries,
                )
                .map(|compilation_result| compilation_result.map(CacheValue::new))
                .await
            }
            .instrument(debug_span!(
                "Running compilation for the cache key",
                cache_key.platform_key = %cache_key.platform_key,
                cache_key.compiler_version = %cache_key.compiler_version,
                cache_key.metadata_file_path = %cache_key.metadata_file_path.display(),
                cache_key.solc_mode = %cache_key.solc_mode,
            ))
        };

        let compiled_contracts = match deployed_libraries {
            // If deployed libraries have been specified then we will re-compile the contract as it
            // means that linking is required in this case.
            Some(_) => {
                debug!("Deployed libraries defined, recompilation must take place");
                debug!("Cache miss");
                compilation_callback().await?.compiler_output
            }
            // If no deployed libraries are specified then we can follow the cached flow and attempt
            // to lookup the compilation artifacts in the cache.
            None => {
                debug!("Deployed libraries undefined, attempting to make use of cache");

                // Lock this specific cache key such that we do not get inconsistent state. We want
                // that when multiple cases come in asking for the compilation artifacts then they
                // don't all trigger a compilation if there's a cache miss. Hence, the lock here.
                let read_guard = CACHE_KEY_LOCK.read().await;
                let mutex = match read_guard.get(&cache_key).cloned() {
                    Some(value) => value,
                    None => {
                        drop(read_guard);
                        CACHE_KEY_LOCK
                            .write()
                            .await
                            .entry(cache_key.clone())
                            .or_default()
                            .clone()
                    }
                };
                let _guard = mutex.lock().await;

                self.0
                    .get_or_insert_with(&cache_key, compilation_callback)
                    .await
                    .map(|value| value.compiler_output)?
            }
        };

        Ok((compiled_contracts, compiler_version))
    }
}

async fn compile_contracts<P: Platform>(
    metadata_directory: impl AsRef<Path>,
    compiler_path: impl AsRef<Path>,
    mut files_to_compile: impl Iterator<Item = PathBuf>,
    mode: &Mode,
    deployed_libraries: Option<&HashMap<ContractInstance, (ContractIdent, Address, JsonAbi)>>,
) -> Result<CompilerOutput> {
    let all_sources_in_dir = FilesWithExtensionIterator::new(metadata_directory.as_ref())
        .with_allowed_extension("sol")
        .with_use_cached_fs(true)
        .collect::<Vec<_>>();

    Compiler::<P::Compiler>::new()
        .with_allow_path(metadata_directory)
        // Handling the modes
        .with_optimization(mode.optimize_setting)
        .with_pipeline(mode.pipeline)
        // Adding the contract sources to the compiler.
        .try_then(|compiler| {
            files_to_compile.try_fold(compiler, |compiler, path| compiler.with_source(path))
        })?
        // Adding the deployed libraries to the compiler.
        .then(|compiler| {
            deployed_libraries
                .iter()
                .flat_map(|value| value.iter())
                .map(|(instance, (ident, address, abi))| (instance, ident, address, abi))
                .flat_map(|(_, ident, address, _)| {
                    all_sources_in_dir
                        .iter()
                        .map(move |path| (ident, address, path))
                })
                .fold(compiler, |compiler, (ident, address, path)| {
                    compiler.with_library(path, ident.as_str(), *address)
                })
        })
        .try_build(compiler_path)
        .await
}

struct ArtifactsCache {
    path: PathBuf,
}

impl ArtifactsCache {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    #[instrument(level = "debug", skip_all, err)]
    pub async fn with_invalidated_cache(self) -> Result<Self> {
        cacache::clear(self.path.as_path())
            .await
            .map_err(Into::<Error>::into)?;
        Ok(self)
    }

    #[instrument(level = "debug", skip_all, err)]
    pub async fn insert(&self, key: &CacheKey, value: &CacheValue) -> Result<()> {
        let key = bson::to_vec(key)?;
        let value = bson::to_vec(value)?;
        cacache::write(self.path.as_path(), key.encode_hex(), value).await?;
        Ok(())
    }

    pub async fn get(&self, key: &CacheKey) -> Option<CacheValue> {
        let key = bson::to_vec(key).ok()?;
        let value = cacache::read(self.path.as_path(), key.encode_hex())
            .await
            .ok()?;
        let value = bson::from_slice::<CacheValue>(&value).ok()?;
        Some(value)
    }

    #[instrument(level = "debug", skip_all, err)]
    pub async fn get_or_insert_with(
        &self,
        key: &CacheKey,
        callback: impl AsyncFnOnce() -> Result<CacheValue>,
    ) -> Result<CacheValue> {
        match self.get(key).await {
            Some(value) => {
                debug!("Cache hit");
                Ok(value)
            }
            None => {
                debug!("Cache miss");
                let value = callback().await?;
                self.insert(key, &value).await?;
                Ok(value)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct CacheKey {
    /// The platform name that this artifact was compiled for. For example, this could be EVM or
    /// PVM.
    platform_key: String,

    /// The version of the compiler that was used to compile the artifacts.
    compiler_version: Version,

    /// The path of the metadata file that the compilation artifacts are for.
    metadata_file_path: PathBuf,

    /// The mode that the compilation artifacts where compiled with.
    solc_mode: Mode,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CacheValue {
    /// The compiler output from the compilation run.
    compiler_output: CompilerOutput,
}

impl CacheValue {
    pub fn new(compiler_output: CompilerOutput) -> Self {
        Self { compiler_output }
    }
}
