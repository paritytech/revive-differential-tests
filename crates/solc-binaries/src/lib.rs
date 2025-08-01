//! This crates provides serializable Rust type definitions for the [solc binary lists][0]
//! and download helpers.
//!
//! [0]: https://binaries.soliditylang.org

use std::path::{Path, PathBuf};

use cache::get_or_download;
use download::GHDownloader;

use revive_dt_common::types::VersionOrRequirement;

pub mod cache;
pub mod download;
pub mod list;

/// Downloads the solc binary for Wasm is `wasm` is set, otherwise for
/// the target platform.
///
/// Subsequent calls for the same version will use a cached artifact
/// and not download it again.
pub async fn download_solc(
    cache_directory: &Path,
    version: impl Into<VersionOrRequirement>,
    wasm: bool,
) -> anyhow::Result<PathBuf> {
    let downloader = if wasm {
        GHDownloader::wasm(version).await
    } else if cfg!(target_os = "linux") {
        GHDownloader::linux(version).await
    } else if cfg!(target_os = "macos") {
        GHDownloader::macosx(version).await
    } else if cfg!(target_os = "windows") {
        GHDownloader::windows(version).await
    } else {
        unimplemented!()
    }?;

    get_or_download(cache_directory, &downloader).await
}
