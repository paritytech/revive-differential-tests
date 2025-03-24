//! Helper for caching the solc binaries.

use std::{
    cell::OnceCell,
    collections::HashSet,
    fs::{File, create_dir_all},
    io::Write,
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

        if file_path.exists() {
            self.cached_binaries.insert(file_path.clone());
            return Ok(file_path);
        }

        create_dir_all(directory)?;

        let buf = downloader.download()?;
        File::create_new(&file_path)
            .expect("should not exist because of above early return")
            .write_all(&buf)?;

        Ok(file_path)
    }
}
