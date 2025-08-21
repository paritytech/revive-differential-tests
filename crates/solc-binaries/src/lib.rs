//! This crates provides serializable Rust type definitions for the [solc binary lists][0]
//! and download helpers.
//!
//! [0]: https://binaries.soliditylang.org

use std::path::{Path, PathBuf};

use cache::get_or_download;
use download::SolcDownloader;
use semver::Version;

use revive_dt_common::types::VersionOrRequirement;

pub mod cache;
pub mod download;
pub mod list;

/// Return a [`SolcDownloader`] which can be used to download a `solc`
/// binary or return the resolved version it will download.
async fn downloader(
    version: impl Into<VersionOrRequirement>,
    wasm: bool,
) -> anyhow::Result<SolcDownloader> {
    if wasm {
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
}

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
    let downloader = downloader(version, wasm).await?;
    get_or_download(cache_directory, &downloader).await
}

/// Return the version of solc that will be downloaded via [`download_solc`]
/// given the version requirements and wasm flag.
pub async fn solc_version(
    version: impl Into<VersionOrRequirement>,
    wasm: bool,
) -> anyhow::Result<Version> {
    let downloader = downloader(version, wasm).await?;
    Ok(downloader.version.clone())
}
