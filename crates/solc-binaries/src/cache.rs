//! Helper for caching the solc binaries.

use std::{
    collections::HashSet,
    fs::{File, create_dir_all},
    io::{BufWriter, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use semver::Version;
use tokio::sync::Mutex;

use crate::download::SolcDownloader;
use anyhow::Context as _;

pub const SOLC_CACHE_DIRECTORY: &str = "solc";
pub(crate) static SOLC_CACHER: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Default::default);

pub(crate) async fn get_or_download(
    working_directory: &Path,
    downloader: &SolcDownloader,
) -> anyhow::Result<(Version, PathBuf)> {
    let target_directory =
        working_directory.join(SOLC_CACHE_DIRECTORY).join(downloader.version.to_string());
    let target_file = target_directory.join(downloader.target);

    let mut cache = SOLC_CACHER.lock().await;
    if cache.contains(&target_file) {
        tracing::debug!("using cached solc: {}", target_file.display());
        return Ok((downloader.version.clone(), target_file));
    }

    create_dir_all(&target_directory).with_context(|| {
        format!("Failed to create solc cache directory: {}", target_directory.display())
    })?;
    download_to_file(&target_file, downloader)
        .await
        .with_context(|| format!("Failed to write downloaded solc to {}", target_file.display()))?;
    cache.insert(target_file.clone());

    Ok((downloader.version.clone(), target_file))
}

async fn download_to_file(path: &Path, downloader: &SolcDownloader) -> anyhow::Result<()> {
    let Ok(file) = File::create_new(path) else {
        return Ok(());
    };

    #[cfg(unix)]
    {
        let mut permissions = file
            .metadata()
            .with_context(|| format!("Failed to read metadata for {}", path.display()))?
            .permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        file.set_permissions(permissions).with_context(|| {
            format!("Failed to set executable permissions on {}", path.display())
        })?;
    }

    let mut file = BufWriter::new(file);
    file.write_all(&downloader.download().await.context("Failed to download solc binary bytes")?)
        .with_context(|| format!("Failed to write solc binary to {}", path.display()))?;
    file.flush().with_context(|| format!("Failed to flush file {}", path.display()))?;
    drop(file);

    #[cfg(target_os = "macos")]
    std::process::Command::new("xattr")
        .arg("-d")
        .arg("com.apple.quarantine")
        .arg(path)
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()
        .with_context(|| {
            format!("Failed to spawn xattr to remove quarantine attribute on {}", path.display())
        })?
        .wait()
        .with_context(|| {
            format!("Failed waiting for xattr operation to complete on {}", path.display())
        })?;

    Ok(())
}
