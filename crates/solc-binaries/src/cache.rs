//! Helper for caching the solc binaries.

use std::{
    collections::HashSet,
    fs::{File, create_dir_all},
    io::{BufWriter, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use tokio::sync::Mutex;

use crate::download::SolcDownloader;

pub const SOLC_CACHE_DIRECTORY: &str = "solc";
pub(crate) static SOLC_CACHER: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Default::default);

pub(crate) async fn get_or_download(
    working_directory: &Path,
    downloader: &SolcDownloader,
) -> anyhow::Result<PathBuf> {
    let target_directory = working_directory
        .join(SOLC_CACHE_DIRECTORY)
        .join(downloader.version.to_string());
    let target_file = target_directory.join(downloader.target);

    let mut cache = SOLC_CACHER.lock().await;
    if cache.contains(&target_file) {
        tracing::debug!("using cached solc: {}", target_file.display());
        return Ok(target_file);
    }

    create_dir_all(target_directory)?;
    download_to_file(&target_file, downloader).await?;
    cache.insert(target_file.clone());

    Ok(target_file)
}

async fn download_to_file(path: &Path, downloader: &SolcDownloader) -> anyhow::Result<()> {
    tracing::info!("caching file: {}", path.display());

    let Ok(file) = File::create_new(path) else {
        tracing::debug!("cache file already exists: {}", path.display());
        return Ok(());
    };

    #[cfg(unix)]
    {
        let mut permissions = file.metadata()?.permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        file.set_permissions(permissions)?;
    }

    let mut file = BufWriter::new(file);
    file.write_all(&downloader.download().await?)?;
    file.flush()?;
    drop(file);

    #[cfg(target_os = "macos")]
    std::process::Command::new("xattr")
        .arg("-d")
        .arg("com.apple.quarantine")
        .arg(path)
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()?
        .wait()?;

    Ok(())
}
