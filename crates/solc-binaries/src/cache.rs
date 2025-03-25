//! Helper for caching the solc binaries.

use std::{
    cell::OnceCell,
    collections::HashSet,
    fs::{File, create_dir_all},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
};

use crate::download::GHDownloader;

pub const SOLC_CACHE_DIRECTORY: &str = "solc";
pub const SOLC_CACHER: OnceCell<Mutex<SolcCacher>> = OnceCell::new();

pub fn get_or_download(
    working_directory: &Path,
    downloader: &GHDownloader,
) -> anyhow::Result<PathBuf> {
    SOLC_CACHER
        .get_or_init(|| {
            Mutex::new(SolcCacher::new(
                working_directory.join(SOLC_CACHE_DIRECTORY),
            ))
        })
        .lock()
        .unwrap()
        .get_or_download(downloader)
}

pub struct SolcCacher {
    cache_directory: PathBuf,
    cached_binaries: HashSet<PathBuf>,
}

impl SolcCacher {
    fn new(cache_directory: PathBuf) -> Self {
        Self {
            cache_directory,
            cached_binaries: Default::default(),
        }
    }

    fn get_or_download(&mut self, downloader: &GHDownloader) -> anyhow::Result<PathBuf> {
        let directory = self.cache_directory.join(downloader.version.to_string());
        let file_path = directory.join(downloader.target);

        if self.cached_binaries.contains(&file_path) {
            return Ok(file_path);
        }

        create_dir_all(directory)?;

        let Ok(mut file) = File::create_new(&file_path) else {
            self.cached_binaries.insert(file_path.clone());
            return Ok(file_path);
        };

        file.write_all(&downloader.download()?)?;

        #[cfg(unix)]
        {
            let mut permissions = file.metadata()?.permissions();
            let mode = permissions.mode() | 0o111;
            permissions.set_mode(mode);
            file.set_permissions(permissions)?;
        }

        #[cfg(target_os = "macos")]
        std::process::Command::new("xattr")
            .arg("-d")
            .arg("com.apple.quarantine")
            .arg(&file_path)
            .stderr(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .spawn()?
            .wait()?;

        Ok(file_path)
    }
}
