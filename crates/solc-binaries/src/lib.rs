//! This crates provides serializable Rust type definitions for the [solc binary lists][0]
//! and download helpers.
//!
//! [0]: https://binaries.soliditylang.org

use std::path::{Path, PathBuf};

use cache::get_or_download;
use download::GHDownloader;
use semver::Version;

pub mod cache;
pub mod download;
pub mod list;

/// Downloads the solc binary for Wasm is `wasm` is set, otherwise for
/// the target platform.
///
/// Subsequent calls for the same version will use a cached artifact
/// and not download it again.
pub fn download_solc(
    working_directory: &Path,
    version: Version,
    wasm: bool,
) -> anyhow::Result<PathBuf> {
    let downloader = if wasm {
        GHDownloader::wasm(version)
    } else if cfg!(target_os = "linux") {
        GHDownloader::linux(version)
    } else if cfg!(target_os = "macos") {
        GHDownloader::macosx(version)
    } else if cfg!(target_os = "windows") {
        GHDownloader::windows(version)
    } else {
        unimplemented!()
    };

    get_or_download(working_directory, &downloader)
}
