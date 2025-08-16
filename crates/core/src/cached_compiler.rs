//! A wrapper around the compiler which allows for caching of compilation artifacts so that they can
//! be reused between runs.

use std::path::{Path, PathBuf};

use anyhow::{Error, Result};
use revive_dt_format::mode::SolcMode;
use serde::{Deserialize, Serialize};

struct ArtifactsCache {
    path: PathBuf,
}

impl ArtifactsCache {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn with_invalidated_cache(self) -> Result<Self> {
        cacache::clear_sync(self.path.as_path()).map_err(Into::<Error>::into)?;
        Ok(self)
    }
}

struct CacheKey {
    metadata_file_path: PathBuf,
    solc_mode: SolcMode,
}
