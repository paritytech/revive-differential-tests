//! This crates provides serializable Rust type definitions for the [solc binary lists][0]
//! and download helpers.
//!
//! [0]: https://binaries.soliditylang.org

pub mod cache;
pub mod download;
pub mod list;
pub mod prelude {
    pub use crate::download_solc;
}

pub(crate) mod internal_prelude {
    pub use revive_dt_common::prelude::*;

    pub use std::collections::{HashMap, HashSet};
    pub use std::fs::{File, create_dir_all};
    pub use std::io::{BufWriter, Write};
    pub use std::path::{Path, PathBuf};
    pub use std::str::FromStr;
    pub use std::sync::{LazyLock, Mutex};

    pub use anyhow::Context as _;
    pub use semver::{Version, VersionReq};
    pub use serde::Deserialize;
    pub use sha2::{Digest, Sha256};
    pub use tokio::sync::Mutex as TokioMutex;

    pub(crate) use crate::cache::get_or_download;
    pub use crate::download::SolcDownloader;
    pub use crate::list::List;
}

use crate::internal_prelude::*;

/// Downloads the solc binary for Wasm is `wasm` is set, otherwise for
/// the target platform.
///
/// Subsequent calls for the same version will use a cached artifact
/// and not download it again.
pub async fn download_solc(
    cache_directory: &Path,
    version: impl Into<VersionOrRequirement> + Send + 'static,
    wasm: bool,
) -> anyhow::Result<(Version, PathBuf)> {
    let downloader = if wasm {
        SolcDownloader::wasm(version).await
    } else if cfg!(target_os = "linux") {
        SolcDownloader::linux(version).await
    } else if cfg!(target_os = "macos") {
        SolcDownloader::macosx(version).await
    } else if cfg!(target_os = "windows") {
        SolcDownloader::windows(version).await
    } else {
        unimplemented!()
    }
    .context("Failed to initialize the Solc Downloader")?;

    get_or_download(cache_directory, &downloader).await
}
